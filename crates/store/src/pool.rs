// Adapted from mxr/crates/store/src/pool.rs (vendored mechanical, see
// docs/developer/code-reuse.md Phase 1). Stripped of mxr-specific
// AddColumn/Composite migration kinds — cognitive-memory's v1 schema uses
// only plain SQL migrations. Re-add the helpers if/when migrations need
// schema-altering steps that benefit from the helpers.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;

/// Two-pool SQLite wrapper. One writer (max=1) and N readers. The writer
/// pool serialises mutating SQL inside the daemon process. Single-writer is
/// principle 8 in `AGENTS.md` §2; do not open a second writer pool.
pub struct Store {
    writer: SqlitePool,
    reader: SqlitePool,
}

const READER_POOL_SIZE: u32 = 4;

impl Store {
    /// Open a Store backed by a file. Runs migrations.
    pub async fn open(db_path: &Path) -> Result<Self, sqlx::Error> {
        let db_url = format!("sqlite:{}", db_path.display());

        let write_opts = SqliteConnectOptions::from_str(&db_url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .pragma("foreign_keys", "ON");

        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(write_opts)
            .await?;

        let read_opts = SqliteConnectOptions::from_str(&db_url)?
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .pragma("foreign_keys", "ON")
            .read_only(true);

        let reader = SqlitePoolOptions::new()
            .max_connections(READER_POOL_SIZE)
            .connect_with(read_opts)
            .await?;

        let store = Self { writer, reader };
        store.run_migrations().await?;
        Ok(store)
    }

    /// Open a Store backed by an in-memory SQLite. Single shared pool
    /// because in-memory DBs do not survive across separate connections.
    /// Use this for tests; do not use it for the daemon proper.
    pub async fn in_memory() -> Result<Self, sqlx::Error> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?
            .journal_mode(SqliteJournalMode::Wal)
            .pragma("foreign_keys", "ON");

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;

        let store = Self {
            writer: pool.clone(),
            reader: pool,
        };
        store.run_migrations().await?;
        Ok(store)
    }

    /// Borrow the writer pool. Use for INSERT/UPDATE/DELETE.
    pub fn writer(&self) -> &SqlitePool {
        &self.writer
    }

    /// Borrow the reader pool. Use for SELECT only.
    pub fn reader(&self) -> &SqlitePool {
        &self.reader
    }

    async fn run_migrations(&self) -> Result<(), sqlx::Error> {
        sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version    INTEGER PRIMARY KEY,
                name       TEXT NOT NULL,
                applied_at INTEGER NOT NULL
            )",
        )
        .execute(&self.writer)
        .await?;

        for migration in MIGRATIONS {
            self.apply_migration(migration).await?;
        }

        Ok(())
    }

    async fn apply_migration(&self, migration: &Migration) -> Result<(), sqlx::Error> {
        if self.is_migration_applied(migration.version).await? {
            return Ok(());
        }

        sqlx::raw_sql(migration.sql).execute(&self.writer).await?;

        let applied_at = chrono::Utc::now().timestamp();
        sqlx::query("INSERT INTO schema_migrations (version, name, applied_at) VALUES (?, ?, ?)")
            .bind(migration.version as i64)
            .bind(migration.name)
            .bind(applied_at)
            .execute(&self.writer)
            .await?;
        Ok(())
    }

    async fn is_migration_applied(&self, version: u32) -> Result<bool, sqlx::Error> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM schema_migrations WHERE version = ?")
                .bind(version as i64)
                .fetch_optional(&self.writer)
                .await?;
        Ok(row.is_some())
    }
}

struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

/// Schema migration v1: cognitive-memory's initial tables.
///
/// `memories`, `associations`, `events`, `embedding_cache`, `extractions`, `kv`.
/// See `docs/concepts/memory-model.md` §2 for the field-by-field rationale.
const MIGRATION_V1_INITIAL_SCHEMA: &str = r#"
CREATE TABLE memories (
    id                 TEXT PRIMARY KEY,
    user_id            TEXT NOT NULL,
    content            TEXT NOT NULL,
    category           TEXT NOT NULL,
    memory_type        TEXT NOT NULL,
    embedding          BLOB,
    embedding_provider TEXT,
    embedding_model    TEXT,
    created_at         INTEGER NOT NULL,
    last_accessed_at   INTEGER NOT NULL,
    valid_from         INTEGER,
    valid_until        INTEGER,
    ttl_seconds        INTEGER,
    retention_floor    REAL NOT NULL DEFAULT 0.0,
    retrieval_count    INTEGER NOT NULL DEFAULT 0,
    metadata           TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX idx_memories_user_id ON memories(user_id);
CREATE INDEX idx_memories_user_category ON memories(user_id, category);
CREATE INDEX idx_memories_user_type ON memories(user_id, memory_type);

CREATE TABLE associations (
    source_memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    target_memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    weight           REAL NOT NULL,
    kind             TEXT NOT NULL,
    updated_at       INTEGER NOT NULL,
    PRIMARY KEY (source_memory_id, target_memory_id)
);

CREATE INDEX idx_associations_target ON associations(target_memory_id);

CREATE TABLE events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL,
    payload     TEXT NOT NULL,
    occurred_at INTEGER NOT NULL
);

