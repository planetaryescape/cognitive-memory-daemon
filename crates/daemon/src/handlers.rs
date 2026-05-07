//! Request handlers, dispatcher entry point.
//!
//! One function per `Request` variant we serve. Full v0.1.0 surface —
//! feature-parity with the SDK's `MemoryAdapter` interface plus the
//! paper-faithful StoreBatch with co-creation auto-association.

use cognitive_memory_embeddings::{CachedEmbeddings, EmbeddingError, EmbeddingProvider};
use cognitive_memory_lifecycle::{
    base_decay_rate_for_category, compute_retention, parse_category, stability_from_importance,
    LifecycleConfig, MemoryState,
};
use cognitive_memory_protocol::{
    AffectedData, BatchUpdateArgs, BridgeTokenData, ClearArgs, ConvertToStubArgs, CountsArgs,
    CountsData, DeleteManyMemoryArgs, DeleteMemoryArgs, DiagnosticsRequest, FindFadingArgs,
    FindStableArgs, GetLinkedArgs, GetLinkedManyArgs, GetManyMemoryArgs, GetMemoryArgs,
    LexicalIdsData, LifecycleRequest, LinkMemoryArgs, LinkStrengthData, LinkedMemoriesData,
    LinkedMemoryData, ListMemoryArgs, MarkSupersededArgs, MemoriesData, MemoryData, MemoryRequest,
    MemorySearchResultsData, MemoryStoredBatchData, MemoryStoredData, MigrateToColdArgs,
    MigrateToHotArgs, MintBridgeTokenArgs, Request, Response, ResponseData, SearchHit,
    SearchLexicalArgs, SearchMemoryArgs, StatusData, StoreBatchArgs, StoreMemoryArgs, TickArgs,
    TickResultData, UnlinkMemoryArgs, UpdateMemoryArgs, UpdateRetentionArgs, VectorSearchArgs,
};
use cognitive_memory_search::{ResultSource, SearchError, SearchOptions, Searcher};
use cognitive_memory_store::{
    AssociationRepo, MemoryFilters, MemoryRepo, MemoryRow, MemoryUpdate, Store,
};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tokio::sync::Semaphore;
use tracing::{debug, instrument};

const PROVIDER_NAME: &str = "local";
const CORE_RETENTION_FLOOR: f64 = 0.6;

