# Architecture

This document is the load-bearing blueprint for `cognitive-memory-daemon`. Every other doc in the repo refines a slice of what's here. Read it once before touching code.

## 0. The load-bearing claim

**The daemon is the memory. SDKs, CLIs, agents, and the HTTP bridge are clients.**

That one sentence is the architecture. Everything below is how it manifests. Three supporting claims operationalise it:

- **SQLite is the source of truth. Embedding cache, trace ring buffer, and in-RAM model are derived and rebuildable.** Anything derivable from SQLite can be discarded and rebuilt without data loss. Crash recovery falls out of this for free.
- **The daemon serves reusable truth and lifecycle, not response payloads shaped for any particular UI.** The IPC contract describes facts a thinking client wants, not what a screen needs to render. Client-specific concerns (pane state, view shaping, selection state) never cross the wire.
- **Provider differences (LLM and embedding) are absorbed below the protocol surface in provider crates, but capability differences stay visible where behaviour actually differs.** A query that requires a paid model surfaces that, it does not silently fall back. Provider-agnostic at the data layer is not the same as flattening real behavioural differences.

The 12 non-negotiable principles in [`AGENTS.md` §2](./AGENTS.md) operationalise this claim. In one breath:

1. Daemon-backed (the claim above).
2. Local-first.
3. One canonical truth; derived caches are rebuildable.
4. Protocol-first.
5. Lifecycle is a first-class concern.
6. Cross-agent visibility by default.
7. Provider-agnostic internal model.
8. Single writer to SQLite.
9. Operable without provider keys.
10. CLI-first.
11. Pipeable structured output.
12. Mutations are previewable; destructive mutations are reversible within a window.

Build-time enforcement of principles 1, 4, and 8 lives in `AGENTS.md` §3 (architectural rules checked by Cargo and CI).

## 1. Goals and non-goals

### Goals

1. **One memory, many agents.** Multiple AI clients on the same machine — Claude Code, Cursor, scripts, the TS and Python SDKs in remote mode — read and write a single shared cognitive memory store, with cross-agent visibility under a shared `user_id` namespace.
2. **Local-first, owner-only.** Memory lives on the user's machine. The Unix socket and runtime directory are reachable only by the local user. No code path silently sends memory contents off-machine except through user-configured LLM and embedding providers.
3. **Always-on.** The daemon survives client restarts, idle periods, and OS suspends. State is durable across daemon restarts; only in-flight session state is ephemeral.
4. **Stable wire protocol.** Clients and the daemon evolve independently behind a versioned, language-agnostic IPC contract. The protocol is the public surface; everything else is an implementation detail.
5. **Single source of algorithmic truth.** v6 algorithms (decay, hybrid retrieval, graph expansion, instrumentation) live in this daemon. The TS and Python SDK `RemoteAdapter`s are thin clients; they don't re-implement scoring.
6. **Operate quietly.** Default install: load model, bind socket, idle at < 200MB RAM, < 1% CPU. Sub-100ms latency for `Memory::Search` on a hot daemon under typical loads.

### Non-goals

- **Replacing the SDK.** The SDK's in-process adapters remain first-class for the embed-in-app use case. See `docs/concepts/memory-model.md` for the deployment-shape comparison.
- **Multi-user / shared-tenant operation.** This is a per-OS-user daemon. Multi-user deployment is out of scope; run separate daemons per OS user if you need it.
- **Network-exposed daemon.** The daemon binds Unix sockets only. The optional `cm-http` bridge binds `127.0.0.1` and is also not intended for the open internet.
- **Cluster / distributed memory.** One node. Memory is not synced between machines (the SDK with a remote adapter — Postgres, Convex — covers that case).
- **Plugin marketplace.** Provider extension is in-tree only at this stage. Out-of-tree provider plugins can come later if needed.

## 2. High-level shape

