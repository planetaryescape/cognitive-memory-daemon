//! Repositories for cognitive-memory's storage layer.
//!
//! One struct per table. Methods take `&self` plus owned arguments where
//! relevant; pool refs come from the `Store` passed at construction time.

// SQL query construction uses `iter::repeat("?").take(n).join(",")` to build
// placeholder lists; clippy suggests `repeat_n` but the version we depend on
// doesn't expose it stably. The clamp-like patterns are intentional —
// .min(1.0).max(0.0) reads more clearly here than .clamp().
#![allow(clippy::manual_repeat_n, clippy::manual_clamp)]

use crate::Store;
use sqlx::SqlitePool;

/// Search candidate: trimmed columns the search layer needs to score and
/// surface a memory. Cheaper than a full `MemoryRow`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SearchCandidate {
    pub id: String,
    pub content: String,
    pub embedding: Vec<u8>,
    pub category: String,
    pub memory_type: String,
    pub last_accessed_at: i64,
    pub created_at: i64,
    pub retention_floor: f64,
    pub retrieval_count: i64,
    /// Retention-time inputs needed for R^α weighting (Eq. 3) and
    /// graph-expansion path scoring. Cheap to read since these are
    /// already on the row.
    pub stability: f64,
    pub importance: f64,
    pub is_stub: bool,
}

impl SearchCandidate {
    /// Decode the embedding blob (little-endian f32 sequence) into a vector.
    pub fn embedding_vec(&self) -> Vec<f32> {
        self.embedding
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }
}

/// A row in the `memories` table — full v6 surface (paper §3.2 Table 1).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct MemoryRow {
    pub id: String,
    pub user_id: String,
    pub content: String,
    pub category: String,
    pub memory_type: String,
    pub embedding: Option<Vec<u8>>,
    pub embedding_provider: Option<String>,
    pub embedding_model: Option<String>,
    pub created_at: i64,
    pub last_accessed_at: i64,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub ttl_seconds: Option<i64>,
    pub retention_floor: f64,
    pub retrieval_count: i64,
    pub metadata: String,
    /// v3 fields:
    pub importance: f64,
    pub stability: f64,
    pub is_cold: bool,
    pub cold_since: Option<i64>,
    pub days_at_floor: i64,
    pub is_superseded: bool,
    pub superseded_by: Option<String>,
    pub is_stub: bool,
    pub stub_content: Option<String>,
    pub contradicted_by: Option<String>,
    /// JSON array of distinct session ids.
    pub session_ids: String,
}

impl MemoryRow {
    /// Build a minimal row for a fresh memory with sensible defaults.
    /// Caller fills `id`, `user_id`, `content`, classification, and
    /// timestamps; lifecycle fields default to v6-spec values.
    pub fn new_minimal(
        id: impl Into<String>,
        user_id: impl Into<String>,
        content: impl Into<String>,
        category: impl Into<String>,
        memory_type: impl Into<String>,
        now: i64,
    ) -> Self {
        Self {
            id: id.into(),
            user_id: user_id.into(),
            content: content.into(),
            category: category.into(),
            memory_type: memory_type.into(),
            embedding: None,
            embedding_provider: None,
            embedding_model: None,
            created_at: now,
            last_accessed_at: now,
            valid_from: None,
            valid_until: None,
            ttl_seconds: None,
            retention_floor: 0.0,
            retrieval_count: 0,
            metadata: "{}".to_string(),
            importance: 0.0,
            // SDK: stability = 0.1 + 0.3 * importance (core.py:126).
            // Default importance=0.0 ⇒ stability=0.1. Callers that
            // override importance after construction must also call
            // `stability_from_importance` to keep the invariant.
            stability: 0.1,
            is_cold: false,
            cold_since: None,
            days_at_floor: 0,
            is_superseded: false,
            superseded_by: None,
            is_stub: false,
            stub_content: None,
            contradicted_by: None,
            session_ids: "[]".to_string(),
        }
    }
}

/// Filters for listing/querying memories. Mirrors the SDK's `MemoryFilters`.
#[derive(Debug, Clone, Default)]
pub struct MemoryFilters {
    pub categories: Option<Vec<String>>,
    pub memory_types: Option<Vec<String>>,
    pub min_retention_floor: Option<f64>,
    pub min_importance: Option<f64>,
    pub created_after: Option<i64>,
    pub created_before: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub include_superseded: bool,
    pub include_cold: bool,
    pub include_stubs: bool,
}

const ALL_FIELDS: &str = "id, user_id, content, category, memory_type, embedding,
    embedding_provider, embedding_model, created_at, last_accessed_at,
    valid_from, valid_until, ttl_seconds, retention_floor, retrieval_count,
    metadata, importance, stability, is_cold, cold_since, days_at_floor,
    is_superseded, superseded_by, is_stub, stub_content, contradicted_by,
    session_ids";