CREATE INDEX idx_events_kind_time ON events(kind, occurred_at);

CREATE TABLE embedding_cache (
    provider  TEXT NOT NULL,
    model     TEXT NOT NULL,
    text_hash BLOB NOT NULL,
    embedding BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (provider, model, text_hash)
);

CREATE TABLE extractions (
    input_hash BLOB NOT NULL,
    provider   TEXT NOT NULL,
    model      TEXT NOT NULL,
    output     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (input_hash, provider, model)
);

CREATE TABLE kv (
    namespace TEXT NOT NULL,
    key       TEXT NOT NULL,
    value     TEXT NOT NULL,
    PRIMARY KEY (namespace, key)
);
"#;

/// Schema migration v2: FTS5 virtual table over `memories.content` plus
/// triggers that keep it in sync with INSERT / UPDATE / DELETE on the base
/// table. Powers BM25 hybrid retrieval (Phase 3 v2 / search crate).
///
/// Note: external content tables (`content='memories'`) would save space,
/// but the trigger maintenance is identical and we keep things explicit.
const MIGRATION_V2_FTS5: &str = r#"
CREATE VIRTUAL TABLE memories_fts USING fts5(
    id UNINDEXED,
    content,
    tokenize = 'porter unicode61'
);

CREATE TRIGGER memories_after_insert AFTER INSERT ON memories BEGIN
    INSERT INTO memories_fts(id, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER memories_after_update AFTER UPDATE OF content ON memories BEGIN
    DELETE FROM memories_fts WHERE id = old.id;
    INSERT INTO memories_fts(id, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER memories_after_delete AFTER DELETE ON memories BEGIN
    DELETE FROM memories_fts WHERE id = old.id;
END;
"#;

/// Schema migration v3: paper-faithful Memory fields (§3.2 Table 1).
///
/// Adds columns the v6 SDK exposes that v1 omitted:
///   - lifecycle scoring: importance, stability
///   - tiered storage: is_cold, cold_since, days_at_floor, is_stub, stub_content
///   - consolidation: is_superseded, superseded_by, contradicted_by
///   - core promotion: session_ids (JSON array of distinct session IDs)
///
/// All columns are nullable or have defaults so the migration is safe on a
/// populated v2 store.
const MIGRATION_V3_FULL_MEMORY_FIELDS: &str = r#"
ALTER TABLE memories ADD COLUMN importance REAL NOT NULL DEFAULT 0.0;
ALTER TABLE memories ADD COLUMN stability REAL NOT NULL DEFAULT 0.5;
ALTER TABLE memories ADD COLUMN is_cold INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memories ADD COLUMN cold_since INTEGER;
ALTER TABLE memories ADD COLUMN days_at_floor INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memories ADD COLUMN is_superseded INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memories ADD COLUMN superseded_by TEXT;
ALTER TABLE memories ADD COLUMN is_stub INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memories ADD COLUMN stub_content TEXT;
ALTER TABLE memories ADD COLUMN contradicted_by TEXT;
ALTER TABLE memories ADD COLUMN session_ids TEXT NOT NULL DEFAULT '[]';

CREATE INDEX idx_memories_user_cold ON memories(user_id, is_cold);
CREATE INDEX idx_memories_user_stub ON memories(user_id, is_stub);
CREATE INDEX idx_memories_user_superseded ON memories(user_id, is_superseded);
"#;

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_schema",
        sql: MIGRATION_V1_INITIAL_SCHEMA,
    },
    Migration {
        version: 2,
        name: "fts5_for_hybrid_search",
        sql: MIGRATION_V2_FTS5,
    },
    Migration {
        version: 3,
        name: "full_memory_fields",
        sql: MIGRATION_V3_FULL_MEMORY_FIELDS,
    },
];