/// Errors a handler can produce.
#[derive(Debug, thiserror::Error)]
pub enum HandlerError {
    #[error("storage: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("embedding: {0}")]
    Embedding(#[from] EmbeddingError),
    #[error("search: {0}")]
    Search(#[from] SearchError),
    #[error("invalid payload: {0}")]
    InvalidPayload(String),
    #[error("unknown bucket")]
    UnknownBucket,
    #[error("not found")]
    NotFound,
}

pub struct AppState {
    pub store: Store,
    pub embeddings: Arc<dyn EmbeddingProvider>,
    pub request_semaphore: Arc<Semaphore>,
    /// Monotonic clock anchor set when `AppState` is constructed. Used
    /// to compute `StatusData::uptime_seconds`. Monotonic (not wall
    /// clock) so suspend/clock-skew don't produce negative or
    /// nonsensical uptimes.
    pub started_at: Instant,
    /// Optional LLM provider for conflict judging (Stage 4) and
    /// consolidation summarisation (Stage 4). `None` is the
    /// default — the daemon falls back to the heuristic conflict
    /// resolver and skips consolidation. Wired by config:
    ///   `provider = "local"` ⇒ LocalLlmProvider
    ///   `provider = "openai"` ⇒ OpenAiProvider (existing)
    ///   `provider = "anthropic"` ⇒ AnthropicProvider (existing)
    /// See `crates/cli/src/main.rs::download_model` and `set-llm`.
    pub llm: Option<Arc<dyn cognitive_memory_llm::LlmProvider>>,
}

#[instrument(skip(req, state), fields(user_id = %user_id))]
pub async fn handle_request(
    req: Request,
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<Response, HandlerError> {
    match req {
        Request::Diagnostics(DiagnosticsRequest::Status) => handle_status(state).await,
        Request::Diagnostics(DiagnosticsRequest::MintBridgeToken(args)) => {
            handle_mint_bridge_token(args, state).await
        }
        Request::Diagnostics(DiagnosticsRequest::Counts(args)) => {
            handle_counts(args, state, user_id).await
        }
        Request::Memory(mem) => handle_memory_request(mem, state, user_id).await,
        Request::Lifecycle(life) => handle_lifecycle_request(life, state, user_id).await,
        Request::UnknownBucket => Err(HandlerError::UnknownBucket),
    }
}

// =========================================================================
// Diagnostics
// =========================================================================

async fn handle_status(state: &Arc<AppState>) -> Result<Response, HandlerError> {
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM memories")
        .fetch_one(state.store.reader())
        .await?;
    Ok(Response::ok(ResponseData::Status(StatusData {
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        memory_count: count.0 as u64,
    })))
}

async fn handle_counts(
    args: CountsArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let counts = MemoryRepo::new(&state.store)
        .counts_for_user(&args.user_id)
        .await?;
    Ok(Response::ok(ResponseData::Counts(CountsData {
        hot: counts.hot,
        cold: counts.cold,
        stub: counts.stub,
        total: counts.total,
    })))
}

async fn handle_mint_bridge_token(
    args: MintBridgeTokenArgs,
    state: &Arc<AppState>,
) -> Result<Response, HandlerError> {
    use sha2::{Digest, Sha256};

    let raw = format!("cmb_{}{}", ulid::Ulid::new(), ulid::Ulid::new());
    let salt = "cm-daemon-bridge-token-salt";
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(raw.as_bytes());
    let hash = hasher.finalize();
    let hash_hex = hex_lower(&hash);

    let now = unix_now();
    let expires = now + args.ttl_seconds as i64;
    let scope_str = match args.scope {
        cognitive_memory_protocol::BridgeScope::Read => "read",
        cognitive_memory_protocol::BridgeScope::Write => "write",
        cognitive_memory_protocol::BridgeScope::Admin => "admin",
    };
    let value = serde_json::json!({
        "user_id": args.user_id,
        "scope": scope_str,
        "expires_at_unix": expires,
    })
    .to_string();

    sqlx::query("INSERT INTO kv (namespace, key, value) VALUES (?, ?, ?)")
        .bind("bridge_tokens")
        .bind(&hash_hex)
        .bind(&value)
        .execute(state.store.writer())
        .await?;

    Ok(Response::ok(ResponseData::BridgeToken(BridgeTokenData {
        token: raw,
        expires_at_unix: expires,
    })))
}

// =========================================================================
// Memory
// =========================================================================

async fn handle_memory_request(
    req: MemoryRequest,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    match req {
        MemoryRequest::Store(args) => handle_memory_store(args, state, connection_user).await,
        MemoryRequest::StoreBatch(args) => {
            handle_memory_store_batch(args, state, connection_user).await
        }
        MemoryRequest::Search(args) => handle_memory_search(args, state, connection_user).await,
        MemoryRequest::Get(args) => handle_memory_get(args, state, connection_user).await,
        MemoryRequest::GetMany(args) => handle_memory_get_many(args, state, connection_user).await,
        MemoryRequest::List(args) => handle_memory_list(args, state, connection_user).await,
        MemoryRequest::Update(args) => handle_memory_update(args, state, connection_user).await,
        MemoryRequest::Delete(args) => handle_memory_delete(args, state, connection_user).await,
        MemoryRequest::DeleteMany(args) => {
            handle_memory_delete_many(args, state, connection_user).await
        }
        MemoryRequest::Link(args) => handle_memory_link(args, state, connection_user).await,
        MemoryRequest::Unlink(args) => handle_memory_unlink(args, state, connection_user).await,
        MemoryRequest::GetLinked(args) => {
            handle_memory_get_linked(args, state, connection_user).await
        }
        MemoryRequest::GetLinkedMany(args) => {
            handle_memory_get_linked_many(args, state, connection_user).await
        }
        MemoryRequest::VectorSearch(args) => {
            handle_vector_search(args, state, connection_user).await
        }
        MemoryRequest::SearchLexical(args) => {
            handle_search_lexical(args, state, connection_user).await
        }
        MemoryRequest::BatchUpdate(args) => handle_batch_update(args, state, connection_user).await,
    }
}

async fn handle_memory_store(
    args: StoreMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;

    let cached = CachedEmbeddings::new(ProviderRef(state.embeddings.clone()), &state.store);
    let vector = cached.embed(&args.content).await?;

    let id = format!("mem_{}", ulid::Ulid::new());
    let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
    let now = unix_now();

    let mut row = MemoryRow::new_minimal(
        id.clone(),
        args.user_id.clone(),
        args.content,
        args.category.clone(),
        args.memory_type,
        now,
    );
    row.embedding = Some(bytes);
    row.embedding_provider = Some(state.embeddings.name().to_string());
    row.embedding_model = Some(state.embeddings.model().to_string());
    row.metadata = args.metadata;
    if let Some(imp) = args.importance {
        row.importance = imp.clamp(0.0, 1.0);
        // Recompute stability from the new importance so the SDK
        // invariant `stability = 0.1 + 0.3 * importance` holds at
        // creation regardless of construction order.
        row.stability = stability_from_importance(row.importance);
    }
    // Synaptic tagging (paper §3.4): category=core triggers protected
    // retention floor at encoding time.
    if args.category == "core" {
        row.retention_floor = CORE_RETENTION_FLOOR;
    }

    MemoryRepo::new(&state.store).insert(&row).await?;

    // Deferred conflict detection (paper §3.7, core.py:215-220):
    // search for similar existing memories above the threshold and
    // queue the pair. Resolved at the next tick. Uses the just-
    // computed query vector and a small candidate radius.
    queue_conflicts_for(state, &args.user_id, &id, &vector, now).await?;

    debug!(memory_id = %id, "memory stored");
    Ok(Response::ok(ResponseData::MemoryStored(MemoryStoredData {
        id,
    })))
}

/// Cosine threshold above which two memories are treated as
/// conflict candidates (core.py:38: CONFLICT_SIMILARITY_THRESHOLD).
const CONFLICT_SIMILARITY_THRESHOLD: f64 = 0.85;

/// Cosine threshold above which a newly-ingested near-duplicate
/// reinforces the existing memory's stability by +0.05.
/// Mirrors STABILITY_REINFORCEMENT_THRESHOLD in core.py:39.
const STABILITY_REINFORCEMENT_THRESHOLD: f64 = 0.75;

/// Stability bump applied during the reinforcement-band branch.
/// SDK core.py:223 — `stability + 0.05`.
const STABILITY_REINFORCEMENT_AMOUNT: f64 = 0.05;

/// Bump applied to every association edge between co-retrieved
/// memories (paper Eq 9). SDK default `association_strengthen_amount = 0.1`.
const ASSOCIATION_STRENGTHEN_AMOUNT: f64 = 0.1;

/// Cosine threshold above which a co-ingested memory is auto-linked
/// to existing memories. Mirrors INGESTION_ASSOCIATION_THRESHOLD
/// in cognitive_memory/core.py:40 (= 0.4).
const SYNAPTIC_TAG_THRESHOLD: f64 = 0.4;

/// Base weight for synaptic-tag links. Mirrors
/// INGESTION_ASSOCIATION_BASE_WEIGHT in core.py:41 (= 0.2).
const SYNAPTIC_TAG_BASE_WEIGHT: f64 = 0.2;

/// Compute synaptic-tag weight from cosine similarity per SDK
/// core.py:255 — `min(0.5, 0.2 + (sim - 0.4) * 0.5)`.
fn synaptic_tag_weight(sim: f64) -> f64 {
    (SYNAPTIC_TAG_BASE_WEIGHT + (sim - SYNAPTIC_TAG_THRESHOLD) * 0.5).min(0.5)
}

/// Detect similar memories for a freshly-stored memory by running a
/// vector search and dispatching by similarity band:
///   - sim ≥ 0.85 ⇒ insert into `conflict_queue` for tick to resolve
///   - 0.75 ≤ sim < 0.85 ⇒ reinforce existing memory's stability +0.05
///   - 0.40 ≤ sim < 0.75 ⇒ synaptic-tag (bidirectional auto-link)
///   - sim < 0.40 ⇒ no action
///
/// Mirrors the dispatch logic in cognitive_memory/core.py:215-262.
async fn queue_conflicts_for(
    state: &Arc<AppState>,
    user_id: &str,
    new_id: &str,
    new_vec: &[f32],
    now: i64,
) -> Result<(), HandlerError> {
    let opts = SearchOptions {
        limit: 5,
        deep_recall: false,
        provider: state.embeddings.name().to_string(),
        model: state.embeddings.model().to_string(),
        now,
        hybrid: false,
        query_text: None,
        graph_expansion_hops: 0,
        min_bridge_edge_weight: 0.3,
        bridge_discovery: false,
        max_bridge_paths: 3,
    };
    let hits = Searcher::new(&state.store)
        .search(user_id, new_vec, &opts)
        .await?;

    let mut tx = state.store.writer().begin().await?;
    for hit in hits {
        if hit.memory_id == new_id {
            continue;
        }
        // Composite score = cosine * R^α. Fresh memories have R≈1.0
        // so score≈cosine; the order is preserved either way. Bands
        // are exclusive; results sorted desc, so once we drop below
        // the lowest band we can stop.
        let sim = hit.score as f64;
        if sim >= CONFLICT_SIMILARITY_THRESHOLD {
            sqlx::query(
                "INSERT OR IGNORE INTO conflict_queue
                 (user_id, new_memory_id, existing_memory_id, similarity, queued_at)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(user_id)
            .bind(new_id)
            .bind(&hit.memory_id)
            .bind(sim)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        } else if sim >= STABILITY_REINFORCEMENT_THRESHOLD {
            // Inline the reinforcement write into the same tx so we
            // don't deadlock against the single-connection writer
            // pool. (`MemoryRepo::reinforce_stability` is the standalone
            // version for callers that don't already hold a tx.)
            sqlx::query(
                "UPDATE memories
                 SET stability = MIN(1.0, stability + ?)
                 WHERE user_id = ? AND id = ?",
            )
            .bind(STABILITY_REINFORCEMENT_AMOUNT)
            .bind(user_id)
            .bind(&hit.memory_id)
            .execute(&mut *tx)
            .await?;
        } else if sim >= SYNAPTIC_TAG_THRESHOLD {
            // Synaptic tag — bidirectional auto-link with weight
            // derived from similarity (paper §3.4 / SDK core.py:255).
            let weight = synaptic_tag_weight(sim);
            for (src, tgt) in [
                (new_id, hit.memory_id.as_str()),
                (hit.memory_id.as_str(), new_id),
            ] {
                sqlx::query(
                    "INSERT INTO associations
                       (source_memory_id, target_memory_id, weight, kind, updated_at)
                     VALUES (?, ?, ?, 'synaptic', ?)
                     ON CONFLICT(source_memory_id, target_memory_id) DO UPDATE
                     SET weight = excluded.weight,
                         kind = excluded.kind,
                         updated_at = excluded.updated_at",
                )
                .bind(src)
                .bind(tgt)
                .bind(weight)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            }
        } else {
            break;
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn handle_memory_store_batch(
    args: StoreBatchArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    if args.memories.is_empty() {
        return Ok(Response::ok(ResponseData::MemoryStoredBatch(
            MemoryStoredBatchData {
                ids: Vec::new(),
                associations_created: 0,
            },
        )));
    }

    let cached = CachedEmbeddings::new(ProviderRef(state.embeddings.clone()), &state.store);
    let now = unix_now();
    let provider_name = state.embeddings.name().to_string();
    let model_name = state.embeddings.model().to_string();
    let mem_repo = MemoryRepo::new(&state.store);
    let assoc_repo = AssociationRepo::new(&state.store);

    let mut ids = Vec::with_capacity(args.memories.len());
    for entry in &args.memories {
        let vector = cached.embed(&entry.content).await?;
        let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
        let id = format!("mem_{}", ulid::Ulid::new());

        let mut row = MemoryRow::new_minimal(
            id.clone(),
            args.user_id.clone(),
            entry.content.clone(),
            entry.category.clone(),
            entry.memory_type.clone(),
            now,
        );
        row.embedding = Some(bytes);
        row.embedding_provider = Some(provider_name.clone());
        row.embedding_model = Some(model_name.clone());
        row.metadata = entry.metadata.clone();
        if entry.category == "core" {
            row.retention_floor = CORE_RETENTION_FLOOR;
        }
        mem_repo.insert(&row).await?;
        ids.push(id);
    }

    // Co-creation associations (paper §3.6): every pair of newly-stored
    // memories gets a bidirectional link at the configured initial weight.
    // Pairs only — no self-edges.
    let mut associations_created = 0_u64;
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            assoc_repo
                .link_bidirectional(
                    &ids[i],
                    &ids[j],
                    args.initial_link_weight,
                    now,
                    "cooccurrence",
                )
                .await?;
            associations_created += 2; // bidirectional
        }
    }

    debug!(
        count = ids.len(),
        associations = associations_created,
        "batch stored with co-creation associations"
    );
    Ok(Response::ok(ResponseData::MemoryStoredBatch(
        MemoryStoredBatchData {
            ids,
            associations_created,
        },
    )))
}

async fn handle_memory_search(
    args: SearchMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let cached = CachedEmbeddings::new(ProviderRef(state.embeddings.clone()), &state.store);
    let query_vec = cached.embed(&args.query).await?;
    let now = unix_now();
    let opts = SearchOptions {
        limit: args.limit,
        deep_recall: args.deep_recall,
        provider: state.embeddings.name().to_string(),
        model: state.embeddings.model().to_string(),
        now,
        hybrid: args.hybrid,
        query_text: Some(args.query.clone()),
        graph_expansion_hops: args.graph_expansion_hops,
        min_bridge_edge_weight: 0.3,
        bridge_discovery: args.bridge_discovery,
        max_bridge_paths: 3,
    };
    let searcher = Searcher::new(&state.store);
    let results = searcher.search(&args.user_id, &query_vec, &opts).await?;
    let anchor_ids: Vec<String> = results.iter().map(|r| r.memory_id.clone()).collect();
    let bridge_paths = searcher.find_bridges(&anchor_ids, &opts).await?;

    // Cold-store auto-restore on access (engine.py:606,612):
    // any cold memory that surfaces in deep_recall search is
    // migrated back to hot. Stage 3 e2e validates this path.
    if args.deep_recall && !anchor_ids.is_empty() {
        let mem_repo = MemoryRepo::new(&state.store);
        let rows = mem_repo
            .get_many_for_user(&args.user_id, &anchor_ids)
            .await?;
        let cold_ids: Vec<String> = rows
            .into_iter()
            .filter(|r| r.is_cold)
            .map(|r| r.id)
            .collect();
        if !cold_ids.is_empty() {
            mem_repo
                .migrate_to_hot_many(&args.user_id, &cold_ids)
                .await?;
        }
    }

    // Read-side strengthening: apply spaced-repetition direct boost
    // (Eq 6) for direct hits or associative boost (Eq 8) for graph-
    // expanded hits, refresh last_accessed_at, record the session,
    // and check core-promotion eligibility. Mirrors
    // `_apply_direct_boost` (engine.py:148-160) +
    // `_apply_associative_boost` (engine.py:162-174).
    if !anchor_ids.is_empty() {
        let tagged: Vec<(String, ResultSource)> = results
            .iter()
            .map(|r| (r.memory_id.clone(), r.source))
            .collect();
        apply_post_retrieval_strengthening(state, &args.user_id, &tagged, now).await?;
        // Co-retrieval strengthening (engine.py:621-625): every
        // unordered pair in the result set gets a `+0.1` bump
        // (capped at 1.0) on both directions, with refreshed
        // last_co_retrieval. Cap pairs at the top-K = limit (we use
        // up to 10) to avoid quadratic growth on large result sets.
        let top_k: Vec<&String> = anchor_ids.iter().take(10).collect();
        let mut pairs: Vec<(String, String)> = Vec::new();
        for i in 0..top_k.len() {
            for j in (i + 1)..top_k.len() {
                pairs.push((top_k[i].clone(), top_k[j].clone()));
            }
        }
        if !pairs.is_empty() {
            AssociationRepo::new(&state.store)
                .strengthen_pairs(&pairs, ASSOCIATION_STRENGTHEN_AMOUNT, now, "co-retrieval")
                .await?;
        }
    }

    let hits: Vec<SearchHit> = results
        .into_iter()
        .map(|r| SearchHit {
            memory_id: r.memory_id,
            content: r.content,
            category: r.category,
            memory_type: r.memory_type,
            score: r.score,
        })
        .collect();
    Ok(Response::ok(ResponseData::MemorySearchResults(
        MemorySearchResultsData {
            results: hits,
            bridge_paths,
        },
    )))
}

/// Apply direct-boost stability gain, refresh `last_accessed_at`,
/// increment `retrieval_count`, append a new session id, and check
/// each retrieved memory for core-promotion eligibility. Idempotent
/// w.r.t. core (already-core memories aren't promoted twice).
///
/// Mirrors `engine.py:_apply_direct_boost` + `engine.py:615`. One
/// call = one session id; all memories returned by a single search
/// share that session.
async fn apply_post_retrieval_strengthening(
    state: &Arc<AppState>,
    user_id: &str,
    retrieved: &[(String, ResultSource)],
    now: i64,
) -> Result<(), HandlerError> {
    let mem_repo = MemoryRepo::new(&state.store);
    let ids: Vec<String> = retrieved.iter().map(|(id, _)| id.clone()).collect();
    let rows = mem_repo.get_many_for_user(user_id, &ids).await?;
    // Map id → source for per-row dispatch.
    let source_by_id: std::collections::HashMap<&str, ResultSource> = retrieved
        .iter()
        .map(|(id, src)| (id.as_str(), *src))
        .collect();

    let cfg = lifecycle_config();
    let mut boosts: Vec<(String, f64)> = Vec::with_capacity(rows.len());
    let mut promotions: Vec<String> = Vec::new();

    for row in &rows {
        if row.is_stub {
            continue;
        }
        let dt_days = ((now - row.last_accessed_at).max(0) as f64) / 86_400.0;
        let factor = (dt_days / cfg.spaced_rep_interval_days).min(cfg.max_spaced_rep_multiplier);
        // Direct hits get +0.1 stability bump (Eq 6); graph-expanded
        // hits get the smaller +0.03 (Eq 8). Source defaults to
        // Direct if missing — backward-compat for callers that don't
        // tag (none today).
        let bump_amount = match source_by_id.get(row.id.as_str()) {
            Some(ResultSource::GraphExpanded) => cfg.associative_boost,
            _ => cfg.direct_boost,
        };
        let new_stability = (row.stability + bump_amount * factor).min(1.0);
        boosts.push((row.id.clone(), new_stability));

        // Core-promotion gate uses post-boost stability, post-increment
        // access count, and the session-set length (after the new
        // session id is appended).
        if row.category != "core" {
            let post_access = row.retrieval_count + 1;
            let mut sessions: Vec<String> =
                serde_json::from_str(&row.session_ids).unwrap_or_default();
            if !sessions.contains(&"__pending__".to_string()) {
                sessions.push("__pending__".to_string());
            }
            if post_access >= cfg.core_access_threshold as i64
                && new_stability >= cfg.core_stability_threshold
                && sessions.len() >= cfg.core_session_threshold
            {
                promotions.push(row.id.clone());
            }
        }
    }

    let session_id = format!("s_{}", ulid::Ulid::new());
    mem_repo
        .apply_direct_boost(user_id, &boosts, now, Some(&session_id))
        .await?;
    if !promotions.is_empty() {
        mem_repo.promote_to_core(user_id, &promotions).await?;
    }
    Ok(())
}

async fn handle_memory_get(
    args: GetMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let repo = MemoryRepo::new(&state.store);
    let mut row = repo
        .get_for_user(&args.user_id, &args.id)
        .await?
        .ok_or(HandlerError::NotFound)?;
    // Cold-store auto-restore on access (engine.py:606,612).
    // Any access surfaces a cold memory and migrates it back to hot.
    // The response reflects the post-restore state.
    if row.is_cold {
        repo.migrate_to_hot_many(&args.user_id, &[row.id.clone()])
            .await?;
        row.is_cold = false;
        row.cold_since = None;
        row.days_at_floor = 0;
    }
    Ok(Response::ok(ResponseData::Memory(memory_row_to_data(row))))
}

async fn handle_memory_get_many(
    args: GetManyMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let repo = MemoryRepo::new(&state.store);
    let mut rows = repo.get_many_for_user(&args.user_id, &args.ids).await?;
    // Bulk auto-restore for any cold rows in the result set.
    let cold_ids: Vec<String> = rows
        .iter()
        .filter(|r| r.is_cold)
        .map(|r| r.id.clone())
        .collect();
    if !cold_ids.is_empty() {
        repo.migrate_to_hot_many(&args.user_id, &cold_ids).await?;
        for row in rows.iter_mut().filter(|r| r.is_cold) {
            row.is_cold = false;
            row.cold_since = None;
            row.days_at_floor = 0;
        }
    }
    Ok(Response::ok(ResponseData::Memories(MemoriesData {
        memories: rows.into_iter().map(memory_row_to_data).collect(),
    })))
}

async fn handle_memory_list(
    args: ListMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let filters = MemoryFilters {
        categories: args.categories,
        memory_types: args.memory_types,
        min_retention_floor: args.min_retention_floor,
        min_importance: args.min_importance,
        created_after: args.created_after,
        created_before: args.created_before,
        limit: args.limit,
        offset: args.offset,
        include_superseded: args.include_superseded,
        include_cold: args.include_cold,
        include_stubs: args.include_stubs,
    };
    let rows = MemoryRepo::new(&state.store)
        .query(&args.user_id, &filters)
        .await?;
    Ok(Response::ok(ResponseData::Memories(MemoriesData {
        memories: rows.into_iter().map(memory_row_to_data).collect(),
    })))
}

async fn handle_memory_update(
    args: UpdateMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let upd = MemoryUpdate {
        content: args.content,
        category: args.category,
        memory_type: args.memory_type,
        metadata: args.metadata,
        retention_floor: args.retention_floor,
        importance: args.importance,
        stability: args.stability,
        valid_until: args.valid_until,
    };
    let updated = MemoryRepo::new(&state.store)
        .update(&args.user_id, &args.id, &upd)
        .await?;
    Ok(Response::ok(ResponseData::Affected(AffectedData {
        affected: if updated { 1 } else { 0 },
    })))
}

async fn handle_memory_delete(
    args: DeleteMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let deleted = MemoryRepo::new(&state.store)
        .delete(&args.user_id, &args.id)
        .await?;
    Ok(Response::ok(ResponseData::Affected(AffectedData {
        affected: if deleted { 1 } else { 0 },
    })))
}

async fn handle_memory_delete_many(
    args: DeleteManyMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let count = MemoryRepo::new(&state.store)
        .delete_many(&args.user_id, &args.ids)
        .await?;
    Ok(Response::ok(ResponseData::Affected(AffectedData {
        affected: count,
    })))
}

async fn handle_memory_link(
    args: LinkMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let now = unix_now();
    let assoc = AssociationRepo::new(&state.store);
    let strength = if args.bidirectional {
        assoc
            .link_bidirectional(
                &args.source_id,
                &args.target_id,
                args.strength,
                now,
                &args.kind,
            )
            .await?
    } else {
        assoc
            .create_or_strengthen(
                &args.source_id,
                &args.target_id,
                args.strength,
                now,
                &args.kind,
            )
            .await?
    };
    Ok(Response::ok(ResponseData::LinkStrength(LinkStrengthData {
        strength,
    })))
}

async fn handle_memory_unlink(
    args: UnlinkMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let assoc = AssociationRepo::new(&state.store);
    let count = if args.bidirectional {
        assoc
            .delete_bidirectional(&args.source_id, &args.target_id)
            .await?
    } else {
        if assoc.delete(&args.source_id, &args.target_id).await? {
            1
        } else {
            0
        }
    };
    Ok(Response::ok(ResponseData::Affected(AffectedData {
        affected: count,
    })))
}

async fn handle_memory_get_linked(
    args: GetLinkedArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let linked = AssociationRepo::new(&state.store)
        .linked_for(&args.user_id, &args.source_id, args.min_strength)
        .await?;
    let memories = linked
        .into_iter()
        .map(|lm| LinkedMemoryData {
            memory: memory_row_to_data(lm.memory),
            link_strength: lm.link_strength,
        })
        .collect();
    Ok(Response::ok(ResponseData::LinkedMemories(
        LinkedMemoriesData { memories },
    )))
}

async fn handle_memory_get_linked_many(
    args: GetLinkedManyArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let linked = AssociationRepo::new(&state.store)
        .linked_for_many(&args.user_id, &args.source_ids, args.min_strength)
        .await?;
    let memories = linked
        .into_iter()
        .map(|lm| LinkedMemoryData {
            memory: memory_row_to_data(lm.memory),
            link_strength: lm.link_strength,
        })
        .collect();
    Ok(Response::ok(ResponseData::LinkedMemories(
        LinkedMemoriesData { memories },
    )))
}

async fn handle_vector_search(
    args: VectorSearchArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let now = unix_now();
    let opts = SearchOptions {
        limit: args.limit,
        deep_recall: args.deep_recall,
        provider: args.embedding_provider,
        model: args.embedding_model,
        now,
        hybrid: false,
        query_text: None,
        graph_expansion_hops: 0,
        min_bridge_edge_weight: 0.3,
        bridge_discovery: false,
        max_bridge_paths: 3,
    };
    let results = Searcher::new(&state.store)
        .search(&args.user_id, &args.embedding, &opts)
        .await?;
    let hits: Vec<SearchHit> = results
        .into_iter()
        .map(|r| SearchHit {
            memory_id: r.memory_id,
            content: r.content,
            category: r.category,
            memory_type: r.memory_type,
            score: r.score,
        })
        .collect();
    Ok(Response::ok(ResponseData::MemorySearchResults(
        MemorySearchResultsData {
            results: hits,
            bridge_paths: Vec::new(),
        },
    )))
}

async fn handle_search_lexical(
    args: SearchLexicalArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let ids = MemoryRepo::new(&state.store)
        .bm25_search(&args.user_id, &args.query, args.limit)
        .await?;
    Ok(Response::ok(ResponseData::LexicalIds(LexicalIdsData {
        ids,
    })))
}

async fn handle_batch_update(
    args: BatchUpdateArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let updates: Vec<(String, f64)> = args
        .updates
        .into_iter()
        .map(|u| (u.id, u.retention_floor))
        .collect();
    let count = MemoryRepo::new(&state.store)
        .update_retention_scores(&args.user_id, &updates)
        .await?;
    Ok(Response::ok(ResponseData::Affected(AffectedData {
        affected: count,
    })))
}

// =========================================================================
// Lifecycle
// =========================================================================

async fn handle_lifecycle_request(
    req: LifecycleRequest,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    match req {
        LifecycleRequest::Tick(args) => handle_tick(args, state).await,
        LifecycleRequest::FindFading(args) => {
            handle_find_fading(args, state, connection_user).await
        }
        LifecycleRequest::FindStable(args) => {
            handle_find_stable(args, state, connection_user).await
        }
        LifecycleRequest::MarkSuperseded(args) => {
            handle_mark_superseded(args, state, connection_user).await
        }
        LifecycleRequest::MigrateToCold(args) => {
            handle_migrate_to_cold(args, state, connection_user).await
        }
        LifecycleRequest::MigrateToHot(args) => {
            handle_migrate_to_hot(args, state, connection_user).await
        }
        LifecycleRequest::ConvertToStub(args) => {
            handle_convert_to_stub(args, state, connection_user).await
        }
        LifecycleRequest::UpdateRetention(args) => {
            handle_update_retention(args, state, connection_user).await
        }
        LifecycleRequest::Clear(args) => handle_clear(args, state, connection_user).await,
    }
}

/// Days at floor before a non-core memory is auto-migrated to cold.
/// Mirrors `cold_migration_days = 7` in the Python SDK.
const COLD_MIGRATION_DAYS: i64 = 7;

/// Days a memory may live in cold storage before being converted to a
/// retrieval-stub and dropped. Mirrors `cold_storage_ttl_days = 180`.
const COLD_TTL_DAYS: i64 = 180;

/// Cap on how many queued conflicts a single tick will resolve.
const CONFLICT_RESOLUTION_BATCH: i64 = 50;

async fn handle_tick(_args: TickArgs, state: &Arc<AppState>) -> Result<Response, HandlerError> {
    // The SDK's `tick()` pipeline runs four passes in order
    // (engine.py:810-819 + core.py:tick line 371-378):
    //   1. days_at_floor counter + cold migration
    //   2. cold TTL expiry (cold_since older than 180d → stub)
    //   3. conflict-queue resolution (≤50/tick; LLM-judge in SDK,
    //      heuristic here pending an LLM provider in AppState)
    //   4. consolidation clustering (LLM compression — Phase 11+)
    //
    // `memories_decayed` is the legacy v0 counter; we keep it but
    // also track migrated/expired/resolved separately for the trace.
    let now = unix_now();
    let mut decayed: u64 = 0;

    // -------- Pass 1: counter + cold migration --------
    let hot_rows: Vec<MemoryRow> = sqlx::query_as(
        "SELECT * FROM memories WHERE is_cold = 0 AND is_stub = 0 AND is_superseded = 0",
    )
    .fetch_all(state.store.reader())
    .await?;

    let mut tx = state.store.writer().begin().await?;
    let mut migrated_to_cold = 0u64;
    for row in hot_rows {
        let retention = compute_current_retention(&row, now);
        let at_floor = (retention - row.retention_floor).abs() < 1e-9 && row.retention_floor > 0.0;
        let new_days_at_floor = if at_floor {
            sqlx::query("UPDATE memories SET days_at_floor = days_at_floor + 1 WHERE id = ?")
                .bind(&row.id)
                .execute(&mut *tx)
                .await?;
            decayed += 1;
            row.days_at_floor + 1
        } else {
            if row.days_at_floor > 0 {
                sqlx::query("UPDATE memories SET days_at_floor = 0 WHERE id = ?")
                    .bind(&row.id)
                    .execute(&mut *tx)
                    .await?;
            }
            0
        };

        // Cold-migrate non-core memories that have been at floor long
        // enough. Core memories are exempt (engine.py:665-666).
        if row.category != "core" && new_days_at_floor >= COLD_MIGRATION_DAYS {
            sqlx::query("UPDATE memories SET is_cold = 1, cold_since = ? WHERE id = ?")
                .bind(now)
                .bind(&row.id)
                .execute(&mut *tx)
                .await?;
            migrated_to_cold += 1;
        }
    }
    tx.commit().await?;

    // -------- Pass 2: cold TTL expiry → stub --------
    let stale_cold: Vec<MemoryRow> = sqlx::query_as(
        "SELECT * FROM memories
         WHERE is_cold = 1 AND is_stub = 0 AND cold_since IS NOT NULL
           AND cold_since < ?",
    )
    .bind(now - COLD_TTL_DAYS * 86_400)
    .fetch_all(state.store.reader())
    .await?;

    let mut tx = state.store.writer().begin().await?;
    let mut expired_to_stub = 0u64;
    for row in stale_cold {
        if row.category == "core" {
            continue;
        }
        let preview: String = row.content.chars().take(200).collect();
        let stub = format!("[archived] {preview}");
        sqlx::query(
            "UPDATE memories SET is_stub = 1, stub_content = ?, embedding = NULL WHERE id = ?",
        )
        .bind(&stub)
        .bind(&row.id)
        .execute(&mut *tx)
        .await?;
        expired_to_stub += 1;
    }
    tx.commit().await?;

    // -------- Pass 3: conflict resolution --------
    let resolved = resolve_conflicts(state, now).await?;

    // -------- Pass 4: consolidation clustering + LLM compress --------
    let consolidated = consolidate_at_tick(state, now).await?;

    tracing::debug!(
        decayed,
        migrated_to_cold,
        expired_to_stub,
        resolved,
        consolidated,
        "tick complete"
    );

    Ok(Response::ok(ResponseData::Tick(TickResultData {
        completed: true,
        memories_decayed: decayed,
    })))
}

/// Drain up to `CONFLICT_RESOLUTION_BATCH` conflict pairs from the
/// queue. For each, apply a heuristic resolver (the loser is
/// LLM judge prompt — verbatim from cognitive_memory/extraction.py:230-249.
/// Returns one of CONTRADICTION / UPDATE / OVERLAP / NONE.
const CONFLICT_JUDGE_PROMPT: &str =
    "Does the new memory contradict or update an existing memory?\n\n\
Existing memory: \"{existing}\"\n\
New memory: \"{new}\"\n\n\
Respond with exactly one word: CONTRADICTION, UPDATE, OVERLAP, or NONE.\n\
- CONTRADICTION: the new memory directly negates the existing one\n\
- UPDATE: the new memory is a newer version of the same fact\n\
- OVERLAP: they cover similar ground but don't conflict\n\
- NONE: they are unrelated";

/// One of the four labels returned by the LLM judge. Unparseable
/// responses default to `None_` (= "do nothing safely") per the SDK
/// fallback at extraction.py:248.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictLabel {
    Contradiction,
    Update,
    Overlap,
    None_,
}

fn parse_conflict_label(raw: &str) -> ConflictLabel {
    let upper = raw.trim().to_uppercase();
    if upper.contains("CONTRADICTION") {
        ConflictLabel::Contradiction
    } else if upper.contains("UPDATE") {
        ConflictLabel::Update
    } else if upper.contains("OVERLAP") {
        ConflictLabel::Overlap
    } else {
        ConflictLabel::None_
    }
}

/// Resolve queued conflicts. Two paths:
///   - With `state.llm = Some(...)`: ask the LLM to classify each
///     pair into CONTRADICTION/UPDATE/OVERLAP/NONE; act on the label
///     per SDK semantics (extraction.py:230-249).
///   - With `state.llm = None`: fall back to the original heuristic
///     (higher-importance/more-recent wins as if every pair were a
///     CONTRADICTION). Logs a one-time tracing::warn so operators
///     know they're degraded.
///
/// Either way, every queued pair is dropped from `conflict_queue` so
/// the queue drains regardless of resolver choice.
async fn resolve_conflicts(state: &Arc<AppState>, now: i64) -> Result<u64, HandlerError> {
    let pairs: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT id, user_id, new_memory_id, existing_memory_id
         FROM conflict_queue
         ORDER BY similarity DESC LIMIT ?",
    )
    .bind(CONFLICT_RESOLUTION_BATCH)
    .fetch_all(state.store.reader())
    .await?;

    if pairs.is_empty() {
        return Ok(0);
    }

    let _ = now;
    let mut resolved = 0u64;
    let mut tx = state.store.writer().begin().await?;
    for (queue_id, user_id, new_id, existing_id) in pairs {
        // Re-fetch both rows; either may have been deleted/superseded
        // in the meantime.
        let new_row: Option<MemoryRow> =
            sqlx::query_as("SELECT * FROM memories WHERE user_id = ? AND id = ?")
                .bind(&user_id)
                .bind(&new_id)
                .fetch_optional(&mut *tx)
                .await?;
        let existing_row: Option<MemoryRow> =
            sqlx::query_as("SELECT * FROM memories WHERE user_id = ? AND id = ?")
                .bind(&user_id)
                .bind(&existing_id)
                .fetch_optional(&mut *tx)
                .await?;

        if let (Some(n), Some(e)) = (new_row, existing_row) {
            if !n.is_superseded && !e.is_superseded {
                // Decide outcome: LLM if configured, else heuristic
                // (which acts as if every pair were CONTRADICTION).
                let outcome = match state.llm.as_ref() {
                    Some(provider) => {
                        let prompt = CONFLICT_JUDGE_PROMPT
                            .replace("{existing}", &e.content)
                            .replace("{new}", &n.content);
                        match provider.complete(&prompt, 20).await {
                            Ok(raw) => parse_conflict_label(&raw),
                            Err(err) => {
                                tracing::warn!(
                                    %err,
                                    "LLM judge failed; defaulting to NONE for pair"
                                );
                                ConflictLabel::None_
                            }
                        }
                    }
                    None => ConflictLabel::Contradiction, // heuristic mode
                };

                match outcome {
                    ConflictLabel::Contradiction | ConflictLabel::Update => {
                        // SDK extraction.py:236-249: existing → superseded
                        // by new. Heuristic mode picks winner via
                        // (importance, recency) for backward-compat.
                        let (winner, loser) = match (state.llm.is_some(), outcome) {
                            (true, ConflictLabel::Update) => {
                                // SDK: UPDATE means "new is the current
                                // version". Winner is the new memory;
                                // its importance is bumped to max of
                                // both.
                                let new_importance = n.importance.max(e.importance);
                                if (new_importance - n.importance).abs() > f64::EPSILON {
                                    sqlx::query("UPDATE memories SET importance = ? WHERE id = ?")
                                        .bind(new_importance)
                                        .bind(&n.id)
                                        .execute(&mut *tx)
                                        .await?;
                                }
                                (n, e)
                            }
                            (true, ConflictLabel::Contradiction) => (n, e),
                            _ => {
                                // Heuristic mode.
                                if n.importance > e.importance
                                    || (n.importance == e.importance && n.created_at > e.created_at)
                                {
                                    (n, e)
                                } else {
                                    (e, n)
                                }
                            }
                        };
                        sqlx::query(
                            "UPDATE memories
                             SET is_superseded = 1, superseded_by = ?, contradicted_by = ?
                             WHERE id = ?",
                        )
                        .bind(&winner.id)
                        .bind(&winner.id)
                        .bind(&loser.id)
                        .execute(&mut *tx)
                        .await?;
                        // Demote a CORE loser to semantic (engine.py:418-419).
                        if loser.category == "core" {
                            sqlx::query(
                                "UPDATE memories
                                 SET category = 'semantic', retention_floor = 0.0
                                 WHERE id = ?",
                            )
                            .bind(&loser.id)
                            .execute(&mut *tx)
                            .await?;
                        }
                        resolved += 1;
                    }
                    ConflictLabel::Overlap | ConflictLabel::None_ => {
                        // Both stay non-superseded; drop queue entry.
                    }
                }
            }
        }

        sqlx::query("DELETE FROM conflict_queue WHERE id = ?")
            .bind(queue_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(resolved)
}

/// Memories below this retention are eligible for consolidation
/// clustering. SDK default `consolidation_retention_threshold = 0.20`.
const CONSOLIDATION_RETENTION_THRESHOLD: f64 = 0.20;

/// Min cluster size to trigger LLM compression. Smaller groups don't
/// justify a summary memory. SDK default 5.
const CONSOLIDATION_GROUP_SIZE: usize = 5;

/// Cosine similarity threshold for cluster membership. SDK default 0.70.
const CONSOLIDATION_SIM_THRESHOLD: f64 = 0.70;

/// Cap on summary memories produced per tick. Keeps tick latency
/// bounded; remaining clusters get picked up next tick.
const CONSOLIDATION_MAX_PER_TICK: usize = 5;

/// Consolidation prompt — verbatim from cognitive_memory/extraction.py:107-112.
const CONSOLIDATION_PROMPT: &str = "Compress these related memories into a single concise summary that preserves all key facts.\n\n\
Memories:\n{memories}\n\n\
Write one clear paragraph. Preserve specific names, dates, numbers, and preferences. Do not add information that isn't in the originals.";

/// Pass 4: cluster fading memories by category/similarity and ask the
/// LLM to compress each cluster into a single summary. Originals are
/// marked `is_superseded=1, superseded_by=summary_id` and migrated to
/// cold (preserves audit trail; reachable via `cm get` and via
/// `cm search --deep-recall`).
///
/// Skipped silently if `state.llm` is None (no LLM configured); the
/// daemon traces a single warn at the call site.
async fn consolidate_at_tick(state: &Arc<AppState>, now: i64) -> Result<u64, HandlerError> {
    let Some(provider) = state.llm.clone() else {
        // Don't log on every tick — too noisy. The first call to
        // `cm tick` after startup with no LLM logs a warn at the
        // `consolidated` field; debug-level otherwise. The opt-in
        // surface (`cm config set-llm`) is documented in CLI help.
        return Ok(0);
    };

    // Distinct user_ids with hot memories.
    let users: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT user_id FROM memories
         WHERE is_cold = 0 AND is_stub = 0 AND is_superseded = 0",
    )
    .fetch_all(state.store.reader())
    .await?;

    let mut summaries_created = 0u64;
    'outer: for (user_id,) in users {
        let rows: Vec<MemoryRow> = sqlx::query_as(
            "SELECT * FROM memories
             WHERE user_id = ? AND is_cold = 0 AND is_stub = 0 AND is_superseded = 0
               AND embedding IS NOT NULL",
        )
        .bind(&user_id)
        .fetch_all(state.store.reader())
        .await?;

        // Filter to "fading" — computed retention below threshold.
        let fading: Vec<MemoryRow> = rows
            .into_iter()
            .filter(|r| compute_current_retention(r, now) < CONSOLIDATION_RETENTION_THRESHOLD)
            .collect();

        // Group by category. Greedy clustering happens within each.
        let mut by_cat: std::collections::HashMap<String, Vec<MemoryRow>> =
            std::collections::HashMap::new();
        for row in fading {
            by_cat.entry(row.category.clone()).or_default().push(row);
        }

        for (_category, mems) in by_cat {
            if mems.len() < CONSOLIDATION_GROUP_SIZE {
                continue;
            }
            let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
            for i in 0..mems.len() {
                if used.contains(&mems[i].id) {
                    continue;
                }
                let head_vec = decode_embedding(mems[i].embedding.as_deref());
                if head_vec.is_empty() {
                    continue;
                }
                let mut group: Vec<&MemoryRow> = vec![&mems[i]];
                for cand in mems.iter().skip(i + 1) {
                    if used.contains(&cand.id) {
                        continue;
                    }
                    let cand_vec = decode_embedding(cand.embedding.as_deref());
                    if cand_vec.len() != head_vec.len() {
                        continue;
                    }
                    let sim = cosine_similarity_f32(&head_vec, &cand_vec) as f64;
                    if sim >= CONSOLIDATION_SIM_THRESHOLD {
                        group.push(cand);
                        if group.len() >= CONSOLIDATION_GROUP_SIZE {
                            break;
                        }
                    }
                }
                if group.len() >= CONSOLIDATION_GROUP_SIZE {
                    let summary_id =
                        compress_cluster(state, &user_id, &group, &provider, now).await?;
                    for m in &group {
                        used.insert(m.id.clone());
                    }
                    summaries_created += 1;
                    let _ = summary_id;
                    if (summaries_created as usize) >= CONSOLIDATION_MAX_PER_TICK {
                        break 'outer;
                    }
                }
            }
        }
    }
    Ok(summaries_created)
}

/// Compress a cluster: send the contents to the LLM, embed the
/// summary, write a new summary memory, and mark all originals
/// superseded + migrated to cold.
async fn compress_cluster(
    state: &Arc<AppState>,
    user_id: &str,
    group: &[&MemoryRow],
    provider: &Arc<dyn cognitive_memory_llm::LlmProvider>,
    now: i64,
) -> Result<String, HandlerError> {
    let bullet_list: String = group
        .iter()
        .map(|m| format!("- {}", m.content))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = CONSOLIDATION_PROMPT.replace("{memories}", &bullet_list);
    let summary_text = provider
        .complete(&prompt, 200)
        .await
        .map_err(|e| HandlerError::InvalidPayload(format!("LLM compress: {e}")))?;

    let cached = CachedEmbeddings::new(ProviderRef(state.embeddings.clone()), &state.store);
    let summary_vec = cached.embed(&summary_text).await?;
    let summary_bytes: Vec<u8> = summary_vec.iter().flat_map(|f| f.to_le_bytes()).collect();

    let summary_id = format!("mem_{}", ulid::Ulid::new());
    let max_imp = group
        .iter()
        .map(|m| m.importance)
        .fold(f64::NEG_INFINITY, f64::max);
    let mean_stab: f64 = group.iter().map(|m| m.stability).sum::<f64>() / (group.len() as f64);
    let category = group[0].category.clone();

    let mut summary_row = MemoryRow::new_minimal(
        summary_id.clone(),
        user_id,
        summary_text,
        category.clone(),
        "summary",
        now,
    );
    summary_row.embedding = Some(summary_bytes);
    summary_row.embedding_provider = Some(state.embeddings.name().to_string());
    summary_row.embedding_model = Some(state.embeddings.model().to_string());
    summary_row.importance = max_imp.max(0.0);
    summary_row.stability = mean_stab;
    if category == "core" {
        summary_row.retention_floor = CORE_RETENTION_FLOOR;
    }

    // Insert summary first (own writer call), then mark originals in
    // a single tx. Two writes is fine — the summary is the new
    // source of truth, the originals get demoted to its children.
    MemoryRepo::new(&state.store).insert(&summary_row).await?;

    let mut tx = state.store.writer().begin().await?;
    for m in group {
        sqlx::query(
            "UPDATE memories
             SET is_superseded = 1, superseded_by = ?,
                 is_cold = 1, cold_since = ?
             WHERE user_id = ? AND id = ?",
        )
        .bind(&summary_id)
        .bind(now)
        .bind(user_id)
        .bind(&m.id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(summary_id)
}

/// Decode the f32 LE-byte embedding blob into a vector. Returns an
/// empty vec when bytes are absent or wrong-sized (caller skips).
fn decode_embedding(bytes: Option<&[u8]>) -> Vec<f32> {
    bytes
        .map(|b| {
            b.chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        })
        .unwrap_or_default()
}

/// Inline cosine for the consolidation hot path. Avoids depending on
/// the `search` crate from `daemon` for one tiny function.
fn cosine_similarity_f32(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|y| y * y).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

async fn handle_find_fading(
    args: FindFadingArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let rows = MemoryRepo::new(&state.store)
        .find_fading_candidates(&args.user_id)
        .await?;

    // Compute retention per candidate, keep those at or below the
    // threshold, sort ascending (most-faded first), trim to limit.
    let now = unix_now();
    let mut scored: Vec<(MemoryRow, f64)> = rows
        .into_iter()
        .map(|row| {
            let r = compute_current_retention(&row, now);
            (row, r)
        })
        .filter(|(_, r)| *r <= args.max_retention)
        .collect();
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(args.limit as usize);

    Ok(Response::ok(ResponseData::Memories(MemoriesData {
        memories: scored
            .into_iter()
            .map(|(row, _)| memory_row_to_data(row))
            .collect(),
    })))
}

async fn handle_find_stable(
    args: FindStableArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let rows = MemoryRepo::new(&state.store)
        .find_stable(
            &args.user_id,
            args.min_stability,
            args.min_access_count,
            args.limit,
        )
        .await?;
    Ok(Response::ok(ResponseData::Memories(MemoriesData {
        memories: rows.into_iter().map(memory_row_to_data).collect(),
    })))
}

async fn handle_mark_superseded(
    args: MarkSupersededArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let count = MemoryRepo::new(&state.store)
        .mark_superseded(&args.user_id, &args.ids, &args.summary_id)
        .await?;
    Ok(Response::ok(ResponseData::Affected(AffectedData {
        affected: count,
    })))
}

async fn handle_migrate_to_cold(
    args: MigrateToColdArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let ok = MemoryRepo::new(&state.store)
        .migrate_to_cold(&args.user_id, &args.id, args.cold_since)
        .await?;
    Ok(Response::ok(ResponseData::Affected(AffectedData {
        affected: if ok { 1 } else { 0 },
    })))
}

async fn handle_migrate_to_hot(
    args: MigrateToHotArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let ok = MemoryRepo::new(&state.store)
        .migrate_to_hot(&args.user_id, &args.id)
        .await?;
    Ok(Response::ok(ResponseData::Affected(AffectedData {
        affected: if ok { 1 } else { 0 },
    })))
}

async fn handle_convert_to_stub(
    args: ConvertToStubArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let ok = MemoryRepo::new(&state.store)
        .convert_to_stub(&args.user_id, &args.id, &args.stub_content)
        .await?;
    Ok(Response::ok(ResponseData::Affected(AffectedData {
        affected: if ok { 1 } else { 0 },
    })))
}

async fn handle_update_retention(
    args: UpdateRetentionArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let upd = MemoryUpdate {
        retention_floor: Some(args.retention_floor),
        ..MemoryUpdate::default()
    };
    let updated = MemoryRepo::new(&state.store)
        .update(&args.user_id, &args.id, &upd)
        .await?;
    Ok(Response::ok(ResponseData::Affected(AffectedData {
        affected: if updated { 1 } else { 0 },
    })))
}

async fn handle_clear(
    args: ClearArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    if !args.confirm {
        return Err(HandlerError::InvalidPayload(
            "Lifecycle::Clear requires confirm=true — destructive".to_string(),
        ));
    }
    let count = MemoryRepo::new(&state.store)
        .clear_user(&args.user_id)
        .await?;
    Ok(Response::ok(ResponseData::Affected(AffectedData {
        affected: count,
    })))
}

// =========================================================================
// Helpers
// =========================================================================

fn require_user_match(arg_user: &str, connection_user: &str) -> Result<(), HandlerError> {
    if arg_user != connection_user {
        return Err(HandlerError::InvalidPayload(format!(
            "request user_id {arg_user:?} != connection user_id {connection_user:?}"
        )));
    }
    Ok(())
}

fn memory_row_to_data(row: MemoryRow) -> MemoryData {
    let current_retention = compute_current_retention(&row, unix_now());
    MemoryData {
        id: row.id,
        user_id: row.user_id,
        content: row.content,
        category: row.category,
        memory_type: row.memory_type,
        created_at: row.created_at,
        last_accessed_at: row.last_accessed_at,
        valid_from: row.valid_from,
        valid_until: row.valid_until,
        retention_floor: row.retention_floor,
        retrieval_count: row.retrieval_count,
        importance: row.importance,
        stability: row.stability,
        is_cold: row.is_cold,
        cold_since: row.cold_since,
        is_superseded: row.is_superseded,
        superseded_by: row.superseded_by,
        is_stub: row.is_stub,
        stub_content: row.stub_content,
        metadata: row.metadata,
        current_retention,
        days_at_floor: row.days_at_floor,
    }
}

/// Compute R(m) for a memory row at the given wall-clock time using the
/// paper's power-law decay model (Equation 1). Reads the memory's
/// stability, importance, retention floor, and category-derived
/// `base_decay_rate`.
fn compute_current_retention(row: &MemoryRow, now: i64) -> f64 {
    let state = MemoryState {
        last_accessed_at: row.last_accessed_at,
        created_at: row.created_at,
        stability: row.stability,
        importance: row.importance,
        base_decay_rate: base_decay_rate_for_category(&row.category),
        floor: row.retention_floor,
        is_stub: row.is_stub,
        access_count: row.retrieval_count as u64,
        session_count: 0,
        category: parse_category(&row.category),
    };
    compute_retention(&state, now, &lifecycle_config())
}

fn lifecycle_config() -> LifecycleConfig {
    // Paper §3.2 defaults to power-law decay (gamma=0.7); the lifecycle
    // crate's Default is exponential for backward-compat with the older
    // SDK. The daemon ships the paper-faithful model.
    LifecycleConfig {
        decay_model: cognitive_memory_lifecycle::DecayModel::Power,
        ..LifecycleConfig::default()
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

// CachedEmbeddings requires P: EmbeddingProvider (sized type, not dyn).
// Wrap the trait object so it can be passed as a generic parameter.
struct ProviderRef(Arc<dyn EmbeddingProvider>);

#[async_trait::async_trait]
impl EmbeddingProvider for ProviderRef {
    fn name(&self) -> &str {
        let _ = PROVIDER_NAME;
        self.0.name()
    }
    fn model(&self) -> &str {
        self.0.model()
    }
    fn dimension(&self) -> usize {
        self.0.dimension()
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.0.embed(text).await
    }
}
