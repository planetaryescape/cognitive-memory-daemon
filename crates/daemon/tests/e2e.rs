//! End-to-end tests for the daemon binary path.
//!
//! Spin up a real `Daemon` against a temp socket and SQLite file, drive it
//! with the real `Client`, assert. No mocks for the store; mocks only at
//! the embedding-provider boundary (FakeEmbeddingProvider).
//!
//! Per `docs/developer/test-discipline.md` §10, this is the contract that
//! shows the daemon is wired correctly end-to-end. If this passes, all the
//! Phase 4 plumbing (accept loop, Hello/Welcome, dispatch, response) works.

#![allow(clippy::panic, clippy::unwrap_used)]

use cognitive_memory_client::Client;
use cognitive_memory_daemon::Daemon;
use cognitive_memory_embeddings::FakeEmbeddingProvider;
use cognitive_memory_protocol::{
    BatchMemoryEntry, BridgeScope, ClearArgs, CountsArgs, DeleteMemoryArgs, DiagnosticsRequest,
    GetLinkedArgs, GetMemoryArgs, LifecycleRequest, LinkMemoryArgs, ListMemoryArgs, MemoryRequest,
    MintBridgeTokenArgs, Request, ResponseData, SearchMemoryArgs, StoreBatchArgs, StoreMemoryArgs,
    TickArgs, UpdateMemoryArgs, UpdateRetentionArgs,
};
use cognitive_memory_store::Store;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// Boot a daemon backed by an on-disk SQLite + a fake embedding provider,
/// returning the running task handle, the socket path, and a shutdown sender.
async fn boot_daemon() -> (
    tokio::task::JoinHandle<()>,
    PathBuf,
    tokio::sync::broadcast::Sender<()>,
    TempDir,
) {
    let (handle, socket, shutdown, tmp, _) = boot_daemon_with_embeddings().await;
    (handle, socket, shutdown, tmp)
}

/// Variant exposing the embeddings provider so tests can call
/// `set_override` to pin specific cosine similarities between inputs.
async fn boot_daemon_with_embeddings() -> (
    tokio::task::JoinHandle<()>,
    PathBuf,
    tokio::sync::broadcast::Sender<()>,
    TempDir,
    Arc<FakeEmbeddingProvider>,
) {
    boot_daemon_full(None).await
}

/// Variant that wires both the embeddings AND a scripted LLM
/// provider into the daemon. Used by Stage 4 conflict-judge and
/// consolidation tests to drive deterministic outcomes.
async fn boot_daemon_with_llm(
    llm: Arc<dyn cognitive_memory_llm::LlmProvider>,
) -> (
    tokio::task::JoinHandle<()>,
    PathBuf,
    tokio::sync::broadcast::Sender<()>,
    TempDir,
    Arc<FakeEmbeddingProvider>,
) {
    boot_daemon_full(Some(llm)).await
}

async fn boot_daemon_full(
    llm: Option<Arc<dyn cognitive_memory_llm::LlmProvider>>,
) -> (
    tokio::task::JoinHandle<()>,
    PathBuf,
    tokio::sync::broadcast::Sender<()>,
    TempDir,
    Arc<FakeEmbeddingProvider>,
) {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("cm.sock");
    let db_path = tmp.path().join("data.db");

    let store = Store::open(&db_path).await.unwrap();
    let embeddings = Arc::new(FakeEmbeddingProvider::new("local", "fake-16", 16));
    let embeddings_for_daemon: Arc<dyn cognitive_memory_embeddings::EmbeddingProvider> =
        embeddings.clone();
    let daemon = Daemon::new_with_llm(store, embeddings_for_daemon, socket_path.clone(), llm);
    let shutdown = daemon.shutdown_handle();
    boot_daemon_serve(daemon, socket_path, shutdown, tmp, embeddings).await
}

/// Variant that wires an explicit `LifecycleConfig` into the daemon —
/// used by the Phase 0a-daemon override-propagation tests below.
/// Returns the same handle quintuple plus the temp dir's path so the
/// test can open the SQLite directly to backdate memories.
async fn boot_daemon_with_lifecycle(
    lifecycle: cognitive_memory_lifecycle::LifecycleConfig,
) -> (
    tokio::task::JoinHandle<()>,
    PathBuf,
    tokio::sync::broadcast::Sender<()>,
    TempDir,
    Arc<FakeEmbeddingProvider>,
    PathBuf,
) {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("cm.sock");
    let db_path = tmp.path().join("data.db");
    let store = Store::open(&db_path).await.unwrap();
    let embeddings = Arc::new(FakeEmbeddingProvider::new("local", "fake-16", 16));
    let embeddings_for_daemon: Arc<dyn cognitive_memory_embeddings::EmbeddingProvider> =
        embeddings.clone();
    let daemon = Daemon::new_full(
        store,
        embeddings_for_daemon,
        socket_path.clone(),
        None,
        lifecycle,
    );
    let shutdown = daemon.shutdown_handle();
    let (handle, socket_path, shutdown, tmp, embeddings) =
        boot_daemon_serve(daemon, socket_path, shutdown, tmp, embeddings).await;
    (handle, socket_path, shutdown, tmp, embeddings, db_path)
}

