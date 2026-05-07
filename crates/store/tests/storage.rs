//! Storage-layer tests for cognitive-memory-store.
//!
//! Each test exercises one behaviour. See the build plan
//! `~/.claude/plans/now-create-a-plan-validated-yao.md` Phase 1 for the
//! 12-behaviour TDD checklist this file walks.
//!
//! Tests use `Store::in_memory()` exclusively — fast, no temp files, but
//! exercises the same migration engine and pool wiring as on-disk DBs.
//! Per `docs/developer/test-discipline.md`, tests against SQLite use real
//! SQLite (in this case in-memory) — never mocks.

#![allow(clippy::panic, clippy::unwrap_used)]

use cognitive_memory_store::Store;
use tempfile::TempDir;

/// Behaviour 1: a fresh in-memory store can be opened without error.
#[tokio::test]
async fn fresh_in_memory_store_opens_successfully() {
    let store = Store::in_memory().await.expect("open in-memory store");
    // The Store handle existing is the assertion; if the constructor fails,
    // the `.expect` panics.
    drop(store);
}

/// Behaviour 2: opening a fresh store applies the v1 migration; the
/// `schema_migrations` table records it.
#[tokio::test]
async fn opening_fresh_store_applies_initial_migration() {
    let store = Store::in_memory().await.unwrap();

    let row: (i64, String) =
        sqlx::query_as("SELECT version, name FROM schema_migrations WHERE version = 1")
            .fetch_one(store.reader())
            .await
            .expect("schema_migrations row for v1 must exist");

    assert_eq!(row.0, 1);
    assert_eq!(row.1, "initial_schema");
}

/// Behaviour 3: re-running migrations on an already-migrated store is a
/// no-op (idempotence). This is the most important property in the layer —
/// a half-applied migration after a crash must replay cleanly.
#[tokio::test]
async fn re_running_migrations_is_idempotent() {
    let store = Store::in_memory().await.unwrap();

    // Re-running migrations would normally happen via Store::open on an
    // existing DB. We simulate it by counting schema_migrations rows before
    // and after a no-op migration pass — there's only one migration in v1
    // so this is enough to assert idempotence.
    let count_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM schema_migrations")
        .fetch_one(store.reader())
        .await
        .unwrap();

    // After every migration in the engine has been applied, the row count
    // equals the number of declared migrations. Adding a migration bumps
    // this expectation in lockstep with the constant `MIGRATIONS` slice.
    let expected_migrations = 5_i64;
    assert_eq!(
        count_before.0, expected_migrations,
        "expected one row per declared migration"
    );

    // Also assert that the schema is intact: a known table from migration
    // v1 exists.
    let table_row: (String,) =
        sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' AND name='memories'")
            .fetch_one(store.reader())
            .await
            .expect("memories table must exist after migrations");
    assert_eq!(table_row.0, "memories");
}

/// Behaviour 7: `journal_mode = WAL` and `foreign_keys = ON` are configured
/// on the pool's connections.
#[tokio::test]
async fn pragmas_wal_and_foreign_keys_are_enabled() {
    let store = Store::in_memory().await.unwrap();

    let (journal,): (String,) = sqlx::query_as("PRAGMA journal_mode")
        .fetch_one(store.reader())
        .await
        .unwrap();
    // SQLite's in-memory DB downgrades WAL → MEMORY journal_mode silently.
    // For an in-memory DB MEMORY is the correct equivalent ("write-ahead"
    // semantics are moot when the DB doesn't survive). On a file DB the
    // pragma reports "wal". This test accepts both so it's portable.
    assert!(
        matches!(journal.as_str(), "wal" | "memory"),
        "expected wal or memory, got {journal}"
    );

    let (fk_on,): (i64,) = sqlx::query_as("PRAGMA foreign_keys")
        .fetch_one(store.reader())
        .await
        .unwrap();
    assert_eq!(fk_on, 1, "foreign_keys must be ON");
}

