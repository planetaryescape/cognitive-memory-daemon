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
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("cm.sock");
    let db_path = tmp.path().join("data.db");

    let store = Store::open(&db_path).await.unwrap();
    let embeddings = Arc::new(FakeEmbeddingProvider::new("local", "fake-16", 16));
    let daemon = Daemon::new(store, embeddings, socket_path.clone());
    let shutdown = daemon.shutdown_handle();

    let handle = tokio::spawn(async move {
        daemon.serve().await.expect("daemon serve");
    });

    // Wait briefly for the socket to appear.
    for _ in 0..100 {
        if socket_path.exists() {
            return (handle, socket_path, shutdown, tmp);
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

    // Backdate everything 365 days. Reach into the daemon's SQLite to
    // edit last_accessed_at directly — there is no IPC op to set it.
    let store = Store::open(&tmp.path().join("data.db")).await.unwrap();
    for id in [&episodic_id, &semantic_id, &core_id, &proc_id] {
        backdate(&store, id, 365).await;
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