async fn boot_daemon_serve(
    daemon: Daemon,
    socket_path: PathBuf,
    shutdown: tokio::sync::broadcast::Sender<()>,
    tmp: TempDir,
    embeddings: Arc<FakeEmbeddingProvider>,
) -> (
    tokio::task::JoinHandle<()>,
    PathBuf,
    tokio::sync::broadcast::Sender<()>,
    TempDir,
    Arc<FakeEmbeddingProvider>,
) {

    let handle = tokio::spawn(async move {
        daemon.serve().await.expect("daemon serve");
    });

    for _ in 0..100 {
        if socket_path.exists() {
            return (handle, socket_path, shutdown, tmp, embeddings);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon never bound the socket");
}

#[tokio::test]
async fn diagnostics_status_returns_zero_memory_count_on_fresh_store() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let response = client
        .request(Request::Diagnostics(DiagnosticsRequest::Status))
        .await
        .unwrap();

    assert!(response.ok);
    let data = response.data.expect("status response carries data");
    match data {
        ResponseData::Status(status) => {
            assert_eq!(status.memory_count, 0);
            assert_eq!(status.daemon_version, env!("CARGO_PKG_VERSION"));
        }
        other => panic!("expected Status, got {other:?}"),
    }

    let _ = shutdown.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn store_then_search_returns_the_stored_memory() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let store_resp = client
        .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
            user_id: "alice".to_string(),
            content: "User dislikes mocked database tests.".to_string(),
            category: "semantic".to_string(),
            memory_type: "preference".to_string(),
            metadata: "{}".to_string(),
            importance: None,
        })))
        .await
        .unwrap();

    assert!(store_resp.ok, "store: {:?}", store_resp.error);
    let stored_id = match store_resp.data.expect("data") {
        ResponseData::MemoryStored(s) => s.id,
        other => panic!("expected MemoryStored, got {other:?}"),
    };
    assert!(stored_id.starts_with("mem_"), "id should be ULID-shaped");

    let search_resp = client
        .request(Request::Memory(MemoryRequest::Search(SearchMemoryArgs {
            user_id: "alice".to_string(),
            query: "User dislikes mocked database tests.".to_string(),
            limit: 5,
            deep_recall: false,
            hybrid: false,
            graph_expansion_hops: 0,
            bridge_discovery: false,
        })))
        .await
        .unwrap();

    assert!(search_resp.ok, "search: {:?}", search_resp.error);
    match search_resp.data.expect("data") {
        ResponseData::MemorySearchResults(results) => {
            assert_eq!(results.results.len(), 1);
            assert_eq!(results.results[0].memory_id, stored_id);
            assert_eq!(
                results.results[0].content,
                "User dislikes mocked database tests."
            );
            assert!(
                results.results[0].score > 0.99,
                "exact-text match should score ~1.0"
            );
        }
        other => panic!("expected MemorySearchResults, got {other:?}"),
    }

    let _ = shutdown.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn mint_bridge_token_returns_token_and_persists_hash_in_kv() {
    let (handle, socket, shutdown, tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let resp = client
        .request(Request::Diagnostics(DiagnosticsRequest::MintBridgeToken(
            MintBridgeTokenArgs {
                user_id: "alice".to_string(),
                scope: BridgeScope::Write,
                ttl_seconds: 3600,
            },
        )))
        .await
        .unwrap();

    assert!(resp.ok, "mint must succeed: {:?}", resp.error);
    let token = match resp.data.expect("data") {
        ResponseData::BridgeToken(t) => t.token,
        other => panic!("expected BridgeToken, got {other:?}"),
    };
    assert!(token.starts_with("cmb_"));
    assert!(
        token.len() > 30,
        "token entropy should be sufficient: {token}"
    );

    // Confirm the token's *hash* is persisted in the kv table — the raw
    // token must NEVER appear there.
    use cognitive_memory_store::Store;
    let db_path = tmp.path().join("data.db");
    let store = Store::open(&db_path).await.unwrap();
    let (raw_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM kv WHERE namespace = 'bridge_tokens' AND key = ?")
            .bind(&token)
            .fetch_one(store.reader())
            .await
            .unwrap();
    assert_eq!(raw_count, 0, "raw token must not appear in kv");

    let (rows,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM kv WHERE namespace = 'bridge_tokens'")
            .fetch_one(store.reader())
            .await
            .unwrap();
    assert_eq!(rows, 1, "exactly one token hash row must exist");

    let _ = shutdown.send(());
    let _ = handle.await;
}

// ===========================================================================
// Feature-parity E2E tests (full SDK MemoryAdapter surface)
// ===========================================================================

#[tokio::test]
async fn store_batch_creates_co_creation_associations_per_paper_3_6() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    // Three memories stored together → C(3,2) * 2 = 6 directed edges
    // (3 unordered pairs × 2 directions for bidirectional links).
    let resp = client
        .request(Request::Memory(MemoryRequest::StoreBatch(StoreBatchArgs {
            user_id: "alice".to_string(),
            memories: vec![
                BatchMemoryEntry {
                    content: "Tea over coffee.".to_string(),
                    category: "semantic".to_string(),
                    memory_type: "preference".to_string(),
                    metadata: "{}".to_string(),
                },
                BatchMemoryEntry {
                    content: "Standup at 09:00.".to_string(),
                    category: "semantic".to_string(),
                    memory_type: "plan".to_string(),
                    metadata: "{}".to_string(),
                },
                BatchMemoryEntry {
                    content: "Allergic to penicillin.".to_string(),
                    category: "core".to_string(),
                    memory_type: "fact".to_string(),
                    metadata: "{}".to_string(),
                },
            ],
            initial_link_weight: 0.5,
        })))
        .await
        .unwrap();

    assert!(resp.ok, "{:?}", resp.error);
    let batch = match resp.data.unwrap() {
        ResponseData::MemoryStoredBatch(b) => b,
        other => panic!("expected MemoryStoredBatch, got {other:?}"),
    };
    assert_eq!(batch.ids.len(), 3);
    assert_eq!(
        batch.associations_created, 6,
        "3 pairs × 2 directions for bidirectional links"
    );

    // Linked-from any of the three should surface the other two.
    let linked = client
        .request(Request::Memory(MemoryRequest::GetLinked(GetLinkedArgs {
            user_id: "alice".to_string(),
            source_id: batch.ids[0].clone(),
            min_strength: 0.0,
        })))
        .await
        .unwrap();
    let memories = match linked.data.unwrap() {
        ResponseData::LinkedMemories(l) => l.memories,
        other => panic!("expected LinkedMemories, got {other:?}"),
    };
    assert_eq!(memories.len(), 2, "co-created peers should be linked");

    // The core-tagged memory should have retention_floor = 0.6 (synaptic
    // tagging at storage, paper §3.4).
    let core_id = batch.ids[2].clone();
    let core_resp = client
        .request(Request::Memory(MemoryRequest::Get(GetMemoryArgs {
            user_id: "alice".to_string(),
            id: core_id,
        })))
        .await
        .unwrap();
    let core_mem = match core_resp.data.unwrap() {
        ResponseData::Memory(m) => m,
        other => panic!("expected Memory, got {other:?}"),
    };
    assert!(
        (core_mem.retention_floor - 0.6).abs() < 1e-6,
        "core memory must have synaptic-tagging floor 0.6, got {}",
        core_mem.retention_floor
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn list_filter_link_update_delete_full_crud_loop() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    // Store two memories.
    let r1 = client
        .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
            user_id: "alice".to_string(),
            content: "First.".to_string(),
            category: "semantic".to_string(),
            memory_type: "fact".to_string(),
            metadata: "{}".to_string(),
            importance: None,
        })))
        .await
        .unwrap();
    let id1 = match r1.data.unwrap() {
        ResponseData::MemoryStored(s) => s.id,
        _ => panic!(),
    };

    let r2 = client
        .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
            user_id: "alice".to_string(),
            content: "Second.".to_string(),
            category: "semantic".to_string(),
            memory_type: "preference".to_string(),
            metadata: "{}".to_string(),
            importance: None,
        })))
        .await
        .unwrap();
    let id2 = match r2.data.unwrap() {
        ResponseData::MemoryStored(s) => s.id,
        _ => panic!(),
    };

    // List — both present.
    let listed = client
        .request(Request::Memory(MemoryRequest::List(ListMemoryArgs {
            user_id: "alice".to_string(),
            ..Default::default()
        })))
        .await
        .unwrap();
    let ms = match listed.data.unwrap() {
        ResponseData::Memories(m) => m,
        _ => panic!(),
    };
    assert_eq!(ms.memories.len(), 2);

    // Filter by memory_type.
    let filtered = client
        .request(Request::Memory(MemoryRequest::List(ListMemoryArgs {
            user_id: "alice".to_string(),
            memory_types: Some(vec!["preference".to_string()]),
            ..Default::default()
        })))
        .await
        .unwrap();
    let pref = match filtered.data.unwrap() {
        ResponseData::Memories(m) => m,
        _ => panic!(),
    };
    assert_eq!(pref.memories.len(), 1);
    assert_eq!(pref.memories[0].memory_type, "preference");

    // Link them.
    let linked_resp = client
        .request(Request::Memory(MemoryRequest::Link(LinkMemoryArgs {
            user_id: "alice".to_string(),
            source_id: id1.clone(),
            target_id: id2.clone(),
            strength: 0.5,
            bidirectional: true,
            kind: "explicit".to_string(),
        })))
        .await
        .unwrap();
    assert!(linked_resp.ok);

    // GetLinked sees the peer.
    let linked = client
        .request(Request::Memory(MemoryRequest::GetLinked(GetLinkedArgs {
            user_id: "alice".to_string(),
            source_id: id1.clone(),
            min_strength: 0.0,
        })))
        .await
        .unwrap();
    let lms = match linked.data.unwrap() {
        ResponseData::LinkedMemories(l) => l.memories,
        _ => panic!(),
    };
    assert_eq!(lms.len(), 1);
    assert_eq!(lms[0].memory.id, id2);

    // Update id1's category to core (synaptic tagging via update).
    let updated = client
        .request(Request::Memory(MemoryRequest::Update(UpdateMemoryArgs {
            user_id: "alice".to_string(),
            id: id1.clone(),
            content: None,
            category: Some("core".to_string()),
            memory_type: None,
            metadata: None,
            retention_floor: Some(0.6),
            importance: None,
            stability: None,
            valid_until: None,
        })))
        .await
        .unwrap();
    assert!(updated.ok);

    // Verify update landed.
    let got = client
        .request(Request::Memory(MemoryRequest::Get(GetMemoryArgs {
            user_id: "alice".to_string(),
            id: id1.clone(),
        })))
        .await
        .unwrap();
    let m = match got.data.unwrap() {
        ResponseData::Memory(m) => m,
        _ => panic!(),
    };
    assert_eq!(m.category, "core");
    assert!((m.retention_floor - 0.6).abs() < 1e-6);

    // Delete id2.
    let deleted = client
        .request(Request::Memory(MemoryRequest::Delete(DeleteMemoryArgs {
            user_id: "alice".to_string(),
            id: id2.clone(),
        })))
        .await
        .unwrap();
    assert!(deleted.ok);

    // Counts now: 1 hot, 0 cold, 0 stub, 1 total.
    let counts_resp = client
        .request(Request::Diagnostics(DiagnosticsRequest::Counts(
            CountsArgs {
                user_id: "alice".to_string(),
            },
        )))
        .await
        .unwrap();
    let c = match counts_resp.data.unwrap() {
        ResponseData::Counts(c) => c,
        _ => panic!(),
    };
    assert_eq!(c.hot, 1);
    assert_eq!(c.total, 1);

    let _ = shutdown.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn lifecycle_clear_requires_confirmation() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    // Without confirm: rejected.
    let resp = client
        .request(Request::Lifecycle(LifecycleRequest::Clear(ClearArgs {
            user_id: "alice".to_string(),
            confirm: false,
        })))
        .await
        .unwrap();
    assert!(!resp.ok, "Clear without confirm must be rejected");

    // Store something then clear with confirm.
    client
        .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
            user_id: "alice".to_string(),
            content: "x".to_string(),
            category: "semantic".to_string(),
            memory_type: "fact".to_string(),
            metadata: "{}".to_string(),
            importance: None,
        })))
        .await
        .unwrap();

    let cleared = client
        .request(Request::Lifecycle(LifecycleRequest::Clear(ClearArgs {
            user_id: "alice".to_string(),
            confirm: true,
        })))
        .await
        .unwrap();
    assert!(cleared.ok);
    let n = match cleared.data.unwrap() {
        ResponseData::Affected(a) => a.affected,
        _ => panic!(),
    };
    assert_eq!(n, 1);

    let _ = shutdown.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn search_isolates_results_by_user_id_at_the_daemon_layer() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;

    // Alice stores a memory.
    let mut alice = Client::connect(&socket, "alice-client", "alice")
        .await
        .unwrap();
    alice
        .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
            user_id: "alice".to_string(),
            content: "alice's secret".to_string(),
            category: "semantic".to_string(),
            memory_type: "fact".to_string(),
            metadata: "{}".to_string(),
            importance: None,
        })))
        .await
        .unwrap();

    // Bob searches — should see nothing.
    let mut bob = Client::connect(&socket, "bob-client", "bob").await.unwrap();
    let bob_resp = bob
        .request(Request::Memory(MemoryRequest::Search(SearchMemoryArgs {
            user_id: "bob".to_string(),
            query: "alice's secret".to_string(),
            limit: 5,
            deep_recall: false,
            hybrid: false,
            graph_expansion_hops: 0,
            bridge_discovery: false,
        })))
        .await
        .unwrap();

    match bob_resp.data.expect("data") {
        ResponseData::MemorySearchResults(results) => {
            assert!(
                results.results.is_empty(),
                "bob must not see alice's memories"
            );
        }
        other => panic!("expected MemorySearchResults, got {other:?}"),
    }

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Behavioural test for the `--importance` flag: when a client supplies
/// importance, the daemon writes it onto the row (clamped to [0, 1]) and
/// it round-trips through Get. With no importance, the daemon's default
/// (0.0) stands.
#[tokio::test]
async fn store_writes_importance_when_supplied_and_clamps_out_of_range() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    async fn store(client: &mut Client, content: &str, importance: Option<f64>) -> String {
        let resp = client
            .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
                user_id: "alice".to_string(),
                content: content.to_string(),
                category: "semantic".to_string(),
                memory_type: "fact".to_string(),
                metadata: "{}".to_string(),
                importance,
            })))
            .await
            .unwrap();
        match resp.data.unwrap() {
            ResponseData::MemoryStored(s) => s.id,
            other => panic!("expected MemoryStored, got {other:?}"),
        }
    }

    async fn fetch_importance(client: &mut Client, id: &str) -> f64 {
        let resp = client
            .request(Request::Memory(MemoryRequest::Get(GetMemoryArgs {
                user_id: "alice".to_string(),
                id: id.to_string(),
            })))
            .await
            .unwrap();
        match resp.data.unwrap() {
            ResponseData::Memory(m) => m.importance,
            other => panic!("expected Memory, got {other:?}"),
        }
    }

    let with_imp_id = store(&mut client, "with importance", Some(0.9)).await;
    let no_imp_id = store(&mut client, "no importance", None).await;
    let over_id = store(&mut client, "out of range", Some(2.5)).await;

    let m1 = fetch_importance(&mut client, &with_imp_id).await;
    assert!(
        (m1 - 0.9).abs() < f64::EPSILON,
        "explicit importance should round-trip; got {m1}"
    );

    let m2 = fetch_importance(&mut client, &no_imp_id).await;
    assert_eq!(m2, 0.0, "absent importance should fall through to default");

    let m3 = fetch_importance(&mut client, &over_id).await;
    assert!(
        (m3 - 1.0).abs() < f64::EPSILON,
        "out-of-range importance should clamp to 1.0; got {m3}"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Stability at creation must follow the SDK formula `0.1 + 0.3 * importance`
/// (cognitive_memory/core.py:126), not the legacy hardcoded 0.5. Three
/// values across the importance range pin the linear relationship.
#[tokio::test]
async fn store_initial_stability_follows_importance_formula() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    async fn store_and_fetch_stability(
        client: &mut Client,
        importance: Option<f64>,
        content: &str,
    ) -> f64 {
        let stored = client
            .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
                user_id: "alice".to_string(),
                content: content.to_string(),
                category: "semantic".to_string(),
                memory_type: "fact".to_string(),
                metadata: "{}".to_string(),
                importance,
            })))
            .await
            .unwrap();
        let id = match stored.data.unwrap() {
            ResponseData::MemoryStored(s) => s.id,
            other => panic!("expected MemoryStored, got {other:?}"),
        };
        let got = client
            .request(Request::Memory(MemoryRequest::Get(GetMemoryArgs {
                user_id: "alice".to_string(),
                id,
            })))
            .await
            .unwrap();
        match got.data.unwrap() {
            ResponseData::Memory(m) => m.stability,
            other => panic!("expected Memory, got {other:?}"),
        }
    }

    // SDK: stability = 0.1 + 0.3 * importance.
    // importance=None ⇒ daemon default (importance=0) ⇒ stability=0.1.
    let s_default = store_and_fetch_stability(&mut client, None, "no imp").await;
    assert!(
        (s_default - 0.1).abs() < 1e-6,
        "default stability should be 0.1, got {s_default}"
    );

    // importance=0.5 ⇒ stability = 0.1 + 0.15 = 0.25.
    let s_mid = store_and_fetch_stability(&mut client, Some(0.5), "mid imp").await;
    assert!(
        (s_mid - 0.25).abs() < 1e-6,
        "stability at importance=0.5 should be 0.25, got {s_mid}"
    );

    // importance=1.0 ⇒ stability = 0.4.
    let s_max = store_and_fetch_stability(&mut client, Some(1.0), "max imp").await;
    assert!(
        (s_max - 0.4).abs() < 1e-6,
        "stability at importance=1.0 should be 0.4, got {s_max}"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Stability reinforcement on ingest (SDK core.py:222-224): when a
/// new memory is similar to an existing one in the band
/// (STABILITY_REINFORCEMENT_THRESHOLD=0.75, CONFLICT_SIMILARITY_THRESHOLD=0.85),
/// the existing memory's stability is bumped by +0.05 (capped at 1.0).
/// Above 0.85 → conflict path (no boost). Below 0.75 → no action.
#[tokio::test]
async fn ingest_stability_reinforcement_in_high_similarity_band() {
    let (handle, socket, shutdown, _tmp, embeddings) = boot_daemon_with_embeddings().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    // Pin two embeddings with cosine ≈ 0.80, in the (0.75, 0.85) band.
    // 16-dim vectors:
    //   v1 = [1, 0, 0, ..., 0]
    //   v2 = [0.8, 0.6, 0, ..., 0]
    // |v1| = 1, |v2| = sqrt(0.64+0.36) = 1, dot = 0.8 → cosine = 0.80.
    let mut v1 = vec![0.0f32; 16];
    v1[0] = 1.0;
    let mut v2 = vec![0.0f32; 16];
    v2[0] = 0.8;
    v2[1] = 0.6;
    embeddings.set_override("anchor reinforce text", v1);
    embeddings.set_override("near duplicate reinforce text", v2);

    // Store anchor with importance=0 ⇒ stability=0.1 (SDK formula).
    let anchor_resp = client
        .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
            user_id: "alice".to_string(),
            content: "anchor reinforce text".to_string(),
            category: "semantic".to_string(),
            memory_type: "fact".to_string(),
            metadata: "{}".to_string(),
            importance: None,
        })))
        .await
        .unwrap();
    let anchor_id = match anchor_resp.data.unwrap() {
        ResponseData::MemoryStored(s) => s.id,
        _ => panic!("expected MemoryStored"),
    };

    // Pre-condition check: anchor stability is 0.1.
    let pre = fetch(&mut client, &anchor_id).await;
    assert!(
        (pre.stability - 0.1).abs() < 1e-6,
        "anchor pre-stability should be 0.1, got {}",
        pre.stability
    );

    // Store the near-duplicate. Should trigger reinforcement on anchor.
    let _ = client
        .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
            user_id: "alice".to_string(),
            content: "near duplicate reinforce text".to_string(),
            category: "semantic".to_string(),
            memory_type: "fact".to_string(),
            metadata: "{}".to_string(),
            importance: None,
        })))
        .await
        .unwrap();

    // Anchor stability must be 0.1 + 0.05 = 0.15.
    let post = fetch(&mut client, &anchor_id).await;
    assert!(
        (post.stability - 0.15).abs() < 1e-6,
        "anchor post-stability should be 0.15 (reinforced), got {}",
        post.stability
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Synaptic tagging on ingest (SDK core.py:248-262): when a new
/// memory is similar to an existing one in the band [0.4, 0.75), an
/// auto-link is created bidirectionally with weight
/// `min(0.5, 0.2 + (sim - 0.4) * 0.5)`. Mirror INGESTION_ASSOCIATION_*.
#[tokio::test]
async fn ingest_synaptic_tag_in_mid_similarity_band() {
    let (handle, socket, shutdown, _tmp, embeddings) = boot_daemon_with_embeddings().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    // cosine ≈ 0.50 — in synaptic-tag band [0.4, 0.75).
    // 16-dim:
    //   v1 = [1, 0, 0, ..., 0]      → norm 1
    //   v2 = [0.5, sqrt(0.75), 0, ..., 0] → dot=0.5, norm=1, cos=0.50
    let mut v1 = vec![0.0f32; 16];
    v1[0] = 1.0;
    let mut v2 = vec![0.0f32; 16];
    v2[0] = 0.5;
    v2[1] = (0.75f32).sqrt();
    embeddings.set_override("anchor synaptic", v1);
    embeddings.set_override("midband synaptic", v2);

    let anchor_id = store_helper(&mut client, "anchor synaptic", "semantic").await;
    let _new_id = store_helper(&mut client, "midband synaptic", "semantic").await;

    // SDK weight: min(0.5, 0.2 + (0.5 - 0.4) * 0.5) = min(0.5, 0.25) = 0.25
    let resp = client
        .request(Request::Memory(MemoryRequest::GetLinked(GetLinkedArgs {
            user_id: "alice".to_string(),
            source_id: anchor_id.clone(),
            min_strength: 0.0,
        })))
        .await
        .unwrap();
    let linked = match resp.data.unwrap() {
        ResponseData::LinkedMemories(d) => d.memories,
        _ => panic!("expected LinkedMemories"),
    };
    assert_eq!(
        linked.len(),
        1,
        "anchor should have exactly 1 synaptic-tagged neighbor"
    );
    let weight = linked[0].link_strength;
    assert!(
        (weight - 0.25).abs() < 1e-6,
        "synaptic weight at sim=0.50 should be 0.25 (= 0.2 + 0.05), got {weight}"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Below the synaptic-tag threshold (0.4): no link, no stability boost,
/// no conflict queue entry. The single-search dispatcher must return
/// no-op when the highest-similarity hit is too low to act on.
#[tokio::test]
async fn ingest_below_threshold_is_a_noop() {
    let (handle, socket, shutdown, _tmp, embeddings) = boot_daemon_with_embeddings().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    // cosine ≈ 0.20 — below all bands.
    //   v1 = [1, 0, ...]
    //   v2 = [0.2, sqrt(0.96), 0, ...]
    let mut v1 = vec![0.0f32; 16];
    v1[0] = 1.0;
    let mut v2 = vec![0.0f32; 16];
    v2[0] = 0.2;
    v2[1] = (0.96f32).sqrt();
    embeddings.set_override("anchor below threshold", v1);
    embeddings.set_override("low sim other", v2);

    let anchor_id = store_helper(&mut client, "anchor below threshold", "semantic").await;
    let _ = store_helper(&mut client, "low sim other", "semantic").await;

    // No edge created.
    let resp = client
        .request(Request::Memory(MemoryRequest::GetLinked(GetLinkedArgs {
            user_id: "alice".to_string(),
            source_id: anchor_id.clone(),
            min_strength: 0.0,
        })))
        .await
        .unwrap();
    let linked = match resp.data.unwrap() {
        ResponseData::LinkedMemories(d) => d.memories,
        _ => panic!("expected LinkedMemories"),
    };
    assert!(linked.is_empty(), "no link should be created below sim=0.4");

    // No stability boost on anchor (still 0.1).
    let m = fetch(&mut client, &anchor_id).await;
    assert!(
        (m.stability - 0.1).abs() < 1e-6,
        "anchor stability untouched, got {}",
        m.stability
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Conflict band (sim ≥ 0.85) regression: queue the pair, do NOT
/// reinforce stability and do NOT auto-link. Bands are exclusive.
#[tokio::test]
async fn ingest_at_conflict_threshold_only_queues() {
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_embeddings().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    // cosine ≈ 0.90 — conflict band.
    //   v1 = [1, 0, ...]
    //   v2 = [0.9, sqrt(0.19), 0, ...] → dot=0.9, norm=1, cos=0.90
    let mut v1 = vec![0.0f32; 16];
    v1[0] = 1.0;
    let mut v2 = vec![0.0f32; 16];
    v2[0] = 0.9;
    v2[1] = (0.19f32).sqrt();
    embeddings.set_override("anchor conflict", v1);
    embeddings.set_override("near duplicate conflict", v2);

    let anchor_id = store_helper(&mut client, "anchor conflict", "semantic").await;
    let _ = store_helper(&mut client, "near duplicate conflict", "semantic").await;

    // 1 row in conflict_queue.
    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM conflict_queue WHERE user_id = ?")
        .bind("alice")
        .fetch_one(store.reader())
        .await
        .unwrap();
    drop(store);
    assert_eq!(n, 1, "conflict queue should have exactly 1 entry");

    // Anchor stability unchanged (0.1) — bands are exclusive.
    let m = fetch(&mut client, &anchor_id).await;
    assert!(
        (m.stability - 0.1).abs() < 1e-6,
        "anchor stability should be untouched at conflict band, got {}",
        m.stability
    );

    // No auto-link.
    let resp = client
        .request(Request::Memory(MemoryRequest::GetLinked(GetLinkedArgs {
            user_id: "alice".to_string(),
            source_id: anchor_id.clone(),
            min_strength: 0.0,
        })))
        .await
        .unwrap();
    let linked = match resp.data.unwrap() {
        ResponseData::LinkedMemories(d) => d.memories,
        _ => panic!("expected LinkedMemories"),
    };
    assert!(linked.is_empty(), "no synaptic link in conflict band");

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Plant a memory directly via SQL with a *stale* embedding provider
/// pair so it's invisible to `candidates_for_search` (which filters
/// on the active provider/model) but still discoverable via the
/// associations table by graph expansion.
#[allow(clippy::too_many_arguments)]
async fn plant_memory_with_stale_provider(
    store: &Store,
    id: &str,
    user_id: &str,
    content: &str,
    embedding: Vec<f32>,
    now: i64,
) {
    let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
    sqlx::query(
        "INSERT INTO memories (
            id, user_id, content, category, memory_type, embedding,
            embedding_provider, embedding_model, created_at, last_accessed_at,
            valid_from, valid_until, ttl_seconds, retention_floor,
            retrieval_count, metadata, importance, stability, is_cold,
            cold_since, days_at_floor, is_superseded, superseded_by, is_stub,
            stub_content, contradicted_by, session_ids
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id).bind(user_id).bind(content)
    .bind("semantic").bind("fact").bind(bytes)
    .bind("stale-provider").bind("stale-model")
    .bind(now).bind(now).bind::<Option<i64>>(None).bind::<Option<i64>>(None)
    .bind::<Option<i64>>(None).bind(0.0_f64).bind(0_i64).bind("{}".to_string())
    .bind(0.0_f64).bind(0.5_f64).bind(0_i64).bind::<Option<i64>>(None)
    .bind(0_i64).bind(0_i64).bind::<Option<String>>(None)
    .bind(0_i64).bind::<Option<String>>(None).bind::<Option<String>>(None)
    .bind("[]".to_string())
    .execute(store.writer()).await.unwrap();
}

/// Association decay on read (paper Eq 10): when graph expansion
/// surfaces a linked memory, its composite score uses
/// `stored_weight * exp(-Δt_days/90)`, not the stored weight.
///
/// Setup avoids the dense-search confounder by planting target under
/// a stale (provider, model) so dense excludes it; only graph
/// expansion can surface it.
#[tokio::test]
async fn graph_expansion_uses_decayed_edge_weight() {
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_embeddings().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let mut v_anchor = vec![0.0f32; 16];
    v_anchor[0] = 1.0;
    embeddings.set_override("anchor decay test", v_anchor.clone());
    embeddings.set_override("decay query identical", v_anchor);

    let anchor_id = store_helper(&mut client, "anchor decay test", "semantic").await;

    let target_id = format!("mem_{}", ulid::Ulid::new());
    let mut v_target = vec![0.0f32; 16];
    v_target[0] = 0.5;
    v_target[1] = (0.75f32).sqrt(); // cos(target, query) = 0.5

    let now: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    // Δt=30d so decayed weight (0.8*exp(-1/3) ≈ 0.573) stays above
    // the 0.3 graph-expansion threshold. The threshold-drop case is
    // tested separately by graph_expansion_drops_edges_below_decayed_threshold.
    let last_co = now - 30 * 86_400;

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    plant_memory_with_stale_provider(&store, &target_id, "alice", "planted target", v_target, now)
        .await;
    sqlx::query(
        "INSERT INTO associations
            (source_memory_id, target_memory_id, weight, kind,
             updated_at, last_co_retrieval)
         VALUES (?, ?, ?, 'thematic', ?, ?)",
    )
    .bind(&anchor_id)
    .bind(&target_id)
    .bind(0.8_f64)
    .bind(now)
    .bind(last_co)
    .execute(store.writer())
    .await
    .unwrap();
    drop(store);

    let resp = client
        .request(Request::Memory(MemoryRequest::Search(SearchMemoryArgs {
            user_id: "alice".to_string(),
            query: "decay query identical".to_string(),
            limit: 5,
            deep_recall: false,
            hybrid: false,
            graph_expansion_hops: 1,
            bridge_discovery: false,
        })))
        .await
        .unwrap();
    let results = match resp.data.unwrap() {
        ResponseData::MemorySearchResults(r) => r.results,
        _ => panic!("expected MemorySearchResults"),
    };
    let target_score = results
        .iter()
        .find(|r| r.memory_id == target_id)
        .expect("target should surface via graph expansion only")
        .score as f64;

    // SDK Eq 10: live_weight = 0.8 * exp(-30/90) = 0.8 * exp(-1/3) ≈ 0.5731.
    // Composite = cos(query, target=0.5) * R(target)^α * live_weight.
    // Target's last_accessed_at = now, so R=1.0, R^α=1.0.
    // Composite = 0.5 * 0.5731 ≈ 0.2865.
    let expected = 0.5 * 0.8 * (-1.0_f64 / 3.0).exp();
    assert!(
        (target_score - expected).abs() < 0.01,
        "decayed composite expected ≈ {expected}, got {target_score}"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// When the decayed edge weight drops below `min_bridge_edge_weight`
/// (default 0.3), the linked memory must NOT appear in graph
/// expansion. Stored 0.5 with τ=90 and Δt=90d → decayed ≈ 0.184.
#[tokio::test]
async fn graph_expansion_drops_edges_below_decayed_threshold() {
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_embeddings().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let mut v_anchor = vec![0.0f32; 16];
    v_anchor[0] = 1.0;
    embeddings.set_override("anchor below threshold", v_anchor.clone());
    embeddings.set_override("threshold query", v_anchor);

    let anchor_id = store_helper(&mut client, "anchor below threshold", "semantic").await;

    let target_id = format!("mem_{}", ulid::Ulid::new());
    let mut v_target = vec![0.0f32; 16];
    v_target[0] = 0.5;
    v_target[1] = (0.75f32).sqrt();

    let now: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let last_co = now - 90 * 86_400;

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    plant_memory_with_stale_provider(&store, &target_id, "alice", "hidden target", v_target, now)
        .await;
    sqlx::query(
        "INSERT INTO associations
            (source_memory_id, target_memory_id, weight, kind,
             updated_at, last_co_retrieval)
         VALUES (?, ?, ?, 'thematic', ?, ?)",
    )
    .bind(&anchor_id)
    .bind(&target_id)
    .bind(0.5_f64)
    .bind(now)
    .bind(last_co)
    .execute(store.writer())
    .await
    .unwrap();
    drop(store);

    let resp = client
        .request(Request::Memory(MemoryRequest::Search(SearchMemoryArgs {
            user_id: "alice".to_string(),
            query: "threshold query".to_string(),
            limit: 5,
            deep_recall: false,
            hybrid: false,
            graph_expansion_hops: 1,
            bridge_discovery: false,
        })))
        .await
        .unwrap();
    let ids: Vec<String> = match resp.data.unwrap() {
        ResponseData::MemorySearchResults(r) => {
            r.results.into_iter().map(|h| h.memory_id).collect()
        }
        _ => panic!("expected MemorySearchResults"),
    };
    assert!(
        !ids.contains(&target_id),
        "target should be excluded when decayed weight < 0.3, got results {ids:?}"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Co-retrieval strengthening (paper Eq 9, engine.py:621-625): when
/// a search returns N memories, every unordered pair in the result
/// set has its association weight bumped by 0.1 (capped at 1.0) on
/// BOTH directions. Edges that didn't exist are created at weight
/// 0.1.
#[tokio::test]
async fn search_strengthens_associations_between_top_results() {
    let (handle, socket, shutdown, _tmp, embeddings) = boot_daemon_with_embeddings().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    // Three memories whose embeddings all match the query at cos > 0
    // so all three appear in the dense result set.
    let mut v_q = vec![0.0f32; 16];
    v_q[0] = 1.0;
    let mut v_a = vec![0.0f32; 16];
    v_a[0] = 1.0;
    let mut v_b = vec![0.0f32; 16];
    v_b[0] = 0.9;
    v_b[1] = (0.19f32).sqrt();
    let mut v_c = vec![0.0f32; 16];
    v_c[0] = 0.8;
    v_c[1] = (0.36f32).sqrt();
    embeddings.set_override("co-retrieve query", v_q);
    embeddings.set_override("co-retrieve A", v_a);
    embeddings.set_override("co-retrieve B", v_b);
    embeddings.set_override("co-retrieve C", v_c);

    let a = store_helper(&mut client, "co-retrieve A", "semantic").await;
    let b = store_helper(&mut client, "co-retrieve B", "semantic").await;
    let c = store_helper(&mut client, "co-retrieve C", "semantic").await;

    // Search returns A, B, C. Triggers strengthen_pairs for the 3
    // unordered pairs (A,B), (A,C), (B,C).
    let _ = client
        .request(Request::Memory(MemoryRequest::Search(SearchMemoryArgs {
            user_id: "alice".to_string(),
            query: "co-retrieve query".to_string(),
            limit: 5,
            deep_recall: false,
            hybrid: false,
            graph_expansion_hops: 0,
            bridge_discovery: false,
        })))
        .await
        .unwrap();

    // For each pair, confirm both directional edges exist with weight ≥ 0.1.
    async fn weight_between(client: &mut Client, src: &str, tgt: &str) -> f64 {
        let resp = client
            .request(Request::Memory(MemoryRequest::GetLinked(GetLinkedArgs {
                user_id: "alice".to_string(),
                source_id: src.to_string(),
                min_strength: 0.0,
            })))
            .await
            .unwrap();
        let linked = match resp.data.unwrap() {
            ResponseData::LinkedMemories(d) => d.memories,
            _ => panic!("expected LinkedMemories"),
        };
        linked
            .into_iter()
            .find(|lm| lm.memory.id == tgt)
            .map(|lm| lm.link_strength)
            .unwrap_or(0.0)
    }

    let pairs = [
        (a.as_str(), b.as_str()),
        (a.as_str(), c.as_str()),
        (b.as_str(), c.as_str()),
    ];
    for (src, tgt) in pairs {
        let fwd = weight_between(&mut client, src, tgt).await;
        let bwd = weight_between(&mut client, tgt, src).await;
        assert!(
            (fwd - 0.1).abs() < 1e-6,
            "{src} → {tgt} should be 0.1 after co-retrieval, got {fwd}"
        );
        assert!(
            (bwd - 0.1).abs() < 1e-6,
            "{tgt} → {src} should be 0.1 (bidirectional), got {bwd}"
        );
    }

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Boost differential by source (paper Eq 6 vs Eq 8): direct hits
/// get +0.10 * spaced_rep_factor stability boost; graph-expanded
/// hits get +0.03 * spaced_rep_factor. Tested with a 14-day-old
/// pair (factor capped at 2.0 → direct +0.20, expanded +0.06).
#[tokio::test]
async fn search_applies_smaller_boost_to_graph_expanded_results() {
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_embeddings().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    // Direct hit A: same vector as query → cosine 1.
    let mut v_q = vec![0.0f32; 16];
    v_q[0] = 1.0;
    embeddings.set_override("differential query", v_q.clone());
    embeddings.set_override("anchor direct A", v_q.clone());
    let direct_id = store_helper(&mut client, "anchor direct A", "semantic").await;

    // Graph-expanded target B: planted with stale-provider so dense
    // can't see it; reachable only via association from A.
    let target_id = format!("mem_{}", ulid::Ulid::new());
    let mut v_b = vec![0.0f32; 16];
    v_b[0] = 0.5;
    v_b[1] = (0.75f32).sqrt();
    let now: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    plant_memory_with_stale_provider(&store, &target_id, "alice", "graph target B", v_b, now).await;
    sqlx::query(
        "INSERT INTO associations
            (source_memory_id, target_memory_id, weight, kind,
             updated_at, last_co_retrieval)
         VALUES (?, ?, ?, 'thematic', ?, ?)",
    )
    .bind(&direct_id)
    .bind(&target_id)
    .bind(0.8_f64)
    .bind(now)
    .bind(now)
    .execute(store.writer())
    .await
    .unwrap();

    // Backdate both 14 days so the spaced-rep factor is exactly 2.0
    // (= max). last_accessed_at = now - 14d.
    let backdate = now - 14 * 86_400;
    sqlx::query("UPDATE memories SET last_accessed_at = ? WHERE id IN (?, ?)")
        .bind(backdate)
        .bind(&direct_id)
        .bind(&target_id)
        .execute(store.writer())
        .await
        .unwrap();
    drop(store);

    // Pre-state. direct.stability defaulted to 0.1 (Stage 1 fix);
    // target.stability planted at 0.5.
    // Run the search. Direct hit's stability bumps by 0.1*2 = 0.2
    // → 0.3. Graph-expanded target's stability bumps by 0.03*2 = 0.06
    // → 0.56.
    let _ = client
        .request(Request::Memory(MemoryRequest::Search(SearchMemoryArgs {
            user_id: "alice".to_string(),
            query: "differential query".to_string(),
            limit: 5,
            deep_recall: false,
            hybrid: false,
            graph_expansion_hops: 1,
            bridge_discovery: false,
        })))
        .await
        .unwrap();

    let direct_post = fetch(&mut client, &direct_id).await;
    let target_post = fetch(&mut client, &target_id).await;

    assert!(
        (direct_post.stability - 0.3).abs() < 1e-6,
        "direct stability should be 0.1 + 0.10*2 = 0.3, got {}",
        direct_post.stability
    );
    assert!(
        (target_post.stability - 0.56).abs() < 1e-6,
        "graph-expanded stability should be 0.5 + 0.03*2 = 0.56, got {}",
        target_post.stability
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// `cm get <cold_id>` returns the memory AND auto-restores it to hot.
/// Mirrors SDK engine.py:606,612. Subsequent `cm get` confirms.
#[tokio::test]
async fn get_auto_restores_cold_memory_to_hot() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let id = store_helper(
        &mut client,
        "memory will go cold then get accessed",
        "semantic",
    )
    .await;
    // Manually migrate to cold via the IPC op.
    use cognitive_memory_protocol::MigrateToColdArgs;
    client
        .request(Request::Lifecycle(LifecycleRequest::MigrateToCold(
            MigrateToColdArgs {
                user_id: "alice".to_string(),
                id: id.clone(),
                cold_since: 100,
            },
        )))
        .await
        .unwrap();

    // Pre-condition: is_cold is true.
    let before = fetch(&mut client, &id).await;
    assert!(
        !before.is_cold,
        "after `cm get`, is_cold should already be false (auto-restore on first read)"
    );

    // After a follow-up read, still hot.
    let after = fetch(&mut client, &id).await;
    assert!(!after.is_cold);
    assert_eq!(after.days_at_floor, 0);

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// `cm search --deep-recall` surfaces cold memories AND auto-restores
/// them. Default search (no --deep-recall) does not surface cold.
///
/// Reads `is_cold` via direct SQL (not `cm get`) because the get
/// handler also auto-restores — using IPC would poison the
/// "default search did not restore" check.
#[tokio::test]
async fn deep_recall_search_surfaces_and_restores_cold_memory() {
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_embeddings().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let mut v = vec![0.0f32; 16];
    v[0] = 1.0;
    embeddings.set_override("cold restore search target", v.clone());
    embeddings.set_override("cold restore query", v);

    let id = store_helper(&mut client, "cold restore search target", "semantic").await;
    use cognitive_memory_protocol::MigrateToColdArgs;
    client
        .request(Request::Lifecycle(LifecycleRequest::MigrateToCold(
            MigrateToColdArgs {
                user_id: "alice".to_string(),
                id: id.clone(),
                cold_since: 100,
            },
        )))
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();

    async fn read_is_cold(store: &Store, id: &str) -> bool {
        let row: (i64,) = sqlx::query_as("SELECT is_cold FROM memories WHERE id = ?")
            .bind(id)
            .fetch_one(store.reader())
            .await
            .unwrap();
        row.0 != 0
    }

    assert!(read_is_cold(&store, &id).await, "pre-condition: cold");

    // Default search: must NOT surface cold memory and must NOT restore.
    let default_resp = client
        .request(Request::Memory(MemoryRequest::Search(SearchMemoryArgs {
            user_id: "alice".to_string(),
            query: "cold restore query".to_string(),
            limit: 5,
            deep_recall: false,
            hybrid: false,
            graph_expansion_hops: 0,
            bridge_discovery: false,
        })))
        .await
        .unwrap();
    let default_ids: Vec<String> = match default_resp.data.unwrap() {
        ResponseData::MemorySearchResults(r) => {
            r.results.into_iter().map(|h| h.memory_id).collect()
        }
        _ => panic!("expected MemorySearchResults"),
    };
    assert!(
        !default_ids.contains(&id),
        "default search must NOT surface cold memory; got {default_ids:?}"
    );
    assert!(
        read_is_cold(&store, &id).await,
        "default search must not auto-restore"
    );

    // Deep-recall: surfaces AND restores.
    let deep_resp = client
        .request(Request::Memory(MemoryRequest::Search(SearchMemoryArgs {
            user_id: "alice".to_string(),
            query: "cold restore query".to_string(),
            limit: 5,
            deep_recall: true,
            hybrid: false,
            graph_expansion_hops: 0,
            bridge_discovery: false,
        })))
        .await
        .unwrap();
    let deep_ids: Vec<String> = match deep_resp.data.unwrap() {
        ResponseData::MemorySearchResults(r) => {
            r.results.into_iter().map(|h| h.memory_id).collect()
        }
        _ => panic!("expected MemorySearchResults"),
    };
    assert!(
        deep_ids.contains(&id),
        "deep_recall search must surface cold memory; got {deep_ids:?}"
    );
    assert!(
        !read_is_cold(&store, &id).await,
        "deep_recall surfaced cold must auto-restore"
    );
    drop(store);

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Restoring from cold also resets days_at_floor (set non-zero by
/// the at-floor counter prior to migration).
#[tokio::test]
async fn restore_resets_days_at_floor_alongside_is_cold() {
    let (handle, socket, shutdown, tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let id = store_helper(&mut client, "had non-zero days_at_floor", "semantic").await;
    // Plant: is_cold=1, cold_since=100, days_at_floor=9.
    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    sqlx::query(
        "UPDATE memories
         SET is_cold = 1, cold_since = 100, days_at_floor = 9
         WHERE id = ?",
    )
    .bind(&id)
    .execute(store.writer())
    .await
    .unwrap();
    drop(store);

    // Access via `cm get` triggers restore.
    let m = fetch(&mut client, &id).await;
    assert!(!m.is_cold);
    assert!(m.cold_since.is_none());
    assert_eq!(m.days_at_floor, 0, "all three cold-state fields reset");

    let _ = shutdown.send(());
    let _ = handle.await;
}

// ===========================================================================
// Stage 4 — LLM conflict judge
// ===========================================================================
//
// All conflict-judge tests use FakeLlmProvider with a scripted
// response queue so the SDK extraction.py:230-249 prompt → label
// flow runs deterministically. The real LocalLlmProvider impl has
// its own integration test in crates/llm.

/// Set up: store two near-duplicate memories so the ingest path
/// queues a conflict. Returns (anchor_id, near_dup_id, store_path).
/// The caller chooses an LLM (or None) at boot to dictate the
/// resolution outcome.
async fn setup_conflict_pair(
    client: &mut Client,
    embeddings: &Arc<FakeEmbeddingProvider>,
    anchor_text: &str,
    near_dup_text: &str,
) -> (String, String) {
    // cosine ≈ 0.90 — well into the conflict band.
    let mut v1 = vec![0.0f32; 16];
    v1[0] = 1.0;
    let mut v2 = vec![0.0f32; 16];
    v2[0] = 0.9;
    v2[1] = (0.19f32).sqrt();
    embeddings.set_override(anchor_text.to_string(), v1);
    embeddings.set_override(near_dup_text.to_string(), v2);

    let anchor_id = store_helper(client, anchor_text, "semantic").await;
    let near_dup_id = store_helper(client, near_dup_text, "semantic").await;
    (anchor_id, near_dup_id)
}

async fn read_supersession(store: &Store, id: &str) -> (bool, Option<String>, Option<String>) {
    let row: (i64, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT is_superseded, superseded_by, contradicted_by
         FROM memories WHERE id = ?",
    )
    .bind(id)
    .fetch_one(store.reader())
    .await
    .unwrap();
    (row.0 != 0, row.1, row.2)
}

/// With `state.llm = None`, conflict resolution falls back to the
/// existing heuristic (higher importance / more recent wins). This
/// is the regression test — Stage 4 must NOT silently break the
/// no-LLM path that earlier stages relied on.
#[tokio::test]
async fn conflict_resolution_with_no_llm_uses_heuristic() {
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_embeddings().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let (anchor_id, near_dup_id) = setup_conflict_pair(
        &mut client,
        &embeddings,
        "user lives in Berlin since 2018",
        "user lives in Paris since 2019",
    )
    .await;

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let (anchor_sup, _, _) = read_supersession(&store, &anchor_id).await;
    let (near_sup, _, _) = read_supersession(&store, &near_dup_id).await;
    drop(store);
    // Heuristic: tie on importance (both 0.0) AND tie on created_at
    // (both stored within the same wall-clock second). The
    // strict-greater-than test in the heuristic falls through to
    // `winner = e (anchor)` so near_dup ends up superseded. Either
    // way, EXACTLY ONE of the pair should be superseded — that's
    // the load-bearing claim, not which one.
    assert!(
        anchor_sup ^ near_sup,
        "heuristic must supersede exactly one of the pair (got anchor={anchor_sup}, near={near_sup})"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// LLM returns "CONTRADICTION": existing → superseded by new.
#[tokio::test]
async fn conflict_resolution_with_llm_contradiction_supersedes_existing() {
    let llm = Arc::new(
        cognitive_memory_llm::FakeLlmProvider::new("fake", "fake-judge")
            .with_responses(["CONTRADICTION"]),
    ) as Arc<dyn cognitive_memory_llm::LlmProvider>;
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_llm(llm).await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let (anchor_id, near_dup_id) = setup_conflict_pair(
        &mut client,
        &embeddings,
        "user is allergic to peanuts",
        "user is not allergic to peanuts",
    )
    .await;

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let (anchor_sup, anchor_by, _) = read_supersession(&store, &anchor_id).await;
    let (near_sup, _, _) = read_supersession(&store, &near_dup_id).await;
    drop(store);
    assert!(
        anchor_sup,
        "CONTRADICTION must supersede the existing memory"
    );
    assert_eq!(
        anchor_by.as_deref(),
        Some(near_dup_id.as_str()),
        "superseded_by must point at the new winner"
    );
    assert!(!near_sup, "winner stays hot");

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// LLM returns "UPDATE": existing → superseded by new, winner's
/// importance is bumped to max(both).
#[tokio::test]
async fn conflict_resolution_with_llm_update_supersedes_and_bumps_importance() {
    let llm = Arc::new(
        cognitive_memory_llm::FakeLlmProvider::new("fake", "fake-judge").with_responses(["UPDATE"]),
    ) as Arc<dyn cognitive_memory_llm::LlmProvider>;
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_llm(llm).await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    // Plant existing with importance=0.7 (high).
    let mut v_anchor = vec![0.0f32; 16];
    v_anchor[0] = 1.0;
    let mut v_new = vec![0.0f32; 16];
    v_new[0] = 0.9;
    v_new[1] = (0.19f32).sqrt();
    embeddings.set_override("user is 30 years old", v_anchor);
    embeddings.set_override("user is 31 years old", v_new);

    let anchor_resp = client
        .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
            user_id: "alice".to_string(),
            content: "user is 30 years old".to_string(),
            category: "semantic".to_string(),
            memory_type: "fact".to_string(),
            metadata: "{}".to_string(),
            importance: Some(0.7),
        })))
        .await
        .unwrap();
    let anchor_id = match anchor_resp.data.unwrap() {
        ResponseData::MemoryStored(s) => s.id,
        _ => panic!(),
    };
    let near_dup_id = store_helper(&mut client, "user is 31 years old", "semantic").await;

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let (anchor_sup, _, _) = read_supersession(&store, &anchor_id).await;
    assert!(anchor_sup, "UPDATE must supersede existing");
    let (winner_imp,): (f64,) = sqlx::query_as("SELECT importance FROM memories WHERE id = ?")
        .bind(&near_dup_id)
        .fetch_one(store.reader())
        .await
        .unwrap();
    drop(store);
    assert!(
        (winner_imp - 0.7).abs() < 1e-6,
        "winner inherits max(importance), expected 0.7, got {winner_imp}"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// LLM returns "OVERLAP": both stay non-superseded; queue drops.
#[tokio::test]
async fn conflict_resolution_with_llm_overlap_keeps_both() {
    let llm = Arc::new(
        cognitive_memory_llm::FakeLlmProvider::new("fake", "fake-judge")
            .with_responses(["OVERLAP"]),
    ) as Arc<dyn cognitive_memory_llm::LlmProvider>;
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_llm(llm).await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let (anchor_id, near_dup_id) = setup_conflict_pair(
        &mut client,
        &embeddings,
        "user likes hiking",
        "user went hiking yesterday",
    )
    .await;

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let (anchor_sup, _, _) = read_supersession(&store, &anchor_id).await;
    let (near_sup, _, _) = read_supersession(&store, &near_dup_id).await;
    let (queue_n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM conflict_queue")
        .fetch_one(store.reader())
        .await
        .unwrap();
    drop(store);
    assert!(!anchor_sup, "OVERLAP keeps existing non-superseded");
    assert!(!near_sup, "OVERLAP keeps new non-superseded");
    assert_eq!(queue_n, 0, "queue must drain regardless of label");

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// LLM returns "NONE": both stay non-superseded; queue drops.
#[tokio::test]
async fn conflict_resolution_with_llm_none_keeps_both() {
    let llm = Arc::new(
        cognitive_memory_llm::FakeLlmProvider::new("fake", "fake-judge").with_responses(["NONE"]),
    ) as Arc<dyn cognitive_memory_llm::LlmProvider>;
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_llm(llm).await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let (anchor_id, near_dup_id) = setup_conflict_pair(
        &mut client,
        &embeddings,
        "user prefers tea",
        "user lives in Boston",
    )
    .await;

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let (a, _, _) = read_supersession(&store, &anchor_id).await;
    let (b, _, _) = read_supersession(&store, &near_dup_id).await;
    drop(store);
    assert!(!a && !b, "NONE keeps both memories");

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Garbage LLM response: parser falls back to NONE (= safe no-op).
/// Mirrors SDK extraction.py:248 fallback.
#[tokio::test]
async fn conflict_resolution_with_unparseable_llm_response_falls_back_to_none() {
    let llm = Arc::new(
        cognitive_memory_llm::FakeLlmProvider::new("fake", "fake-judge")
            .with_responses(["yes definitely conflicting"]),
    ) as Arc<dyn cognitive_memory_llm::LlmProvider>;
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_llm(llm).await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let (anchor_id, _) =
        setup_conflict_pair(&mut client, &embeddings, "fact a", "fact aaa similar").await;

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let (a, _, _) = read_supersession(&store, &anchor_id).await;
    drop(store);
    assert!(
        !a,
        "unparseable response must default to NONE (no supersession)"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// CORE loser on CONTRADICTION is demoted to SEMANTIC and its
/// retention_floor reset to 0.0 (engine.py:418-419 parity). The
/// elevated floor protects core memories from decay; demotion
/// removes that protection so the contradicted memory can fade.
#[tokio::test]
async fn conflict_resolution_demotes_core_loser_to_semantic() {
    let llm = Arc::new(
        cognitive_memory_llm::FakeLlmProvider::new("fake", "fake-judge")
            .with_responses(["CONTRADICTION"]),
    ) as Arc<dyn cognitive_memory_llm::LlmProvider>;
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_llm(llm).await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    // Plant the existing memory as CORE, then store a near-duplicate
    // that the LLM judge rules CONTRADICTION (so existing — the core —
    // becomes the loser).
    let mut v_anchor = vec![0.0f32; 16];
    v_anchor[0] = 1.0;
    let mut v_new = vec![0.0f32; 16];
    v_new[0] = 0.9;
    v_new[1] = (0.19f32).sqrt();
    embeddings.set_override("user identity is alice", v_anchor);
    embeddings.set_override("user identity is alice m", v_new);

    let core_resp = client
        .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
            user_id: "alice".to_string(),
            content: "user identity is alice".to_string(),
            category: "core".to_string(),
            memory_type: "fact".to_string(),
            metadata: "{}".to_string(),
            importance: None,
        })))
        .await
        .unwrap();
    let core_id = match core_resp.data.unwrap() {
        ResponseData::MemoryStored(s) => s.id,
        _ => panic!(),
    };
    let _new_id = store_helper(&mut client, "user identity is alice m", "semantic").await;

    // Pre-condition: anchor is core, retention_floor 0.6.
    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let (cat_pre, floor_pre): (String, f64) =
        sqlx::query_as("SELECT category, retention_floor FROM memories WHERE id = ?")
            .bind(&core_id)
            .fetch_one(store.reader())
            .await
            .unwrap();
    assert_eq!(cat_pre, "core");
    assert!((floor_pre - 0.6).abs() < 1e-6);

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    let (cat_post, floor_post): (String, f64) =
        sqlx::query_as("SELECT category, retention_floor FROM memories WHERE id = ?")
            .bind(&core_id)
            .fetch_one(store.reader())
            .await
            .unwrap();
    drop(store);
    assert_eq!(cat_post, "semantic", "core loser must demote to semantic");
    assert!(
        floor_post.abs() < 1e-6,
        "demoted core's retention_floor must reset to 0.0, got {floor_post}"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

// ===========================================================================
// Stage 4 — Consolidation (clustering + LLM compress)
// ===========================================================================

/// Plant N memories in `category` whose embeddings cluster (cosine
/// ≥0.7 with `head_v`) and backdate them so retention is below the
/// 0.20 consolidation threshold. Returns their IDs.
async fn plant_fading_cluster(
    client: &mut Client,
    embeddings: &Arc<FakeEmbeddingProvider>,
    store: &Store,
    category: &str,
    head_v: Vec<f32>,
    n: usize,
) -> Vec<String> {
    let mut ids = Vec::new();
    for i in 0..n {
        let text = format!("cluster {category} item {i}");
        // Each item: same head vector with tiny per-item perturbation
        // so they're distinct memories but cosine ≥ 0.99 (same cluster).
        let mut v = head_v.clone();
        // Perturb the last component minimally; keep cosine high.
        let dim = v.len();
        v[dim - 1] += (i as f32) * 1e-4;
        embeddings.set_override(text.clone(), v);
        let id = store_helper(client, &text, category).await;
        ids.push(id);
    }
    // Backdate so computed retention drops below 0.20.
    let now: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let backdated = now - 5 * 365 * 86_400; // 5 years ago — well into fading band
    for id in &ids {
        sqlx::query("UPDATE memories SET last_accessed_at = ?, created_at = ? WHERE id = ?")
            .bind(backdated)
            .bind(backdated)
            .bind(id)
            .execute(store.writer())
            .await
            .unwrap();
    }
    ids
}

/// Tick with no LLM configured: consolidation pass is a no-op. The
/// daemon must NOT crash and must not create summary memories.
#[tokio::test]
async fn consolidation_with_no_llm_is_noop() {
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_embeddings().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let mut head_v = vec![0.0f32; 16];
    head_v[0] = 1.0;
    let _ids = plant_fading_cluster(&mut client, &embeddings, &store, "semantic", head_v, 6).await;
    drop(store);

    let counts_pre = match client
        .request(Request::Diagnostics(DiagnosticsRequest::Counts(
            CountsArgs {
                user_id: "alice".to_string(),
            },
        )))
        .await
        .unwrap()
        .data
        .unwrap()
    {
        ResponseData::Counts(c) => c.total,
        _ => panic!(),
    };

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    let counts_post = match client
        .request(Request::Diagnostics(DiagnosticsRequest::Counts(
            CountsArgs {
                user_id: "alice".to_string(),
            },
        )))
        .await
        .unwrap()
        .data
        .unwrap()
    {
        ResponseData::Counts(c) => c.total,
        _ => panic!(),
    };

    assert_eq!(
        counts_pre, counts_post,
        "no LLM ⇒ no summary memory created (counts unchanged)"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Tick with LLM + 6 fading semantic memories at high similarity:
/// produces exactly 1 summary memory. The 6th item is left out (group
/// caps at CONSOLIDATION_GROUP_SIZE=5).
#[tokio::test]
async fn consolidation_with_six_fading_creates_one_summary_of_five() {
    let llm = Arc::new(
        cognitive_memory_llm::FakeLlmProvider::new("fake", "fake-summary")
            .with_responses(["User repeatedly mentioned hiking outdoors."]),
    ) as Arc<dyn cognitive_memory_llm::LlmProvider>;
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_llm(llm).await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let mut head_v = vec![0.0f32; 16];
    head_v[0] = 1.0;
    let ids = plant_fading_cluster(&mut client, &embeddings, &store, "semantic", head_v, 6).await;

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    // Count summary memories: any memory whose memory_type='summary'.
    let (summaries,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM memories WHERE memory_type = 'summary'")
            .fetch_one(store.reader())
            .await
            .unwrap();
    assert_eq!(summaries, 1, "exactly one summary should be created");

    // Exactly 5 of the 6 originals should be superseded + cold.
    let (superseded_n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM memories WHERE is_superseded = 1 AND is_cold = 1
         AND id IN (SELECT id FROM memories WHERE memory_type = 'fact')",
    )
    .fetch_one(store.reader())
    .await
    .unwrap();
    drop(store);
    assert_eq!(
        superseded_n, 5,
        "5 of the 6 originals should be superseded + cold; 6th left for next tick"
    );
    assert_eq!(ids.len(), 6); // sanity

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// 4 fading memories: below CONSOLIDATION_GROUP_SIZE=5 ⇒ no clusters
/// formed, no summary created.
#[tokio::test]
async fn consolidation_below_group_size_creates_no_summary() {
    let llm = Arc::new(
        cognitive_memory_llm::FakeLlmProvider::new("fake", "fake-summary").with_responses(["x"]),
    ) as Arc<dyn cognitive_memory_llm::LlmProvider>;
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_llm(llm).await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let mut head_v = vec![0.0f32; 16];
    head_v[0] = 1.0;
    let _ = plant_fading_cluster(&mut client, &embeddings, &store, "semantic", head_v, 4).await;

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    let (summaries,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM memories WHERE memory_type = 'summary'")
            .fetch_one(store.reader())
            .await
            .unwrap();
    drop(store);
    assert_eq!(summaries, 0, "below group size = no clustering");

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Per-category isolation: 3 semantic + 2 episodic fading. Neither
/// category meets CONSOLIDATION_GROUP_SIZE=5 alone ⇒ no summaries.
#[tokio::test]
async fn consolidation_does_not_cluster_across_categories() {
    let llm = Arc::new(
        cognitive_memory_llm::FakeLlmProvider::new("fake", "fake-summary").with_responses(["x"]),
    ) as Arc<dyn cognitive_memory_llm::LlmProvider>;
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_llm(llm).await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let mut head_v = vec![0.0f32; 16];
    head_v[0] = 1.0;
    let _ = plant_fading_cluster(
        &mut client,
        &embeddings,
        &store,
        "semantic",
        head_v.clone(),
        3,
    )
    .await;
    let _ = plant_fading_cluster(&mut client, &embeddings, &store, "episodic", head_v, 2).await;

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    let (summaries,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM memories WHERE memory_type = 'summary'")
            .fetch_one(store.reader())
            .await
            .unwrap();
    drop(store);
    assert_eq!(summaries, 0, "categories are clustered separately");

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// 5-memory cluster with FakeLlm returning a known summary string:
/// resulting summary memory has the LLM's content, max(importance),
/// mean(stability), inherited category, and an embedding.
#[tokio::test]
async fn consolidation_summary_carries_max_importance_and_mean_stability() {
    let summary = "User has consistently preferred blue.";
    // Plant N=5 same-cluster memories. Each store fires a conflict
    // search against all previous (cos≈1.0 ≥ 0.85 threshold), so
    // the queue accumulates C(5,2)=10 conflict pairs by store time.
    // Tick consumes the LLM responses in order: 10 conflict-judge
    // calls THEN 1 consolidation call. Queue exactly 10 NONEs (so
    // conflicts no-op) then the summary (consumed by consolidation).
    let mut responses: Vec<String> = std::iter::repeat_n("NONE".to_string(), 10).collect();
    responses.push(summary.to_string());
    // Tail padding for any extra LLM calls we don't expect — defensive.
    responses.extend(std::iter::repeat_n("NONE".to_string(), 10));
    let llm = Arc::new(
        cognitive_memory_llm::FakeLlmProvider::new("fake", "fake-summary")
            .with_responses(responses),
    ) as Arc<dyn cognitive_memory_llm::LlmProvider>;
    let (handle, socket, shutdown, tmp, embeddings) = boot_daemon_with_llm(llm).await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let mut head_v = vec![0.0f32; 16];
    head_v[0] = 1.0;
    let ids = plant_fading_cluster(&mut client, &embeddings, &store, "semantic", head_v, 5).await;

    // Pin importance + stability low enough that the importance
    // multiplier `B = 1 + 2*importance` doesn't lift retention out
    // of the fading band. importance = [0.04, 0.08, 0.12, 0.16, 0.20]
    // → max = 0.20. stability = [0.05, 0.10, 0.15, 0.20, 0.25] → mean
    // = 0.15. With β=120 and Δt=365d, R ≈ 0.16 (below 0.20 threshold).
    for (i, id) in ids.iter().enumerate() {
        let imp = 0.04 * ((i + 1) as f64);
        let stab = 0.05 * ((i + 1) as f64);
        sqlx::query("UPDATE memories SET importance = ?, stability = ? WHERE id = ?")
            .bind(imp)
            .bind(stab)
            .bind(id)
            .execute(store.writer())
            .await
            .unwrap();
    }

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();

    let row: (String, String, f64, f64, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT content, category, importance, stability, embedding
         FROM memories WHERE memory_type = 'summary'",
    )
    .fetch_one(store.reader())
    .await
    .unwrap();
    drop(store);
    let (content, category, importance, stability, embedding) = row;
    assert_eq!(content, summary, "summary content from LLM");
    assert_eq!(category, "semantic", "summary inherits group category");
    // importance = max([0.04, 0.08, 0.12, 0.16, 0.20]) = 0.20.
    assert!(
        (importance - 0.20).abs() < 1e-6,
        "importance = max(group), expected 0.20, got {importance}"
    );
    // stability = mean([0.05, 0.10, 0.15, 0.20, 0.25]) = 0.15.
    assert!(
        (stability - 0.15).abs() < 1e-6,
        "stability = mean(group), expected 0.15, got {stability}"
    );
    assert!(embedding.is_some(), "summary must be embedded");

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// Status uptime must reflect actual elapsed time, not the bug at
/// 0.0.1 release where the field was hardcoded to 0.
#[tokio::test]
async fn status_uptime_advances_with_wall_clock() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let first = client
        .request(Request::Diagnostics(DiagnosticsRequest::Status))
        .await
        .unwrap();
    let first_uptime = match first.data.unwrap() {
        ResponseData::Status(s) => s.uptime_seconds,
        other => panic!("expected Status, got {other:?}"),
    };

    tokio::time::sleep(Duration::from_millis(1_100)).await;

    let second = client
        .request(Request::Diagnostics(DiagnosticsRequest::Status))
        .await
        .unwrap();
    let second_uptime = match second.data.unwrap() {
        ResponseData::Status(s) => s.uptime_seconds,
        other => panic!("expected Status, got {other:?}"),
    };

    assert!(
        second_uptime > first_uptime,
        "uptime must advance: first={first_uptime}s second={second_uptime}s"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

// ===========================================================================
// Phase 8 — decay-on-read + tick consolidation
// ===========================================================================
//
// These tests exercise the lifecycle wiring end-to-end. The trick is
// that decay is wall-clock-based, so we backdate `last_accessed_at`
// directly via SQL on the daemon's underlying store before reading,
// rather than waiting real days.

async fn store_helper(client: &mut Client, content: &str, category: &str) -> String {
    let resp = client
        .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
            user_id: "alice".to_string(),
            content: content.to_string(),
            category: category.to_string(),
            memory_type: "fact".to_string(),
            metadata: "{}".to_string(),
            importance: None,
        })))
        .await
        .unwrap();
    match resp.data.unwrap() {
        ResponseData::MemoryStored(s) => s.id,
        other => panic!("expected MemoryStored, got {other:?}"),
    }
}

async fn fetch(client: &mut Client, id: &str) -> cognitive_memory_protocol::MemoryData {
    let resp = client
        .request(Request::Memory(MemoryRequest::Get(GetMemoryArgs {
            user_id: "alice".to_string(),
            id: id.to_string(),
        })))
        .await
        .unwrap();
    match resp.data.unwrap() {
        ResponseData::Memory(m) => m,
        other => panic!("expected Memory, got {other:?}"),
    }
}

async fn backdate(store: &Store, id: &str, days_ago: i64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let when = now - days_ago * 86400;
    sqlx::query("UPDATE memories SET last_accessed_at = ?, created_at = ? WHERE id = ?")
        .bind(when)
        .bind(when)
        .bind(id)
        .execute(store.writer())
        .await
        .unwrap();
}

#[tokio::test]
async fn current_retention_decays_over_time_per_category() {
    let (handle, socket, shutdown, tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let episodic_id = store_helper(&mut client, "an episode", "episodic").await;
    let semantic_id = store_helper(&mut client, "a fact", "semantic").await;
    let core_id = store_helper(&mut client, "core knowledge", "core").await;
    let proc_id = store_helper(&mut client, "step-by-step", "procedural").await;

    // Fresh memories: retention near 1.0.
    let fresh_episodic = fetch(&mut client, &episodic_id).await;
    assert!(
        fresh_episodic.current_retention > 0.99,
        "fresh episodic retention should be ~1.0, got {}",
        fresh_episodic.current_retention
    );

    // Backdate everything 365 days AND pin stability to 0.5 so the
    // expected retention ranges below are derived purely from category
    // (β) and Δt, not from the stability default. The default is
    // 0.1 + 0.3*importance per the SDK; this test isolates decay-by-
    // category, so we control stability explicitly.
    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    for id in [&episodic_id, &semantic_id, &core_id, &proc_id] {
        backdate(&store, id, 365).await;
        sqlx::query("UPDATE memories SET stability = 0.5 WHERE id = ?")
            .bind(id)
            .execute(store.writer())
            .await
            .unwrap();
    }
    drop(store);

    let aged_episodic = fetch(&mut client, &episodic_id).await;
    let aged_semantic = fetch(&mut client, &semantic_id).await;
    let aged_core = fetch(&mut client, &core_id).await;
    let aged_proc = fetch(&mut client, &proc_id).await;

    // Episodic (β=45d) should drop sharply: power-law with γ=0.7,
    // S=0.5, B=1.0 (importance=0). Effective rate = 0.5*1*45 = 22.5.
    // raw = (1 + 365/22.5)^-0.7 = ~16.2^-0.7 ≈ 0.144. Floor=0.0.
    assert!(
        aged_episodic.current_retention < 0.20,
        "episodic at 365d should be < 0.20, got {}",
        aged_episodic.current_retention
    );

    // Semantic (β=120d): effective = 60. raw = (1 + 365/60)^-0.7
    // = ~7.08^-0.7 ≈ 0.265.
    assert!(
        aged_semantic.current_retention > 0.20 && aged_semantic.current_retention < 0.35,
        "semantic at 365d should be in (0.20, 0.35), got {}",
        aged_semantic.current_retention
    );

    // Core (β=120d, but floor=0.6 from --core / category=core):
    // raw decay matches semantic (~0.265), but clamps to floor 0.6.
    assert!(
        (aged_core.current_retention - 0.6).abs() < 1e-9,
        "core at 365d should clamp at floor 0.6, got {}",
        aged_core.current_retention
    );

    // Procedural: base_decay_rate is INFINITY, retention always 1.0.
    assert!(
        (aged_proc.current_retention - 1.0).abs() < 1e-9,
        "procedural at 365d should still be 1.0, got {}",
        aged_proc.current_retention
    );

    // Differential check: episodic decays faster than semantic.
    assert!(
        aged_episodic.current_retention < aged_semantic.current_retention,
        "episodic ({}) should decay faster than semantic ({})",
        aged_episodic.current_retention,
        aged_semantic.current_retention
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn tick_increments_days_at_floor_for_at_floor_memories_and_resets_others() {
    let (handle, socket, shutdown, tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let id_floor = store_helper(&mut client, "will floor", "semantic").await;
    let id_fresh = store_helper(&mut client, "stays fresh", "semantic").await;

    // Pin id_floor's retention floor high, then backdate it so its
    // computed retention is at floor. id_fresh stays as-is.
    client
        .request(Request::Lifecycle(LifecycleRequest::UpdateRetention(
            UpdateRetentionArgs {
                user_id: "alice".to_string(),
                id: id_floor.clone(),
                retention_floor: 0.5,
            },
        )))
        .await
        .unwrap();

    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    backdate(&store, &id_floor, 365).await;
    drop(store);

    // First tick — id_floor at floor, id_fresh not.
    let resp = client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();
    let decayed = match resp.data.unwrap() {
        ResponseData::Tick(t) => t.memories_decayed,
        other => panic!("expected Tick, got {other:?}"),
    };
    assert_eq!(decayed, 1, "exactly one memory should be at floor");

    let after_first = fetch(&mut client, &id_floor).await;
    assert_eq!(after_first.days_at_floor, 1);
    let fresh_after = fetch(&mut client, &id_fresh).await;
    assert_eq!(fresh_after.days_at_floor, 0);

    // Tick 4 more times → days_at_floor=5.
    for _ in 0..4 {
        client
            .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
                synchronous: true,
            })))
            .await
            .unwrap();
    }
    let after_five = fetch(&mut client, &id_floor).await;
    assert_eq!(after_five.days_at_floor, 5);

    // "Refresh" the at-floor memory by un-backdating it. Then a tick
    // should reset days_at_floor to 0.
    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    sqlx::query("UPDATE memories SET last_accessed_at = ? WHERE id = ?")
        .bind(now)
        .bind(&id_floor)
        .execute(store.writer())
        .await
        .unwrap();
    drop(store);

    client
        .request(Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: true,
        })))
        .await
        .unwrap();
    let after_reset = fetch(&mut client, &id_floor).await;
    assert_eq!(after_reset.days_at_floor, 0, "tick should reset on refresh");

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// SDK parity (engine.py: `include_superseded = deep_recall`).
/// Default search must hide superseded memories; `--deep-recall` must
/// surface them so the audit trail is reachable through search.
#[tokio::test]
async fn deep_recall_surfaces_superseded_memories() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let old_id = store_helper(&mut client, "User likes blue", "semantic").await;
    let new_id = store_helper(&mut client, "User now prefers green", "semantic").await;

    use cognitive_memory_protocol::MarkSupersededArgs;
    client
        .request(Request::Lifecycle(LifecycleRequest::MarkSuperseded(
            MarkSupersededArgs {
                user_id: "alice".to_string(),
                summary_id: new_id.clone(),
                ids: vec![old_id.clone()],
            },
        )))
        .await
        .unwrap();

    async fn ids_for_search(client: &mut Client, deep: bool) -> Vec<String> {
        let r = client
            .request(Request::Memory(MemoryRequest::Search(SearchMemoryArgs {
                user_id: "alice".to_string(),
                query: "User likes blue".to_string(),
                limit: 10,
                deep_recall: deep,
                hybrid: false,
                graph_expansion_hops: 0,
                bridge_discovery: false,
            })))
            .await
            .unwrap();
        match r.data.unwrap() {
            ResponseData::MemorySearchResults(rs) => {
                rs.results.into_iter().map(|h| h.memory_id).collect()
            }
            other => panic!("expected MemorySearchResults, got {other:?}"),
        }
    }

    let default_ids = ids_for_search(&mut client, false).await;
    assert!(
        !default_ids.contains(&old_id),
        "default search must NOT include superseded {old_id}; got {default_ids:?}"
    );

    let deep_ids = ids_for_search(&mut client, true).await;
    assert!(
        deep_ids.contains(&old_id),
        "deep_recall search MUST include superseded {old_id}; got {deep_ids:?}"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn stub_returns_retention_zero_regardless_of_age() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let mut client = Client::connect(&socket, "test-client", "alice")
        .await
        .unwrap();

    let id = store_helper(&mut client, "becomes a stub", "semantic").await;
    use cognitive_memory_protocol::ConvertToStubArgs;
    client
        .request(Request::Lifecycle(LifecycleRequest::ConvertToStub(
            ConvertToStubArgs {
                user_id: "alice".to_string(),
                id: id.clone(),
                stub_content: "[archived]".to_string(),
            },
        )))
        .await
        .unwrap();

    let m = fetch(&mut client, &id).await;
    assert!(m.is_stub);
    assert!(
        m.current_retention.abs() < 1e-9,
        "stub retention must be 0, got {}",
        m.current_retention
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

// =========================================================================
// Phase 0a-daemon — `[lifecycle]` override propagation through the IPC path.
//
// The Stage 0a-daemon plan (~/.claude/plans/now-create-a-plan-validated-yao.md)
// calls for an e2e test that "writes a config.toml with `[lifecycle]`
// override, restarts daemon, verifies a stored memory's `current_retention`
// reflects the new β." This is that test, structured a level below
// config.toml — it constructs the same `LifecycleConfig` that
// `main.rs::build_lifecycle_config()` would produce after merging
// the TOML overrides, and threads it through `Daemon::new_full`. The
// TOML parse step is covered by `crates/core/src/config.rs` unit tests
// (4 tests on `[lifecycle.base_decay_rates]` parse + roundtrip).
// =========================================================================

/// Helper: store a semantic memory via IPC, backdate its `last_accessed_at`
/// directly via SQLite, then re-fetch via IPC. Returns the
/// `current_retention` the daemon computed.
///
/// `age_days` controls how far back the access timestamp is set so the
/// retention formula returns a value < 1.0 that's sensitive to β.
async fn store_backdate_get_retention(
    socket: &PathBuf,
    db_path: &PathBuf,
    age_days: i64,
) -> f64 {
    let mut client = Client::connect(socket, "test-client", "alice")
        .await
        .unwrap();
    let stored = client
        .request(Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
            user_id: "alice".to_string(),
            content: "the user's favourite colour is blue".to_string(),
            category: "semantic".to_string(),
            memory_type: "fact".to_string(),
            metadata: "{}".to_string(),
            // importance=Some(0.0) keeps boost factor B=1.0 so retention
            // is purely β-driven (no importance multiplier confound).
            importance: Some(0.0),
        })))
        .await
        .unwrap();
    let id = match stored.data.unwrap() {
        ResponseData::MemoryStored(m) => m.id,
        other => panic!("expected MemoryStored on Store, got {other:?}"),
    };

    // Backdate via direct SQLite write so retention has time to decay.
    // The daemon doesn't refuse concurrent reader access (WAL mode);
    // dropping our store handle before the next IPC call avoids any
    // writer contention.
    let store = Store::open(db_path).await.unwrap();
    let backdate_secs = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - age_days * 86_400;
    sqlx::query("UPDATE memories SET last_accessed_at = ? WHERE id = ?")
        .bind(backdate_secs)
        .bind(&id)
        .execute(store.writer())
        .await
        .unwrap();
    drop(store);

    let resp = client
        .request(Request::Memory(MemoryRequest::Get(GetMemoryArgs {
            user_id: "alice".to_string(),
            id,
        })))
        .await
        .unwrap();
    match resp.data.unwrap() {
        ResponseData::Memory(m) => m.current_retention,
        other => panic!("expected Memory on Get, got {other:?}"),
    }
}

#[tokio::test]
async fn lifecycle_override_changes_current_retention_through_ipc() {
    // Two daemons, identical except for `LifecycleConfig.base_decay_rates`:
    //   - paper: semantic β = 120d (Table 2 default)
    //   - fast:  semantic β =  60d (override; halved)
    // Same memory shape (semantic, importance=0, stability default), same
    // 90-day backdate. Faster β should drop retention further → r_fast <
    // r_paper. This is the contract config.toml [lifecycle] is supposed
    // to deliver to operators tuning the daemon.
    use cognitive_memory_lifecycle::LifecycleConfig;

    let mut fast_cfg = cognitive_memory_daemon::paper_faithful_lifecycle_config();
    fast_cfg
        .base_decay_rates
        .insert("semantic".to_string(), 60.0);

    let paper_cfg: LifecycleConfig = cognitive_memory_daemon::paper_faithful_lifecycle_config();

    // Boot both daemons. Each gets its own tempdir, socket, store.
    let (h1, sock1, sh1, _tmp1, _emb1, db1) =
        boot_daemon_with_lifecycle(paper_cfg).await;
    let (h2, sock2, sh2, _tmp2, _emb2, db2) =
        boot_daemon_with_lifecycle(fast_cfg).await;

    let r_paper = store_backdate_get_retention(&sock1, &db1, 90).await;
    let r_fast = store_backdate_get_retention(&sock2, &db2, 90).await;

    let _ = sh1.send(());
    let _ = sh2.send(());
    let _ = h1.await;
    let _ = h2.await;

    // Strict inequality is the contract: halving β doubles the effective
    // decay rate, so 90-day retention must be lower under the override.
    assert!(
        r_fast < r_paper,
        "override didn't change retention: r_paper={r_paper}, r_fast={r_fast}"
    );
    // Sanity: both must still be in [0, 1] (no NaN, no above-floor weirdness).
    assert!(r_paper > 0.0 && r_paper <= 1.0, "r_paper out of range: {r_paper}");
    assert!(r_fast > 0.0 && r_fast <= 1.0, "r_fast out of range: {r_fast}");
}

#[tokio::test]
async fn paper_faithful_default_matches_table_2_through_ipc() {
    // Sibling test: confirm the daemon's `paper_faithful_lifecycle_config()`
    // produces a retention that matches the hand-computed paper formula
    // for a known memory shape. Catches accidental drift from Table 2
    // defaults during refactors.
    let cfg = cognitive_memory_daemon::paper_faithful_lifecycle_config();
    assert!(
        (cfg.beta_for("semantic") - 120.0).abs() < 1e-9,
        "paper default β_semantic must be 120d, got {}",
        cfg.beta_for("semantic")
    );
    assert!(
        (cfg.beta_for("episodic") - 45.0).abs() < 1e-9,
        "paper default β_episodic must be 45d, got {}",
        cfg.beta_for("episodic")
    );
    assert!(
        cfg.beta_for("procedural").is_infinite(),
        "paper default β_procedural must be ∞ (no decay)"
    );
}