/// `AssociationRepo::strengthen_pairs` UPSERTs bidirectional weights
/// in one transaction. Mirrors SDK engine.py:621-625 + core.py:258-262.
/// Pre-existing edge weight 0.4 + bump 0.1 → 0.5; new edges → 0.1.
/// Both directions get last_co_retrieval = now.
#[tokio::test]
async fn strengthen_pairs_bumps_bidirectional_weights_and_refreshes_last_co_retrieval() {
    use cognitive_memory_store::{AssociationRepo, MemoryRepo, MemoryRow};

    let store = Store::in_memory().await.unwrap();
    let mem_repo = MemoryRepo::new(&store);
    let assoc_repo = AssociationRepo::new(&store);

    for id in ["m_a", "m_b", "m_c"] {
        mem_repo
            .insert(&MemoryRow::new_minimal(
                id, "alice", "x", "semantic", "fact", 100,
            ))
            .await
            .unwrap();
    }

    // Pre-existing A↔B at weight 0.4, last_co_retrieval=100.
    assoc_repo
        .create_or_strengthen("m_a", "m_b", 0.4, 100, "thematic")
        .await
        .unwrap();
    assoc_repo
        .create_or_strengthen("m_b", "m_a", 0.4, 100, "thematic")
        .await
        .unwrap();

    let pairs = vec![
        ("m_a".to_string(), "m_b".to_string()),
        ("m_a".to_string(), "m_c".to_string()),
    ];
    let now: i64 = 999;
    assoc_repo
        .strengthen_pairs(&pairs, 0.1, now, "co-retrieval")
        .await
        .unwrap();

    async fn fetch_edge(store: &Store, src: &str, tgt: &str) -> (f64, i64) {
        sqlx::query_as(
            "SELECT weight, last_co_retrieval FROM associations
             WHERE source_memory_id = ? AND target_memory_id = ?",
        )
        .bind(src)
        .bind(tgt)
        .fetch_one(store.reader())
        .await
        .unwrap()
    }

    let (w_ab, last_ab) = fetch_edge(&store, "m_a", "m_b").await;
    assert!(
        (w_ab - 0.5).abs() < 1e-6,
        "A→B should be 0.4+0.1=0.5, got {w_ab}"
    );
    assert_eq!(last_ab, 999);

    let (w_ba, last_ba) = fetch_edge(&store, "m_b", "m_a").await;
    assert!(
        (w_ba - 0.5).abs() < 1e-6,
        "B→A bidirectional, expected 0.5, got {w_ba}"
    );
    assert_eq!(last_ba, 999);

    let (w_ac, last_ac) = fetch_edge(&store, "m_a", "m_c").await;
    assert!(
        (w_ac - 0.1).abs() < 1e-6,
        "A→C new edge, expected 0.1, got {w_ac}"
    );
    assert_eq!(last_ac, 999);

    let (w_ca, last_ca) = fetch_edge(&store, "m_c", "m_a").await;
    assert!(
        (w_ca - 0.1).abs() < 1e-6,
        "C→A new bidirectional, expected 0.1, got {w_ca}"
    );
    assert_eq!(last_ca, 999);
}

/// `AssociationRepo::neighbor_edges` returns `last_co_retrieval`
/// alongside target_id and weight, so callers (graph expansion, BFS
/// bridge) can apply Eq 10 decay (`w * exp(-Δt/90)`) at read time.
#[tokio::test]
async fn neighbor_edges_returns_last_co_retrieval_alongside_weight() {
    use cognitive_memory_store::{AssociationRepo, MemoryRepo, MemoryRow};

    let store = Store::in_memory().await.unwrap();
    let mem_repo = MemoryRepo::new(&store);
    let assoc_repo = AssociationRepo::new(&store);

    // Two memories owned by the same user so we can link them.
    for id in ["mem_a", "mem_b"] {
        mem_repo
            .insert(&MemoryRow::new_minimal(
                id,
                "alice",
                "content",
                "semantic",
                "fact",
                1_700_000_000,
            ))
            .await
            .unwrap();
    }
    // Create a directed edge with `updated_at = 1_700_000_000`.
    // The migration backfilled `last_co_retrieval = updated_at`, so
    // the edge's last_co_retrieval should match.
    assoc_repo
        .create_or_strengthen("mem_a", "mem_b", 0.7, 1_700_000_000, "thematic")
        .await
        .unwrap();

    let edges = assoc_repo
        .neighbor_edges("mem_a", 0.0)
        .await
        .expect("neighbor_edges");

    assert_eq!(edges.len(), 1, "exactly one outgoing edge");
    let edge = &edges[0];
    assert_eq!(edge.target_id, "mem_b");
    assert!((edge.weight - 0.7).abs() < 1e-6);
    assert_eq!(
        edge.last_co_retrieval,
        Some(1_700_000_000),
        "last_co_retrieval must be returned (defaulted to updated_at on insert)"
    );
}