```
   +---------------------+   +-----------------+   +------------------+
   |  Claude Code agent  |   |  Cursor agent   |   |  Custom script   |
   +----------+----------+   +--------+--------+   +---------+--------+
              |                       |                      |
              |   Unix socket (length-delimited JSON)         |
              +-----------+-----------+----------+------------+
                          |                      |
                  +-------v-------+      +-------v-------+
                  |  TS SDK with  |      |  Python SDK   |
                  | RemoteAdapter |      | RemoteAdapter |
                  +-------+-------+      +-------+-------+
                          |                      |
                          +-----------+----------+
                                      |
        Unix socket: identity-scoped runtime dir / cm.sock (0700)
                                      |
                          +-----------v-----------+
                          |     cm-daemon         |
                          |                       |
                          |  IPC accept + dispatch|
                          |  Embedding model RAM  |
                          |  LLM extractor + cache|
                          |  Explicit lifecycle   |
                          |  maintenance          |
                          |  Hybrid retrieval     |
                          |  Graph expansion      |
                          +-----------+-----------+
                                      |
                              +-------v--------+
                              |  SQLite (WAL)  |
                              |  1 writer pool |
                              |  N reader pool |
                              +----------------+


  Browser                       cm-http (loopback only, bearer auth)
  client    --HTTP/JSON-->      |  bind 127.0.0.1                  |
                                |  validate Bearer token           |
                                |  proxy to Unix socket            |
                                +----------------------------------+
                                            |
                                            v
                                   (same daemon as above)
```

## 3. Process model

### 3.1 Two binaries, two roles

- **`cm-daemon`** — long-running service. Owns the socket, store, embedding model, LLM extractor, and lifecycle maintenance execution. Accepts client connections.
- **`cm`** — client CLI. Subcommands include `store`, `search`, `get`, `list`, `tick`, `status`, `counts`, lifecycle helpers, and token/config helpers. Auto-spawns `cm-daemon` if the socket is missing unless `--no-spawn` is set.
- **`cm-http`** — optional separate binary. HTTP loopback bridge. See §10.

The daemon and CLI are the same Rust binary in some sense (one workspace), but ship as distinct executables so that `cm` stays small and starts fast.

### 3.2 Auto-spawn

Adopted from mxr/lazydap, with one change: cognitive-memory auto-spawns by default (lazydap-style), since it's expected the user will rarely think about the daemon explicitly.

Sequence when `cm <subcommand>` is invoked:

1. Probe socket at the configured path.
2. If reachable and protocol-compatible → connect, send request, return.
3. If unreachable: inspect the PID file. Dead PID/stale socket state is cleaned up; a live PID with a broken socket is terminated before restart.
4. Spawn `cm-daemon --foreground` detached with the resolved instance, socket, DB, PID, and log paths.
5. Poll `Diagnostics::Status` for up to 5s and confirm the daemon PID matches the spawned child.
6. If startup fails, surface the daemon log path.

The daemon writes a PID file and refuses to bind over a reachable socket. A second daemon cannot delete the live socket out from under the first.

### 3.3 Shutdown

Graceful: Ctrl-C, `cm daemon stop`, or `Diagnostics::Shutdown` triggers a shutdown broadcast; the accept loop stops accepting; in-flight requests drain briefly; the socket and PID files are removed; process exits. Test harnesses use the same shutdown handle directly.

### 3.4 Crash recovery

Durable state lives in SQLite. Live session state (current connections, in-flight extraction batches, in-memory caches) is reconstructed from scratch on restart. Crash recovery is invisible to clients beyond a connection drop; clients implement reconnect with exponential backoff (10ms, 50ms, 250ms, 1s, 1s, …).

If the daemon crashed mid-write the SQLite WAL recovers on next open. Migrations are idempotent: a half-applied migration replays cleanly (see §6.4).

## 4. IPC

### 4.1 Transport

Unix domain socket, `SOCK_STREAM`. Mode 0700 on the socket and parent runtime directory. Owner-only access enforced by the OS.

Path resolution order:
1. `COGNITIVE_MEMORY_SOCKET_PATH` environment variable (if set).
2. Runtime identity from `COGNITIVE_MEMORY_INSTANCE`.
3. Release default identity `cognitive-memory`; debug default identity `cognitive-memory-dev`.
4. Platform runtime/data/config/cache/log roots from `dirs`, with explicit `COGNITIVE_MEMORY_*_DIR` overrides for tests and local harnesses.

### 4.2 Framing

4-byte big-endian length prefix, then JSON payload. Max frame: 16 MiB. This matches mxr (`tokio_util::codec::LengthDelimitedCodec`). 16 MiB is enough headroom for batched memory inserts and large LLM-extraction inputs without inviting denial-of-service.

### 4.3 Message envelope

```rust
struct IpcMessage {
    id: u64,
    payload: IpcPayload,
}

enum IpcPayload {
    Request(Request),
    Response(Response),
    Event(Event),
}
```