/// Repository over the `memories` table.
pub struct MemoryRepo<'a> {
    writer: &'a SqlitePool,
    reader: &'a SqlitePool,
}

impl<'a> MemoryRepo<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            writer: store.writer(),
            reader: store.reader(),
        }
    }

    pub async fn insert(&self, row: &MemoryRow) -> Result<(), sqlx::Error> {
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
        .bind(&row.id).bind(&row.user_id).bind(&row.content)
        .bind(&row.category).bind(&row.memory_type)
        .bind(row.embedding.as_deref())
        .bind(row.embedding_provider.as_deref())
        .bind(row.embedding_model.as_deref())
        .bind(row.created_at).bind(row.last_accessed_at)
        .bind(row.valid_from).bind(row.valid_until).bind(row.ttl_seconds)
        .bind(row.retention_floor).bind(row.retrieval_count).bind(&row.metadata)
        .bind(row.importance).bind(row.stability)
        .bind(row.is_cold as i64).bind(row.cold_since).bind(row.days_at_floor)
        .bind(row.is_superseded as i64).bind(row.superseded_by.as_deref())
        .bind(row.is_stub as i64).bind(row.stub_content.as_deref())
        .bind(row.contradicted_by.as_deref()).bind(&row.session_ids)
        .execute(self.writer).await?;
        Ok(())
    }

    pub async fn get_for_user(
        &self,
        user_id: &str,
        id: &str,
    ) -> Result<Option<MemoryRow>, sqlx::Error> {
        let sql = format!("SELECT {ALL_FIELDS} FROM memories WHERE user_id = ? AND id = ?");
        sqlx::query_as::<_, MemoryRow>(&sql)
            .bind(user_id)
            .bind(id)
            .fetch_optional(self.reader)
            .await
    }

    /// Fetch many memories by id under a single user. Order matches input.
    pub async fn get_many_for_user(
        &self,
        user_id: &str,
        ids: &[String],
    ) -> Result<Vec<MemoryRow>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {ALL_FIELDS} FROM memories
             WHERE user_id = ? AND id IN ({placeholders})"
        );
        let mut q = sqlx::query_as::<_, MemoryRow>(&sql).bind(user_id);
        for id in ids {
            q = q.bind(id);
        }
        let mut rows = q.fetch_all(self.reader).await?;
        // Re-order to match input.
        rows.sort_by_key(|r| ids.iter().position(|id| id == &r.id).unwrap_or(usize::MAX));
        Ok(rows)
    }

    /// List memories matching `filters`. Returns Vec<MemoryRow>.
    pub async fn query(
        &self,
        user_id: &str,
        filters: &MemoryFilters,
    ) -> Result<Vec<MemoryRow>, sqlx::Error> {
        let mut where_clauses = vec!["user_id = ?".to_string()];
        if !filters.include_cold {
            where_clauses.push("is_cold = 0".to_string());
        }
        if !filters.include_stubs {
            where_clauses.push("is_stub = 0".to_string());
        }
        if !filters.include_superseded {
            where_clauses.push("is_superseded = 0".to_string());
        }
        if let Some(cats) = &filters.categories {
            if !cats.is_empty() {
                let ph = std::iter::repeat("?")
                    .take(cats.len())
                    .collect::<Vec<_>>()
                    .join(",");
                where_clauses.push(format!("category IN ({ph})"));
            }
        }
        if let Some(types) = &filters.memory_types {
            if !types.is_empty() {
                let ph = std::iter::repeat("?")
                    .take(types.len())
                    .collect::<Vec<_>>()
                    .join(",");
                where_clauses.push(format!("memory_type IN ({ph})"));
            }
        }
        if filters.min_retention_floor.is_some() {
            where_clauses.push("retention_floor >= ?".to_string());
        }
        if filters.min_importance.is_some() {
            where_clauses.push("importance >= ?".to_string());
        }
        if filters.created_after.is_some() {
            where_clauses.push("created_at >= ?".to_string());
        }
        if filters.created_before.is_some() {
            where_clauses.push("created_at <= ?".to_string());
        }

        let where_sql = where_clauses.join(" AND ");
        let limit_sql = filters
            .limit
            .map(|l| format!(" LIMIT {l}"))
            .unwrap_or_default();
        let offset_sql = filters
            .offset
            .map(|o| format!(" OFFSET {o}"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT {ALL_FIELDS} FROM memories WHERE {where_sql}
             ORDER BY created_at DESC{limit_sql}{offset_sql}"
        );

        let mut q = sqlx::query_as::<_, MemoryRow>(&sql).bind(user_id);
        if let Some(cats) = &filters.categories {
            for c in cats {
                q = q.bind(c);
            }
        }
        if let Some(types) = &filters.memory_types {
            for t in types {
                q = q.bind(t);
            }
        }
        if let Some(v) = filters.min_retention_floor {
            q = q.bind(v);
        }
        if let Some(v) = filters.min_importance {
            q = q.bind(v);
        }
        if let Some(v) = filters.created_after {
            q = q.bind(v);
        }
        if let Some(v) = filters.created_before {
            q = q.bind(v);
        }
        q.fetch_all(self.reader).await
    }

    /// Update arbitrary fields. The caller passes a `MemoryUpdate` struct
    /// where `Some(...)` fields are written and `None` are left alone.
    pub async fn update(
        &self,
        user_id: &str,
        id: &str,
        upd: &MemoryUpdate,
    ) -> Result<bool, sqlx::Error> {
        let mut sets = Vec::new();
        if upd.content.is_some() {
            sets.push("content = ?");
        }
        if upd.category.is_some() {
            sets.push("category = ?");
        }
        if upd.memory_type.is_some() {
            sets.push("memory_type = ?");
        }
        if upd.metadata.is_some() {
            sets.push("metadata = ?");
        }
        if upd.retention_floor.is_some() {
            sets.push("retention_floor = ?");
        }
        if upd.importance.is_some() {
            sets.push("importance = ?");
        }
        if upd.stability.is_some() {
            sets.push("stability = ?");
        }
        if upd.valid_until.is_some() {
            sets.push("valid_until = ?");
        }
        if sets.is_empty() {
            return Ok(false);
        }
        let sql = format!(
            "UPDATE memories SET {} WHERE user_id = ? AND id = ?",
            sets.join(", ")
        );
        let mut q = sqlx::query(&sql);
        if let Some(v) = &upd.content {
            q = q.bind(v);
        }
        if let Some(v) = &upd.category {
            q = q.bind(v);
        }
        if let Some(v) = &upd.memory_type {
            q = q.bind(v);
        }
        if let Some(v) = &upd.metadata {
            q = q.bind(v);
        }
        if let Some(v) = upd.retention_floor {
            q = q.bind(v);
        }
        if let Some(v) = upd.importance {
            q = q.bind(v);
        }
        if let Some(v) = upd.stability {
            q = q.bind(v);
        }
        if let Some(v) = upd.valid_until {
            q = q.bind(v);
        }
        q = q.bind(user_id).bind(id);
        let result = q.execute(self.writer).await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete(&self, user_id: &str, id: &str) -> Result<bool, sqlx::Error> {
        let r = sqlx::query("DELETE FROM memories WHERE user_id = ? AND id = ?")
            .bind(user_id)
            .bind(id)
            .execute(self.writer)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn delete_many(&self, user_id: &str, ids: &[String]) -> Result<u64, sqlx::Error> {
        if ids.is_empty() {
            return Ok(0);
        }
        let ph = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM memories WHERE user_id = ? AND id IN ({ph})");
        let mut q = sqlx::query(&sql).bind(user_id);
        for id in ids {
            q = q.bind(id);
        }
        Ok(q.execute(self.writer).await?.rows_affected())
    }

    /// Bump stability by `amount` (capped at 1.0) for one memory.
    /// Used by the ingest-side stability-reinforcement path
    /// (cognitive_memory/core.py:222-224) when a near-duplicate
    /// (sim ∈ (0.75, 0.85)) is stored.
    pub async fn reinforce_stability(
        &self,
        user_id: &str,
        id: &str,
        amount: f64,
    ) -> Result<u64, sqlx::Error> {
        let r = sqlx::query(
            "UPDATE memories
             SET stability = MIN(1.0, stability + ?)
             WHERE user_id = ? AND id = ?",
        )
        .bind(amount)
        .bind(user_id)
        .bind(id)
        .execute(self.writer)
        .await?;
        Ok(r.rows_affected())
    }

    /// Apply direct-boost effects to a list of retrieved memories in
    /// one transaction. Mirrors `_apply_direct_boost` in
    /// `engine.py:148-160`. For each memory:
    ///   - access_count += 1
    ///   - last_accessed_at = now
    ///   - stability = min(1.0, stability + boost)
    ///   - session_ids: append `session_id` if provided and not present
    ///
    /// `boosts` is `(id, new_stability)` — the caller computes the
    /// spaced-rep multiplier from each memory's `last_accessed_at`
    /// since the formula needs per-memory dt.
    pub async fn apply_direct_boost(
        &self,
        user_id: &str,
        boosts: &[(String, f64)],
        now: i64,
        session_id: Option<&str>,
    ) -> Result<u64, sqlx::Error> {
        if boosts.is_empty() {
            return Ok(0);
        }
        let mut tx = self.writer.begin().await?;
        let mut total = 0_u64;
        for (id, new_stability) in boosts {
            // Update stability + access counters + last_accessed_at.
            let r = sqlx::query(
                "UPDATE memories
                 SET stability = ?,
                     last_accessed_at = ?,
                     retrieval_count = retrieval_count + 1
                 WHERE user_id = ? AND id = ?",
            )
            .bind(new_stability)
            .bind(now)
            .bind(user_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            total += r.rows_affected();

            // Append session_id to the JSON array if it's not already
            // in there. SQLite has json_each / json_array_append in
            // newer versions, but we keep this simple and read-modify-
            // write the JSON in-process.
            if let Some(sid) = session_id {
                let row: Option<(String,)> =
                    sqlx::query_as("SELECT session_ids FROM memories WHERE user_id = ? AND id = ?")
                        .bind(user_id)
                        .bind(id)
                        .fetch_optional(&mut *tx)
                        .await?;
                if let Some((current,)) = row {
                    // session_ids is JSON array of strings.
                    let mut sessions: Vec<String> =
                        serde_json::from_str(&current).unwrap_or_default();
                    if !sessions.iter().any(|s| s == sid) {
                        sessions.push(sid.to_string());
                        let new_json =
                            serde_json::to_string(&sessions).unwrap_or_else(|_| "[]".to_string());
                        sqlx::query(
                            "UPDATE memories SET session_ids = ? WHERE user_id = ? AND id = ?",
                        )
                        .bind(new_json)
                        .bind(user_id)
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    }
                }
            }
        }
        tx.commit().await?;
        Ok(total)
    }

    /// Promote memories to the `core` category atomically. Sets
    /// `category = 'core'` and `retention_floor = 0.6`. Idempotent
    /// (already-core memories are skipped at the SQL level by the
    /// caller).
    pub async fn promote_to_core(&self, user_id: &str, ids: &[String]) -> Result<u64, sqlx::Error> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut tx = self.writer.begin().await?;
        let mut total = 0_u64;
        for id in ids {
            let r = sqlx::query(
                "UPDATE memories
                 SET category = 'core', retention_floor = 0.6
                 WHERE user_id = ? AND id = ? AND category != 'core'",
            )
            .bind(user_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            total += r.rows_affected();
        }
        tx.commit().await?;
        Ok(total)
    }

    /// Apply a batch of `(id, retention_floor)` updates atomically.
    pub async fn update_retention_scores(
        &self,
        user_id: &str,
        updates: &[(String, f64)],
    ) -> Result<u64, sqlx::Error> {
        if updates.is_empty() {
            return Ok(0);
        }
        let mut tx = self.writer.begin().await?;
        let mut total = 0_u64;
        for (id, score) in updates {
            let r =
                sqlx::query("UPDATE memories SET retention_floor = ? WHERE user_id = ? AND id = ?")
                    .bind(score)
                    .bind(user_id)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            total += r.rows_affected();
        }
        tx.commit().await?;
        Ok(total)
    }

    /// Mark memories as superseded by a summary.
    pub async fn mark_superseded(
        &self,
        user_id: &str,
        ids: &[String],
        summary_id: &str,
    ) -> Result<u64, sqlx::Error> {
        if ids.is_empty() {
            return Ok(0);
        }
        let ph = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE memories SET is_superseded = 1, superseded_by = ?
             WHERE user_id = ? AND id IN ({ph})"
        );
        let mut q = sqlx::query(&sql).bind(summary_id).bind(user_id);
        for id in ids {
            q = q.bind(id);
        }
        Ok(q.execute(self.writer).await?.rows_affected())
    }

    pub async fn migrate_to_cold(
        &self,
        user_id: &str,
        id: &str,
        cold_since: i64,
    ) -> Result<bool, sqlx::Error> {
        let r = sqlx::query(
            "UPDATE memories SET is_cold = 1, cold_since = ?
             WHERE user_id = ? AND id = ?",
        )
        .bind(cold_since)
        .bind(user_id)
        .bind(id)
        .execute(self.writer)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn migrate_to_hot(&self, user_id: &str, id: &str) -> Result<bool, sqlx::Error> {
        // Reset all three cold-state fields, mirroring SDK adapter
        // contract (base.py:116). days_at_floor must zero too — a
        // restored memory starts the at-floor counter fresh.
        let r = sqlx::query(
            "UPDATE memories
             SET is_cold = 0, cold_since = NULL, days_at_floor = 0
             WHERE user_id = ? AND id = ?",
        )
        .bind(user_id)
        .bind(id)
        .execute(self.writer)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Bulk variant: restore many memories from cold in one tx.
    /// Used by the search/get auto-restore paths so a single read
    /// touching multiple cold rows doesn't deadlock the writer pool.
    pub async fn migrate_to_hot_many(
        &self,
        user_id: &str,
        ids: &[String],
    ) -> Result<u64, sqlx::Error> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut tx = self.writer.begin().await?;
        let mut total = 0u64;
        for id in ids {
            let r = sqlx::query(
                "UPDATE memories
                 SET is_cold = 0, cold_since = NULL, days_at_floor = 0
                 WHERE user_id = ? AND id = ?",
            )
            .bind(user_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            total += r.rows_affected();
        }
        tx.commit().await?;
        Ok(total)
    }

    pub async fn convert_to_stub(
        &self,
        user_id: &str,
        id: &str,
        stub_content: &str,
    ) -> Result<bool, sqlx::Error> {
        let r = sqlx::query(
            "UPDATE memories SET is_stub = 1, stub_content = ?
             WHERE user_id = ? AND id = ?",
        )
        .bind(stub_content)
        .bind(user_id)
        .bind(id)
        .execute(self.writer)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Fetch the candidate set the lifecycle layer will scan for fading
    /// memories. Returns all hot, non-superseded, non-stub rows under
    /// the user. The caller computes retention(now) per row and filters
    /// by the threshold — power-law decay can't be expressed in SQL, so
    /// computation moves up the stack.
    pub async fn find_fading_candidates(
        &self,
        user_id: &str,
    ) -> Result<Vec<MemoryRow>, sqlx::Error> {
        let sql = format!(
            "SELECT {ALL_FIELDS} FROM memories
             WHERE user_id = ? AND is_superseded = 0 AND is_stub = 0
               AND is_cold = 0
             ORDER BY last_accessed_at ASC"
        );
        sqlx::query_as::<_, MemoryRow>(&sql)
            .bind(user_id)
            .fetch_all(self.reader)
            .await
    }

    /// Find memories with high stability + access count — core promotion
    /// candidates per paper §3.4.
    pub async fn find_stable(
        &self,
        user_id: &str,
        min_stability: f64,
        min_access_count: i64,
        limit: i64,
    ) -> Result<Vec<MemoryRow>, sqlx::Error> {
        let sql = format!(
            "SELECT {ALL_FIELDS} FROM memories
             WHERE user_id = ? AND stability >= ? AND retrieval_count >= ?
               AND is_cold = 0 AND is_stub = 0 AND is_superseded = 0
             ORDER BY stability DESC LIMIT ?"
        );
        sqlx::query_as::<_, MemoryRow>(&sql)
            .bind(user_id)
            .bind(min_stability)
            .bind(min_access_count)
            .bind(limit)
            .fetch_all(self.reader)
            .await
    }

    /// Per-user counts grouped by tier. Returns (hot, cold, stub, total).
    pub async fn counts_for_user(&self, user_id: &str) -> Result<MemoryCounts, sqlx::Error> {
        let row: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT
                SUM(CASE WHEN is_cold = 0 AND is_stub = 0 THEN 1 ELSE 0 END) AS hot,
                SUM(CASE WHEN is_cold = 1 AND is_stub = 0 THEN 1 ELSE 0 END) AS cold,
                SUM(CASE WHEN is_stub = 1 THEN 1 ELSE 0 END) AS stub,
                COUNT(*) AS total
             FROM memories WHERE user_id = ?",
        )
        .bind(user_id)
        .fetch_one(self.reader)
        .await?;
        Ok(MemoryCounts {
            hot: row.0,
            cold: row.1,
            stub: row.2,
            total: row.3,
        })
    }

    /// Delete all memories under a user_id. Useful for tests and for the
    /// SDK's `clear()` method.
    pub async fn clear_user(&self, user_id: &str) -> Result<u64, sqlx::Error> {
        let r = sqlx::query("DELETE FROM memories WHERE user_id = ?")
            .bind(user_id)
            .execute(self.writer)
            .await?;
        Ok(r.rows_affected())
    }

    /// BM25 lookup over the FTS5 virtual table, scoped by user_id.
    pub async fn bm25_search(
        &self,
        user_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<String>, sqlx::Error> {
        let safe_query = format!("\"{}\"", query.replace('"', " "));
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT m.id FROM memories_fts
             JOIN memories m ON m.id = memories_fts.id
             WHERE memories_fts MATCH ? AND m.user_id = ?
             ORDER BY bm25(memories_fts) ASC LIMIT ?",
        )
        .bind(&safe_query)
        .bind(user_id)
        .bind(limit as i64)
        .fetch_all(self.reader)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Search candidates for vector retrieval (trimmed columns).
    pub async fn candidates_for_search(
        &self,
        user_id: &str,
        provider: &str,
        model: &str,
        now: i64,
        include_expired: bool,
    ) -> Result<Vec<SearchCandidate>, sqlx::Error> {
        // `include_expired` is the daemon's `deep_recall` mode. Per SDK
        // parity (engine.py: `include_superseded = deep_recall` AND
        // `include_cold = deep_recall`) it drops three filters:
        // `is_superseded = 0`, `is_cold = 0`, and `valid_until > now`.
        // Stubs stay hidden in both modes — searchable stubs would
        // defeat their archival purpose.
        let sql = if include_expired {
            "SELECT id, content, embedding, category, memory_type, last_accessed_at,
                    created_at, retention_floor, retrieval_count,
                    stability, importance, is_stub
             FROM memories
             WHERE user_id = ? AND embedding IS NOT NULL
               AND embedding_provider = ? AND embedding_model = ?
               AND is_stub = 0
               AND (valid_from IS NULL OR valid_from <= ?)"
        } else {
            "SELECT id, content, embedding, category, memory_type, last_accessed_at,
                    created_at, retention_floor, retrieval_count,
                    stability, importance, is_stub
             FROM memories
             WHERE user_id = ? AND embedding IS NOT NULL
               AND embedding_provider = ? AND embedding_model = ?
               AND is_stub = 0 AND is_superseded = 0 AND is_cold = 0
               AND (valid_from IS NULL OR valid_from <= ?)
               AND (valid_until IS NULL OR valid_until > ?)"
        };
        let mut q = sqlx::query_as::<_, SearchCandidate>(sql)
            .bind(user_id)
            .bind(provider)
            .bind(model)
            .bind(now);
        if !include_expired {
            q = q.bind(now);
        }
        q.fetch_all(self.reader).await
    }

    pub async fn record_access(
        &self,
        user_id: &str,
        id: &str,
        accessed_at: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE memories SET last_accessed_at = ?, retrieval_count = retrieval_count + 1
             WHERE user_id = ? AND id = ?",
        )
        .bind(accessed_at)
        .bind(user_id)
        .bind(id)
        .execute(self.writer)
        .await?;
        Ok(())
    }
}

/// Partial-update payload. Fields set to `Some(...)` are written; `None`
/// fields are left alone.
#[derive(Debug, Clone, Default)]
pub struct MemoryUpdate {
    pub content: Option<String>,
    pub category: Option<String>,
    pub memory_type: Option<String>,
    pub metadata: Option<String>,
    pub retention_floor: Option<f64>,
    pub importance: Option<f64>,
    pub stability: Option<f64>,
    pub valid_until: Option<i64>,
}

/// Per-user tier counts. Mirrors SDK `hotCount`/`coldCount`/`stubCount`/`totalCount`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryCounts {
    pub hot: i64,
    pub cold: i64,
    pub stub: i64,
    pub total: i64,
}

// --- Associations ---

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AssociationRow {
    pub source_memory_id: String,
    pub target_memory_id: String,
    pub weight: f64,
    pub kind: String,
    pub updated_at: i64,
}

/// Lightweight outgoing edge — the data needed to apply read-time
/// association decay (paper Eq 10). `last_co_retrieval` is `None`
/// only for legacy rows that pre-date migration v5 and lacked an
/// `updated_at` to backfill from (rare).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NeighborEdge {
    pub target_id: String,
    pub weight: f64,
    pub last_co_retrieval: Option<i64>,
}

/// A memory plus the strength of the link that surfaced it. Mirrors the
/// SDK's `Memory & { linkStrength: number }` projection.
#[derive(Debug, Clone)]
pub struct LinkedMemory {
    pub memory: MemoryRow,
    pub link_strength: f64,
    /// `associations.last_co_retrieval` — unix seconds of the most
    /// recent co-retrieval that strengthened this edge. None for
    /// legacy edges that pre-date migration v5. Callers apply
    /// `decay_association_weight` (Eq 10) using this timestamp.
    pub last_co_retrieval: Option<i64>,
}

/// Repository over the `associations` table.
pub struct AssociationRepo<'a> {
    writer: &'a SqlitePool,
    reader: &'a SqlitePool,
}

impl<'a> AssociationRepo<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            writer: store.writer(),
            reader: store.reader(),
        }
    }

    /// Create or strengthen a directed association. If the edge exists,
    /// new weight = clamp(old + bump, 0, 1). If not, weight = bump.
    pub async fn create_or_strengthen(
        &self,
        source_id: &str,
        target_id: &str,
        bump: f64,
        now: i64,
        kind: &str,
    ) -> Result<f64, sqlx::Error> {
        let existing: Option<(f64,)> = sqlx::query_as(
            "SELECT weight FROM associations
             WHERE source_memory_id = ? AND target_memory_id = ?",
        )
        .bind(source_id)
        .bind(target_id)
        .fetch_optional(self.reader)
        .await?;

        let new_weight = match existing {
            Some((w,)) => (w + bump).min(1.0).max(0.0),
            None => bump.min(1.0).max(0.0),
        };

        sqlx::query(
            "INSERT INTO associations
                 (source_memory_id, target_memory_id, weight, kind,
                  updated_at, last_co_retrieval)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_memory_id, target_memory_id) DO UPDATE
             SET weight = excluded.weight,
                 kind = excluded.kind,
                 updated_at = excluded.updated_at,
                 last_co_retrieval = excluded.last_co_retrieval",
        )
        .bind(source_id)
        .bind(target_id)
        .bind(new_weight)
        .bind(kind)
        .bind(now)
        .bind(now)
        .execute(self.writer)
        .await?;
        Ok(new_weight)
    }

    /// Apply co-retrieval strengthening to a batch of unordered pairs
    /// in a single transaction. For each `(a, b)`: bump weight by
    /// `amount` (capped at 1.0) on both `(a→b)` and `(b→a)` and
    /// refresh `last_co_retrieval = now`. Mirrors the behaviour of
    /// engine.py:621-625 + core.py:258-262.
    ///
    /// Atomic: a partial failure rolls back the whole batch.
    pub async fn strengthen_pairs(
        &self,
        pairs: &[(String, String)],
        amount: f64,
        now: i64,
        kind: &str,
    ) -> Result<u64, sqlx::Error> {
        if pairs.is_empty() {
            return Ok(0);
        }
        let mut tx = self.writer.begin().await?;
        let mut total = 0_u64;
        for (a, b) in pairs {
            for (src, tgt) in [(a, b), (b, a)] {
                // UPSERT: insert with `amount` if new; bump on conflict.
                // Inline of create_or_strengthen so we run inside the tx
                // (the standalone repo method opens its own writer).
                let r = sqlx::query(
                    "INSERT INTO associations
                         (source_memory_id, target_memory_id, weight,
                          kind, updated_at, last_co_retrieval)
                     VALUES (?, ?, ?, ?, ?, ?)
                     ON CONFLICT(source_memory_id, target_memory_id) DO UPDATE
                     SET weight = MIN(1.0, weight + ?),
                         updated_at = excluded.updated_at,
                         last_co_retrieval = excluded.last_co_retrieval",
                )
                .bind(src)
                .bind(tgt)
                .bind(amount.min(1.0).max(0.0))
                .bind(kind)
                .bind(now)
                .bind(now)
                .bind(amount)
                .execute(&mut *tx)
                .await?;
                total += r.rows_affected();
            }
        }
        tx.commit().await?;
        Ok(total)
    }

    /// Bidirectional create-or-strengthen: applies to both (a→b) and (b→a).
    pub async fn link_bidirectional(
        &self,
        a: &str,
        b: &str,
        bump: f64,
        now: i64,
        kind: &str,
    ) -> Result<f64, sqlx::Error> {
        let w1 = self.create_or_strengthen(a, b, bump, now, kind).await?;
        let _w2 = self.create_or_strengthen(b, a, bump, now, kind).await?;
        Ok(w1)
    }

    /// Get linked memories for one source, with weight ≥ min_weight, scoped
    /// by user_id.
    ///
    /// Implementation: two queries (associations → ids+weights, then a
    /// batch fetch of the memories). Two round trips but the code stays
    /// straightforward; the alternative is a hand-rolled FromRow impl on
    /// a flattened struct, which is brittle when schema changes.
    pub async fn linked_for(
        &self,
        user_id: &str,
        source_id: &str,
        min_weight: f64,
    ) -> Result<Vec<LinkedMemory>, sqlx::Error> {
        // Need a temporary borrow of the reader pool for the second hop;
        // we reach into MemoryRepo via ad-hoc construction.
        let edges: Vec<(String, f64, Option<i64>)> = sqlx::query_as(
            "SELECT target_memory_id, weight, last_co_retrieval FROM associations
             WHERE source_memory_id = ? AND weight >= ?
             ORDER BY weight DESC",
        )
        .bind(source_id)
        .bind(min_weight)
        .fetch_all(self.reader)
        .await?;

        if edges.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<String> = edges.iter().map(|(id, _, _)| id.clone()).collect();
        let edge_meta: std::collections::HashMap<String, (f64, Option<i64>)> = edges
            .into_iter()
            .map(|(id, w, last)| (id, (w, last)))
            .collect();

        let mem_repo = MemoryRepo {
            writer: self.writer,
            reader: self.reader,
        };
        let mems = mem_repo.get_many_for_user(user_id, &ids).await?;
        Ok(mems
            .into_iter()
            .filter_map(|m| {
                let (strength, last_co) = *edge_meta.get(&m.id)?;
                Some(LinkedMemory {
                    memory: m,
                    link_strength: strength,
                    last_co_retrieval: last_co,
                })
            })
            .collect())
    }

    /// Linked memories for many sources at once. Same two-query shape.
    pub async fn linked_for_many(
        &self,
        user_id: &str,
        source_ids: &[String],
        min_weight: f64,
    ) -> Result<Vec<LinkedMemory>, sqlx::Error> {
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ph = std::iter::repeat("?")
            .take(source_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        // MAX(weight) collapses parallel edges from multiple sources;
        // MAX(last_co_retrieval) keeps the most recent co-retrieval
        // timestamp among them — strongest, freshest interpretation.
        let sql = format!(
            "SELECT target_memory_id, MAX(weight) AS w, MAX(last_co_retrieval) AS last
             FROM associations
             WHERE source_memory_id IN ({ph}) AND weight >= ?
             GROUP BY target_memory_id ORDER BY w DESC"
        );
        let mut q = sqlx::query_as::<_, (String, f64, Option<i64>)>(&sql);
        for id in source_ids {
            q = q.bind(id);
        }
        q = q.bind(min_weight);
        let edges: Vec<(String, f64, Option<i64>)> = q.fetch_all(self.reader).await?;

        if edges.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = edges.iter().map(|(id, _, _)| id.clone()).collect();
        let edge_meta: std::collections::HashMap<String, (f64, Option<i64>)> = edges
            .into_iter()
            .map(|(id, w, last)| (id, (w, last)))
            .collect();

        let mem_repo = MemoryRepo {
            writer: self.writer,
            reader: self.reader,
        };
        let mems = mem_repo.get_many_for_user(user_id, &ids).await?;
        Ok(mems
            .into_iter()
            .filter_map(|m| {
                let (strength, last_co) = *edge_meta.get(&m.id)?;
                Some(LinkedMemory {
                    memory: m,
                    link_strength: strength,
                    last_co_retrieval: last_co,
                })
            })
            .collect())
    }

    /// Lightweight neighbor query for graph-walking algorithms (bridge
    /// BFS, graph expansion). Returns one `NeighborEdge` per outgoing
    /// edge — the data needed to apply Eq 10 read-side decay
    /// `w * exp(-Δt_days / 90)`. Sorted by stored weight desc so the
    /// strongest edges are visited first; the caller may re-sort by
    /// decayed weight if order matters after decay.
    pub async fn neighbor_edges(
        &self,
        source_id: &str,
        min_weight: f64,
    ) -> Result<Vec<NeighborEdge>, sqlx::Error> {
        sqlx::query_as(
            "SELECT target_memory_id AS target_id, weight, last_co_retrieval
             FROM associations
             WHERE source_memory_id = ? AND weight >= ?
             ORDER BY weight DESC",
        )
        .bind(source_id)
        .bind(min_weight)
        .fetch_all(self.reader)
        .await
    }

    /// Delete a directed link.
    pub async fn delete(&self, source_id: &str, target_id: &str) -> Result<bool, sqlx::Error> {
        let r = sqlx::query(
            "DELETE FROM associations WHERE source_memory_id = ? AND target_memory_id = ?",
        )
        .bind(source_id)
        .bind(target_id)
        .execute(self.writer)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Delete bidirectional link (both (a→b) and (b→a)).
    pub async fn delete_bidirectional(&self, a: &str, b: &str) -> Result<u64, sqlx::Error> {
        let r = sqlx::query(
            "DELETE FROM associations
             WHERE (source_memory_id = ? AND target_memory_id = ?)
                OR (source_memory_id = ? AND target_memory_id = ?)",
        )
        .bind(a)
        .bind(b)
        .bind(b)
        .bind(a)
        .execute(self.writer)
        .await?;
        Ok(r.rows_affected())
    }
}

// --- Embedding cache (unchanged) ---

pub struct EmbeddingCacheRepo<'a> {
    writer: &'a SqlitePool,
    reader: &'a SqlitePool,
}