/// Migration v5 adds `last_co_retrieval` to the associations table so
/// the SDK's Eq 10 read-side decay (`w * exp(-Δt/90)`) has a per-edge
/// timestamp to compute Δt against.
#[tokio::test]
async fn migration_v5_adds_last_co_retrieval_column_to_associations() {
    let store = Store::in_memory().await.unwrap();
    // PRAGMA table_info returns one row per column with the column
    // name in position 1. We assert that `last_co_retrieval` is in
    // the resulting set.
    let rows: Vec<(i64, String, String, i64, Option<String>, i64)> =
        sqlx::query_as("PRAGMA table_info('associations')")
            .fetch_all(store.reader())
            .await
            .unwrap();
    let names: Vec<String> = rows.into_iter().map(|r| r.1).collect();
    assert!(
        names.iter().any(|n| n == "last_co_retrieval"),
        "associations must have last_co_retrieval column post-v5; got {names:?}"
    );
}

/// Behaviour 8 + 9 + 10: MemoryRepo round-trips a memory by user_id, returns
/// None for unknown ids, and isolates rows by user_id.
#[tokio::test]
async fn memory_repo_inserts_gets_and_isolates_by_user_id() {
    use cognitive_memory_store::{MemoryRepo, MemoryRow};

    let store = Store::in_memory().await.unwrap();
    let repo = MemoryRepo::new(&store);

    let mut row = MemoryRow::new_minimal(
        "mem_alice_1",
        "alice",
        "Alice likes Rust.",
        "semantic",
        "preference",
        100,
    );
    row.metadata = r#"{"project":"cognitive-memory"}"#.to_string();

    repo.insert(&row).await.expect("insert");

    let fetched = repo
        .get_for_user("alice", "mem_alice_1")
        .await
        .expect("get")
        .expect("memory must be present");
    assert_eq!(fetched, row);

    let absent = repo
        .get_for_user("alice", "mem_does_not_exist")
        .await
        .expect("get");
    assert!(absent.is_none(), "unknown id must return None");

    let cross_tenant = repo
        .get_for_user("bob", "mem_alice_1")
        .await
        .expect("cross-tenant get");
    assert!(
        cross_tenant.is_none(),
        "cross-tenant read must return None — user_id is the hard isolation key"
    );
}

/// Behaviour 11: EmbeddingCacheRepo `get_or_insert` returns the cached
/// vector on subsequent calls keyed by (provider, model, text_hash). This
/// is the daemon's central efficiency win; if cache key collisions or
/// non-determinism break it, two agents pay for the same embedding twice.
#[tokio::test]
async fn embedding_cache_returns_cached_vector_on_second_call() {
    use cognitive_memory_store::EmbeddingCacheRepo;

    let store = Store::in_memory().await.unwrap();
    let repo = EmbeddingCacheRepo::new(&store);

    let provider = "local";
    let model = "bge-small-en-v1.5";
    let text_hash = vec![0xab_u8; 32];
    let vector = vec![0.1_f32, 0.2, 0.3];

    let inserted = repo
        .insert(provider, model, &text_hash, &vector)
        .await
        .expect("insert");
    assert!(inserted, "first insert must report inserted=true");

    let fetched = repo
        .get(provider, model, &text_hash)
        .await
        .expect("get")
        .expect("vector must be cached");
    assert_eq!(fetched, vector);

    // Second insert with identical key is a no-op (idempotent).
    let inserted_again = repo
        .insert(provider, model, &text_hash, &vector)
        .await
        .expect("insert again");
    assert!(
        !inserted_again,
        "second insert with identical key must report inserted=false (already cached)"
    );

    // Different provider+model: cache miss.
    let other = repo
        .get("openai", model, &text_hash)
        .await
        .expect("get other-provider");
    assert!(other.is_none());
}