`id` correlates request ↔ response. Events use `id = 0` (or a daemon-owned counter — TBD in protocol spec). Clients allocate request `id`s monotonically per connection; the daemon echoes them.

### 4.4 Buckets

Following mxr/lazydap discipline, three wire buckets prevent the protocol from drifting into a kitchen sink:

| Bucket | Purpose | Examples |
| --- | --- | --- |
| `Memory` | Store, CRUD/listing, search, links, subscriptions | `Store`, `StoreBatch`, `Search`, `Get`, `List`, `Update`, `Delete`, `Link`, `VectorSearch`, `SearchLexical`, `BatchUpdate`, `Subscribe` |
| `Lifecycle` | Explicit maintenance and tier operations | `Tick`, `FindFading`, `FindStable`, `MarkSuperseded`, `MigrateToCold`, `MigrateToHot`, `ConvertToStub`, `UpdateRetention`, `Clear` |
| `Diagnostics` | Status, traces, health, shutdown, bridge tokens | `Status`, `RecentTraces`, `Doctor`, `Shutdown`, `MintBridgeToken`, `ValidateBridgeToken` |

Client-specific UI/pane state is deliberately not a wire bucket.

Adding a request type is mechanical; the `docs/developer/adding-a-request.md` recipe covers the steps. The bucket discipline is enforced by file layout (one module per bucket) and by code review.

### 4.5 Versioning

Constant `IPC_PROTOCOL_VERSION: u32 = 1`. Bumped on any breaking change to request/response/event shapes. Clients send their version in the first frame after connecting; the daemon refuses mismatched clients with a clear `Response::Error { kind: ProtocolMismatch, ... }` and closes the connection.

In practice clients ship with a known protocol version baked in. Mismatches mean upgrade either the daemon or the SDK.

### 4.6 Subscriptions

Events do not flow until a client subscribes. After connection setup, a client may send `Request::Memory(MemoryRequest::Subscribe { replay_snapshot: true })`. The daemon responds with `Subscribed` and, when requested, immediately pushes a `CurrentState` event. Later lifecycle events such as `TickCompleted` are delivered on the same connection. Consumers dispatch by `IpcPayload` variant.

## 5. Crate layout

The Rust workspace is structured to enforce architectural boundaries with Cargo's dependency graph. The discipline mirrors mxr: a crate that does not depend on `daemon` *cannot* bypass IPC. This is not a convention; it is checked at build time.

```
crates/
├── core/              types, ids, errors, traits with no I/O
├── protocol/          Request/Response/Event enums, codec, version constant
├── store/             SQLite (sqlx), schema, migrations, two-pool, repositories
├── search/            vector search + hybrid retrieval (BM25 via tantivy or FTS5)
├── embeddings/        local model (fastembed-rs), provider trait, cache
├── llm/               LLM provider trait (OpenAI, Anthropic), extractor, rate limit
├── lifecycle/         decay, consolidation, expiry, tick execution
├── graph/             association graph + n-hop expansion
├── client/            Rust client lib for tests; thin wrapper over protocol+codec
├── daemon/            binary `cm-daemon`: accept loop, dispatcher, handlers
├── cli/               binary `cm`: subcommands, auto-spawn, connection
└── http-bridge/       binary `cm-http`: HTTP → Unix socket proxy
```

Dependency rules (enforced by `cargo deny` config + CI):

- `core` depends on nothing internal.
- `protocol` depends only on `core`.
- `store`, `search`, `embeddings`, `llm`, `lifecycle`, `graph` depend on `core` and any one of each other where the layered design demands. They never depend on `daemon`, `cli`, or `http-bridge`.
- `client` depends on `core` + `protocol` only.
- `daemon` may depend on everything in `crates/` except `cli`, `http-bridge`, and `client`.
- `cli` and `http-bridge` depend on `core`, `protocol`, `client` — never on `daemon`, `store`, `search`, etc.

## 6. Storage

### 6.1 SQLite, WAL, two-pool

One database file: `~/Library/Application Support/cognitive-memory/data.db`. Opened in WAL mode with `PRAGMA foreign_keys = ON`, `PRAGMA synchronous = NORMAL`, `PRAGMA journal_mode = WAL`.

Two `sqlx::SqlitePool` instances:
- **Writer pool**: max 1 connection. All writes serialise here.
- **Reader pool**: max N connections (default 4). Read-only.

Why: WAL allows multiple readers concurrent with one writer at the SQLite level. The two-pool wrapper makes this an architectural property of the daemon — any code path that asks for a writer waits if one is busy, any code path that asks for a reader runs in parallel with up to N − 1 others. No mutex juggling in user code.

