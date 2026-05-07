//! Request handlers, dispatcher entry point.
//!
//! One function per `Request` variant we serve. Full v0.1.0 surface —
//! feature-parity with the SDK's `MemoryAdapter` interface plus the
//! paper-faithful StoreBatch with co-creation auto-association.

use cognitive_memory_embeddings::{CachedEmbeddings, EmbeddingError, EmbeddingProvider};
use cognitive_memory_lifecycle::{
    base_decay_rate_for_category, compute_retention, parse_category, LifecycleConfig, MemoryState,
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
use cognitive_memory_search::{SearchError, SearchOptions, Searcher};
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
    }
    // Synaptic tagging (paper §3.4): category=core triggers protected
    // retention floor at encoding time.
    if args.category == "core" {
        row.retention_floor = CORE_RETENTION_FLOOR;
    }

    MemoryRepo::new(&state.store).insert(&row).await?;
    debug!(memory_id = %id, "memory stored");
    Ok(Response::ok(ResponseData::MemoryStored(MemoryStoredData {
        id,
    })))
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

async fn handle_memory_get(
    args: GetMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let row = MemoryRepo::new(&state.store)
        .get_for_user(&args.user_id, &args.id)
        .await?
        .ok_or(HandlerError::NotFound)?;
    Ok(Response::ok(ResponseData::Memory(memory_row_to_data(row))))
}

async fn handle_memory_get_many(
    args: GetManyMemoryArgs,
    state: &Arc<AppState>,
    connection_user: &str,
) -> Result<Response, HandlerError> {
    require_user_match(&args.user_id, connection_user)?;
    let rows = MemoryRepo::new(&state.store)
        .get_many_for_user(&args.user_id, &args.ids)
        .await?;
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

async fn handle_tick(_args: TickArgs, state: &Arc<AppState>) -> Result<Response, HandlerError> {
    // Walk every hot non-stub memory; compute its current retention; if
    // retention has flatlined at the floor, increment `days_at_floor`,
    // otherwise reset it to 0. `memories_decayed` reports the count
    // currently at floor.
    //
    // This is the consolidation pass — it does not advance wall-clock
    // time; decay is read-side. `days_at_floor` is what later passes use
    // to decide cold-migration candidacy.
    let now = unix_now();
    let rows: Vec<MemoryRow> =
        sqlx::query_as("SELECT * FROM memories WHERE is_cold = 0 AND is_stub = 0")
            .fetch_all(state.store.reader())
            .await?;

    let mut decayed: u64 = 0;
    let mut tx = state.store.writer().begin().await?;
    for row in rows {
        let retention = compute_current_retention(&row, now);
        // "At floor" means the computed retention has clamped to the
        // stored floor. Use a small epsilon for f64 comparison.
        let at_floor = (retention - row.retention_floor).abs() < 1e-9 && row.retention_floor > 0.0;
        if at_floor {
            sqlx::query("UPDATE memories SET days_at_floor = days_at_floor + 1 WHERE id = ?")
                .bind(&row.id)
                .execute(&mut *tx)
                .await?;
            decayed += 1;
        } else if row.days_at_floor > 0 {
            sqlx::query("UPDATE memories SET days_at_floor = 0 WHERE id = ?")
                .bind(&row.id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;

    Ok(Response::ok(ResponseData::Tick(TickResultData {
        completed: true,
        memories_decayed: decayed,
    })))
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