impl<'a> EmbeddingCacheRepo<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            writer: store.writer(),
            reader: store.reader(),
        }
    }

    pub async fn insert(
        &self,
        provider: &str,
        model: &str,
        text_hash: &[u8],
        vector: &[f32],
    ) -> Result<bool, sqlx::Error> {
        let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
        let now = chrono::Utc::now().timestamp();
        let result = sqlx::query(
            "INSERT OR IGNORE INTO embedding_cache (provider, model, text_hash, embedding, created_at)
             VALUES (?, ?, ?, ?, ?)"
        )
        .bind(provider).bind(model).bind(text_hash).bind(bytes).bind(now)
        .execute(self.writer).await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn get(
        &self,
        provider: &str,
        model: &str,
        text_hash: &[u8],
    ) -> Result<Option<Vec<f32>>, sqlx::Error> {
        let row: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT embedding FROM embedding_cache
             WHERE provider = ? AND model = ? AND text_hash = ?",
        )
        .bind(provider)
        .bind(model)
        .bind(text_hash)
        .fetch_optional(self.reader)
        .await?;
        Ok(row.map(|(bytes,)| {
            bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        }))
    }
}

// --- Event log (unchanged) ---

pub struct EventLogRepo<'a> {
    writer: &'a SqlitePool,
}

impl<'a> EventLogRepo<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            writer: store.writer(),
        }
    }

    pub async fn append(
        &self,
        kind: &str,
        payload_json: &str,
        occurred_at: i64,
    ) -> Result<i64, sqlx::Error> {
        let result =
            sqlx::query("INSERT INTO events (kind, payload, occurred_at) VALUES (?, ?, ?)")
                .bind(kind)
                .bind(payload_json)
                .bind(occurred_at)
                .execute(self.writer)
                .await?;
        Ok(result.last_insert_rowid())
    }
}