### 6.2 Schema (overview)

Authoritative schema lives in `crates/store/migrations/`. High-level tables:

| Table | Purpose |
| --- | --- |
| `memories` | id, user_id, content, category (episodic / semantic / procedural / core), memory_type (fact / preference / plan / transient_state / other), embedding (BLOB or rowid into vector index), created_at, last_accessed_at, valid_from, valid_until, ttl_seconds, retention_floor, retrieval_count, metadata (JSON: project, source_agent, tags, …) |
| `associations` | source_memory_id, target_memory_id, weight, kind (cooccurrence / inferred / explicit), updated_at |
| `events` | id, kind, payload (JSON), occurred_at — append-only event log used by lifecycle, undo, and pub/sub replay |
| `extractions` | hash of input turn, provider, model, output_memories (JSON) — extraction cache, dedupes repeat extractions |
| `embedding_cache` | provider, model, text_hash, embedding (BLOB) — shared cache across all clients |
| `kv` | namespace, key, value — small daemon-owned config (current schema version, enabled features, etc.) |

Vector storage is a dense embedding blob in `memories.embedding`, scored in Rust with cosine similarity. Hybrid retrieval adds SQLite FTS5/BM25 and fuses dense plus lexical rankings with Reciprocal Rank Fusion.

### 6.3 Multi-tenancy

`user_id` is the tenancy key. Every read and write is scoped by `user_id`. Two daemons could in principle share a SQLite file with different `user_id`s, but we recommend one daemon per OS user and one (or multiple) `user_id`s under that. Project, source-agent, tags, and other slicing dimensions live in `metadata` and are filterable but do not isolate tenancies.

### 6.4 Migrations

Custom migration engine (mxr pattern), not `sqlx::migrate!`. Each migration is idempotent: re-runs after a crash leave the schema in the same state. Migration version recorded last, after the migration body. This is the single most important invariant in the storage layer.

## 7. Embeddings

### 7.1 Default: bge-small-en-v1.5

Loaded at daemon startup via `fastembed-rs`. 384-dim, ~130MB on disk, ~150-200MB in RAM, fast on CPU on Apple Silicon. Detailed reasoning in `docs/decisions/0003-bge-small-default-embedding.md`.

The model is loaded once and shared across all client requests and across all `user_id`s. Clients pay no per-call model-load cost.

### 7.2 Provider override

Clients may attach `EmbeddingOverride { provider, model, api_key }` to specific calls (typically `Memory::Store` or `Memory::Search`). When set, the daemon routes embedding generation to the named hosted provider (OpenAI, Voyage, Cohere — adapters added per demand). The local model is not bypassed if override is absent.

### 7.3 Cache

`embedding_cache` table is keyed by `(provider, model, text_hash)`. SHA-256 of the canonicalised text is the hash. A `Memory::Store` that re-embeds the same text under the same provider is a cache hit; the embedding is loaded from SQLite, not regenerated. Across clients this means agent A and agent B paying for the same conversation only pay (or compute) once.

## 8. LLM extraction

### 8.1 Providers

Trait `LlmProvider` in `crates/llm`. Initial implementations: OpenAI, Anthropic. Each provider crate is in-tree and selected by config at startup.

### 8.2 Key precedence

1. Per-request override (`LlmOverride { provider, model, api_key }`).
2. Daemon environment at startup (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `COGNITIVE_MEMORY_LLM_PROVIDER`).
3. Daemon config file (`config.toml`).

If no key resolves, extraction calls return `Response::Error { kind: NoLlmConfigured, ... }`.

### 8.3 Extraction cache and dedup

`extractions` table is keyed by hash of the input turn (and provider+model so a model upgrade re-extracts). Two agents ingesting the same conversation hit the cache after the first extraction.

Rate limiting is a per-provider bucket inside the daemon, shared across all clients. Limits live in config.

## 9. Lifecycle

The decay/maintenance pipeline ported from the v6 SDK. The daemon owns execution of lifecycle work; clients request it explicitly in this release.

- **Decay**: `R = max(floor, exp(-Δt / (S · B · β_c)))` (or power-law variant per `decay_model` config). Computed lazily at retrieval time so storage doesn't churn; periodically materialised by `Tick` for stats.
- **Consolidation**: reversible summarisation. Originals retained; consolidated form is a new memory linked to its sources.
- **Expiry**: hard-filter expired transient memories from default retrieval; opt-in to surface them via `deep_recall=true`.
- **Promotion**: emergent core-memory promotion on cross-session repeated retrieval, lifting the retention floor.

