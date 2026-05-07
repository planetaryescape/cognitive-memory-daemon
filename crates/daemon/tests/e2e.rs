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
    UpdateMemoryArgs,
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