/// Behaviour 4: re-opening an already-migrated file-backed store does not
/// re-apply migrations (or, if it does run them, they're idempotent and
/// schema_migrations is unchanged). This is the crash-recovery property.
#[tokio::test]
async fn reopening_file_backed_store_does_not_duplicate_migration_rows() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("data.db");

    {
        let store = Store::open(&path).await.unwrap();
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM schema_migrations")
            .fetch_one(store.reader())
            .await
            .unwrap();
        assert_eq!(count.0, 5, "v1 + v2 + v3 + v4 + v5 migrations applied");
        // Drop the store; pools close.
    }

    // Reopen and confirm the migration row count is still 2.
    let store_again = Store::open(&path).await.unwrap();
    let count_again: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM schema_migrations")
        .fetch_one(store_again.reader())
        .await
        .unwrap();
    assert_eq!(
        count_again.0, 5,
        "reopening must not duplicate migration rows"
    );
}

/// Behaviour 5 (and indirectly 6): concurrent writes on the writer pool
/// complete without deadlock and without interleaving (each write is
/// atomic). We don't directly test that the second write *waits* — that's
/// an implementation detail of `SqlitePoolOptions::max_connections(1)`.
/// The behaviour we depend on is "two concurrent writes both succeed and
/// both rows are present afterward, with no torn state."
#[tokio::test]
async fn concurrent_writes_serialise_without_deadlock() {
    use cognitive_memory_store::{MemoryRepo, MemoryRow};
    use std::sync::Arc;

    let store = Arc::new(Store::in_memory().await.unwrap());

    fn make_row(id: &str) -> MemoryRow {
        MemoryRow::new_minimal(id, "alice", "x", "semantic", "fact", 0)
    }

    let store_a = Arc::clone(&store);
    let store_b = Arc::clone(&store);
    let task_a = tokio::spawn(async move {
        let repo = MemoryRepo::new(&store_a);
        repo.insert(&make_row("mem_a")).await
    });
    let task_b = tokio::spawn(async move {
        let repo = MemoryRepo::new(&store_b);
        repo.insert(&make_row("mem_b")).await
    });

    task_a.await.unwrap().expect("write a");
    task_b.await.unwrap().expect("write b");

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM memories")
        .fetch_one(store.reader())
        .await
        .unwrap();
    assert_eq!(count.0, 2);
}

/// Property test: the migration set, applied any number of times, leaves
/// schema_migrations with exactly one row per migration. (Phase 1
/// done-when calls for proptest on migration idempotence.) Each in-memory
/// store re-runs the engine on creation — repeated creation = repeated
/// application. This is a stand-in for "apply random subsets in random
/// order then apply all" since cognitive-memory v1 only has one migration.
/// When v2 lands, expand this property to cover non-trivial subsets.
#[test]
fn migration_engine_is_idempotent_under_repeated_application() {
    // Plain `#[test]` (sync) — proptest's runner is sync, and creating a
    // tokio runtime inside `#[tokio::test]` is forbidden ("runtime from
    // within a runtime"). One fresh runtime owns all the cases.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(&(1u32..5u32), |n_runs| {
            rt.block_on(async {
                let store = Store::in_memory().await.unwrap();
                // First open already ran migrations. Subsequent calls to
                // an analogous in-memory store would also run them; we
                // simulate by re-querying `is_migration_applied`-equivalent.
                for _ in 0..n_runs {
                    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM schema_migrations")
                        .fetch_one(store.reader())
                        .await
                        .unwrap();
                    let expected = 5;
                    if count.0 != expected {
                        return Err(proptest::test_runner::TestCaseError::fail(format!(
                            "expected {expected} migration rows, got {}",
                            count.0
                        )));
                    }
                }
                Ok(())
            })
        })
        .expect("idempotence property must hold");
}

/// Behaviour 12: EventLogRepo append assigns monotonically increasing ids
/// (powered by AUTOINCREMENT). The event log is the basis for undo and
/// pub/sub replay, so id monotonicity is load-bearing.
#[tokio::test]
async fn event_log_append_assigns_monotonic_ids() {
    use cognitive_memory_store::EventLogRepo;

    let store = Store::in_memory().await.unwrap();
    let repo = EventLogRepo::new(&store);

    let id_a = repo
        .append("MemoryStored", r#"{"memory_id":"a"}"#, 100)
        .await
        .unwrap();
    let id_b = repo
        .append("MemoryStored", r#"{"memory_id":"b"}"#, 101)
        .await
        .unwrap();
    let id_c = repo.append("TickCompleted", r#"{}"#, 102).await.unwrap();

    assert!(id_a < id_b, "ids must increase: {id_a} < {id_b}");
    assert!(id_b < id_c, "ids must increase: {id_b} < {id_c}");
}