The `Tick` request triggers a maintenance pass synchronously (for tools that want determinism) or asynchronously (default). A daemon-owned periodic scheduler is not part of the current release; it remains an additive future surface.

## 10. HTTP bridge

`cm-http` is a separate binary. Bound to `127.0.0.1:7472` (default; configurable). Not exposed beyond loopback.

- **AuthN**: per-request `Authorization: Bearer <token>`. Tokens can be minted by the daemon (`Diagnostics::MintBridgeToken`) and validated through `Diagnostics::ValidateBridgeToken`; env bootstrap remains available for tests/local scripts. The bridge's optional local token map stores salted hashes in memory.
- **AuthZ**: tokens are scoped to a `user_id` and a capability (`read`, `write`, `admin`).
- **HTTP hardening**: loopback bind only, Host-header allowlist, explicit CORS origin allowlist, and no HTTP listener in `cm-daemon`.
- **Wire format**: HTTP/JSON. The current bridge exposes `POST /memory/store` and `POST /memory/search`.
- **No event streaming over HTTP in v1.** SSE / WebSocket is a Phase 12 follow-up.

Detail in `docs/concepts/http-bridge.md`.

## 11. Concurrency

### 11.1 Request semaphores

Hot requests use `REQUEST_CONCURRENCY_LIMIT = 64`; bulk lifecycle/diagnostic/batch operations use `BULK_CONCURRENCY_LIMIT = 8`. This keeps long maintenance or batch work from starving normal store/search/get traffic.

### 11.2 Per-client connection

Each accepted connection runs in its own tokio task. Individual requests on that connection run as child tasks, so a slow bulk tick does not block a later hot status/search or subscribed events on the same socket. Responses may arrive out of order and are correlated by request `id`. Subscribed events are broadcast via `tokio::sync::broadcast`; v1 subscriptions receive the connection snapshot plus lifecycle events.

### 11.3 Background tasks

Lifecycle maintenance is explicit in the current release: callers use `Lifecycle::Tick` through `cm tick` or IPC. Tick runs through the bulk lane and logs/traces like any other request. A daemon-owned periodic scheduler is still a future additive surface.

## 12. Observability

Tracing-first, mxr pattern. `tracing` starts at daemon startup and writes to the identity-scoped daemon log. Every request is recorded in a bounded in-memory trace ring with request id, bucket, op, and elapsed time; `cm trace` exposes recent entries through `Diagnostics::RecentTraces`.

- **Logs**: identity-scoped file at the status-reported log path; daemon stdio may be null when auto-spawned.
- **Metrics**: not exposed yet.
- **Doctor**: `cm doctor` runs socket, PID, DB, log, migration, and memory-count checks.

## 13. Security

Threat model and mitigations live in `SECURITY.md`. Highlights:

- Unix socket mode 0700: only the OS user can connect.
- HTTP bridge is loopback only with bearer tokens.
- LLM keys are never logged; redacted in trace fields.
- DB file mode 0600.
- No code path uploads memory contents to any service the user has not configured.

## 14. SDK relationship

The daemon is the canonical store for the daemon-mode deployment. The TS SDK and Python SDK ship a `RemoteAdapter` that:

1. Resolves the socket path the same way the CLI does.
2. Connects to an already-running daemon.
3. Serialises every existing `Adapter` method into a `Request::Memory(...)` call, awaits the matching `Response`, returns.

The SDK's existing in-process adapters (InMemory, SQLite, Postgres, Convex) keep working unchanged; library users pick `RemoteAdapter` only when they want the daemon-mode shape.

When `RemoteAdapter` is in use, the SDK does *not* duplicate scoring, decay, hybrid retrieval, or extraction logic — those live in the daemon. The SDK becomes a typed wrapper plus connection management. A schema-mismatch between SDK protocol expectations and daemon protocol version surfaces as a typed error on startup.

## 15. What's not yet decided

- Whether SDK packages should export `RemoteAdapter` in the next published version or keep it source-only until the daemon is tagged.
- Provider plugin loading. v1 is in-tree only.
- Distribution order: crates.io, Homebrew tap, GitHub tarballs, or all three together.
- Metrics surface: socket snapshot, bridge endpoint, or both.
