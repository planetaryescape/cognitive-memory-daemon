# Memory model

How the daemon represents memories, how it differs from the SDK's in-process model, and when each deployment shape fits.

## 1. The two deployment shapes

`cognitive-memory` ships in two deployment shapes that share an algorithm and a data model but differ in process topology.

| Shape | What runs | Who uses it | When it fits |
| --- | --- | --- | --- |
| **Library mode** (existing SDK) | Your app process imports the SDK and instantiates `CognitiveMemory(...)`. Adapter (InMemory, SQLite, Postgres, Convex, …) is owned by your process. | App developers embedding cognitive memory inside a single application. | One process owns the memory. No other process needs to read or write it. You're shipping a product feature, not a per-user tool. |
| **Daemon mode** (this repo) | One always-on `cm-daemon` per OS user. Multiple AI agents (Claude Code, Cursor, scripts, the SDK with `RemoteAdapter`) connect to it over a Unix socket. | The user (you, the developer) wants their agents on their machine to share one memory across tools and projects. | Many processes need to read and write the same memory. You want centralised LLM/embedding/cache. You want agents to subscribe to memory updates. |

The two shapes are not a migration path. They are different deployments of the same project. Library mode does not become daemon mode by adding `RemoteAdapter` — `RemoteAdapter` is the SDK's *client* of daemon mode.

## 2. Data model

The daemon stores three kinds of records.

### 2.1 Memories

Every memory is a typed record with content, classification, lifecycle metadata, and provenance.

| Field | Type | Notes |
| --- | --- | --- |
| `id` | `MemoryId` | ULID. |
| `user_id` | `UserId` | Tenancy key. Every read and write is scoped by it. |
| `content` | `string` | The memory's natural-language form. |
| `category` | `Category` | `episodic` / `semantic` / `procedural` / `core`. From the existing SDK. CoALA-aligned plus `core` for retention-floored identity. |
| `memory_type` | `MemoryType` | `fact` / `preference` / `plan` / `transient_state` / `other`. From v6 spec. Orthogonal to `category`. |
| `embedding` | `Vec<f32>` or vector-rowid | 384-dim by default (`bge-small-en-v1.5`). Hosted-provider embeddings can be other dims; cache key includes provider+model. |
| `created_at` | `Timestamp` | UTC. |
| `last_accessed_at` | `Timestamp` | Updated on retrieval. Drives reinforcement. |
| `valid_from` | `Timestamp?` | Optional. Memory not visible to default retrieval before this time. |
| `valid_until` | `Timestamp?` | Optional. Memory hard-filtered after this time unless `deep_recall=true`. |
| `ttl_seconds` | `u64?` | Optional. Equivalent to setting `valid_until = created_at + ttl`. |
| `retention_floor` | `f32` | Lower bound on `R`. Default 0.0; core memories use a configured floor (default 0.6). |
| `retrieval_count` | `u64` | Counter driving emergent core promotion. |
| `metadata` | `JSON` | Free-form: `project`, `source_agent`, `tags`, anything the client wants searchable as a filter. |

### 2.2 Associations

A weighted directed edge between two memories.

| Field | Type | Notes |
| --- | --- | --- |
| `source_memory_id` | `MemoryId` | |
| `target_memory_id` | `MemoryId` | |
| `weight` | `f32` | 0.0–1.0. Decays with the memory it links to. |
| `kind` | `AssociationKind` | `cooccurrence` / `inferred` / `explicit`. |
| `updated_at` | `Timestamp` | |

### 2.3 Events

An append-only event log used for lifecycle, undo, pub/sub replay, and forensics.

| Field | Type | Notes |
| --- | --- | --- |
| `id` | `u64` | Monotonic per daemon. |
| `kind` | `EventKind` | See `PROTOCOL.md` §6. |
| `payload` | `JSON` | Kind-specific. |
| `occurred_at` | `Timestamp` | |

## 3. Tenancy: `user_id` and `project`

`user_id` is the *only* hard-isolating dimension. Two `user_id`s are guaranteed not to bleed; the SDK's existing multi-tenancy contract carries through unchanged.

`project` is **metadata**, not a tenant. By default a search under `user_id = "default"` returns hits across every project. To narrow, attach `filters.metadata.project = "lazydap"` to the request. This matches the user-stated intent: agents share memory across projects under one identity.

`source_agent` (e.g. `"claude-code"`, `"cursor"`, `"my-script"`) is also metadata. The daemon does not enforce that agents only see their own writes. If an agent wants that isolation, it filters on `metadata.source_agent`.

## 4. Lifecycle in storage terms

The v6 retention formula `R = max(floor, exp(-Δt / (S · B · β_c)))` is computed at retrieval time, not stored. Retrieval reads `(last_accessed_at, retrieval_count, retention_floor)` and computes `R` for ranking. The daemon does not periodically rewrite `R` into rows; it materialises stats only on `Lifecycle::Tick { synchronous: true }` for diagnostics.

Consolidation is reversible: a consolidated memory is a *new* row linked back to the source rows via `associations` of kind `inferred`. The originals are not deleted.

Expiry is logical by default. Expired transient memories remain in the table but are filtered out of default retrieval. `Lifecycle::Expire { mode: "purge" }` is the only path that physically deletes rows.

Promotion is a write to `retention_floor` and (sometimes) `category = "core"`. The decision lives in `crates/lifecycle/src/promote.rs`.

## 5. SQLite specifics

- One file: `data.db` next to the socket.
- WAL mode, `synchronous = NORMAL`, `foreign_keys = ON`, `temp_store = MEMORY`.
- Two pools (1 writer + 4 readers). Writes serialise inside the daemon; reads parallelise. Per-connection PRAGMAs set in pool init.
- Vector storage: undecided in this doc; ADR lands in Phase 1. Default plan is dense blob in `memories.embedding` plus Rust-side cosine for queries; upgrade to `sqlite-vec` extension when query latency requires it.
- Backup: `data.db` is portable. Copy with the daemon stopped; copying live needs SQLite's `.backup` API or a WAL-aware tool. Add `cm doctor backup` in Phase 13.

## 6. What this model does not include

- No version history per memory. An update overwrites. The event log records that an update happened with old/new payload; full diff history is recoverable from the event log if anyone ever needs it.
- No per-memory ACLs. Tenancy is `user_id`-flat.
- No relations across `user_id`. A memory under one tenant cannot link to a memory under another.
- No durable event subscription replay in v1. A reconnecting client gets the *current* state, not a stream of missed events.
