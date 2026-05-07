# Roadmap

Implementation phases for `cognitive-memory-daemon`. Each phase has a clear "done" criterion and produces something runnable. Phases are ordered to keep the daemon end-to-end functional as early as possible — by Phase 4 the daemon accepts requests and stores memories; everything after is depth.

The numbering matches the milestone numbers used in commits and issue tracking. Items inside a phase are unordered unless flagged.

Most phases vendor substantial code from `mxr` (and some patterns from `lazydap`) — the file-by-file map is in [`docs/developer/code-reuse.md`](./docs/developer/code-reuse.md). Phases below cite which sections of that map apply. The strategy (vendor + adapt, not shared crate) is locked in [ADR 0009](./docs/decisions/0009-vendor-mxr-lazydap-not-shared-crate.md).

## Phase 0 — Workspace and protocol crate

**Goal**: a Rust workspace exists and `crates/protocol` compiles with the full v1 enum surface, codec, version constant, and golden test fixtures.

- Initialise Cargo workspace with `Cargo.toml`, `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `deny.toml`.
- Crate scaffolds for everything in `ARCHITECTURE.md` §5 — most are empty `lib.rs` with a single doc comment.
- `crates/core`: `MemoryId`, `UserId`, `Category`, `MemoryType`, `RetentionFloor`, error types.
- `crates/protocol`: `IpcMessage`, `IpcPayload`, `Request`, `Response`, `Event` enums covering every v1 entry from `PROTOCOL.md`. `IPC_PROTOCOL_VERSION = 1`. Length-delimited JSON codec built on `tokio_util::codec`.
- Golden fixtures under `crates/protocol/tests/fixtures/` — at least one example per request kind. Round-trip test asserts `decode(encode(msg)) == msg` for every fixture.
- CI: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo deny check`.

**Done when**: `cargo test -p cognitive-memory-protocol` is green and CI is wired up.

**Reuse**: `docs/developer/code-reuse.md` Phase 0 section. Workspace meta + codec + envelope are vendored from mxr; enums and fixtures are written fresh against `PROTOCOL.md`.

## Phase 1 — Storage

**Goal**: `crates/store` can open the database, run idempotent migrations, and CRUD memories with two-pool concurrency.

- Schema migrations for `memories`, `associations`, `events`, `extractions`, `embedding_cache`, `kv` (mxr-style migration engine: idempotent, version recorded last).
- Two-pool wrapper around `sqlx::SqlitePool` (1 writer, N readers). `PRAGMA` set on every connection in pool init.
- Repository structs: `MemoryRepo`, `AssociationRepo`, `EventLogRepo`, `ExtractionCacheRepo`, `EmbeddingCacheRepo`. All take pool refs; no global state.
- Decision call: vector storage primitive — `sqlite-vec` extension vs flat blob + Rust cosine. Default this phase to flat blob; revisit in Phase 3 when search lands.
- Unit tests against an in-process SQLite DB. Property tests on migration idempotence (apply twice, schema unchanged).

**Done when**: `cargo test -p cognitive-memory-store` covers CRUD + concurrent reader-writer scenarios.

**Reuse**: Two-pool wrapper and migration engine framework vendor from mxr. Schema, migration `.sql` files, and repositories are written fresh.

## Phase 2 — Embeddings

**Goal**: load `bge-small-en-v1.5` once, embed a string, cache the result.

- `crates/embeddings`: `EmbeddingProvider` trait. `LocalProvider` impl using `fastembed-rs` with `bge-small-en-v1.5`. `OpenAiProvider` skeleton (call site stubbed; full impl in Phase 10 alongside LLM).
- Embedding cache layer over `EmbeddingCacheRepo`. SHA-256 of canonical text as cache key.
- Lazy model load: first embed call triggers download + load; subsequent are in-memory.
- Override resolution: per-request override > daemon config > default local.

**Done when**: a unit test embeds the same string twice, second call hits cache, dimensionality is 384, cosine of identical strings ≈ 1.0.

## Phase 3 — Search

**Goal**: vector similarity search and hybrid retrieval over the store.

- `crates/search`: `Searcher` over `MemoryRepo` + `EmbeddingCacheRepo`. Default scoring: `sim * R^alpha` per v6 spec.
- Hybrid: BM25 via FTS5 (or tantivy if FTS5 proves limiting). Score fusion configurable.
- Validity filtering: hard-filter expired transients unless `deep_recall = true`.
- Per-query trace struct populated alongside results.

**Done when**: integration test stores 100 memories, searches for one, gets it back ranked first; benchmark fixture runs against a slice of LoCoMo to confirm parity with Python SDK on at least one query.

## Phase 4 — Daemon binary, accept loop, dispatcher

**Goal**: `cm-daemon` runs, binds the socket, accepts connections, dispatches `Memory::Store` and `Memory::Search` end to end.

- `crates/daemon`: `main.rs` — config load, model load, store init, socket bind, accept loop.
- Connection state: per-connection task, hello/welcome handshake, request decoder.
- Dispatcher: `Memory::Store`, `Memory::Search`, `Memory::Get`, `Diagnostics::Status`, `Diagnostics::Version` — minimum viable surface. Other ops return `NotImplemented` for now.
- Request semaphore (default 64).
- Graceful shutdown on SIGTERM/SIGINT.
- Tracing wired from line one of `main` (mxr pattern).

**Done when**: a manual test using `nc -U` (or a tiny Rust client) can `Hello`, `Store`, `Search`, `Status`, and disconnect.

**Reuse**: This phase has the largest mxr discount — accept loop, request semaphore, socket inspection, dispatch pattern, shutdown plumbing, tracing setup all vendor verbatim. See `docs/developer/code-reuse.md` Phase 4 section. Auto-spawn pattern comes from lazydap, not mxr.

## Phase 5 — CLI binary

**Goal**: `cm` exists, talks to the daemon, auto-spawns it if missing.

- `crates/cli`: `main.rs` with subcommands: `store`, `search`, `get`, `list`, `tick`, `status`, `daemon` (with sub-subcommands: `start`, `stop`, `status`, `foreground`).
- Auto-spawn: probe socket, fork daemon if needed (double-fork, `setsid`, redirect stdio, write PID file with signal-probe single-instance — see ARCHITECTURE.md §3.2), poll for socket up to 2s.
- Exit codes consistent with Unix conventions.
- `--json` flag on every read subcommand for machine output.

**Done when**: a fresh shell with no daemon running can `cm store "..."` and `cm search "..."`; the daemon comes up invisibly.

**Reuse**: Subcommand pattern and `--json` output convention vendor from mxr. Auto-spawn integration follows the lazydap pattern.

## Phase 6 — TS SDK RemoteAdapter

**Goal**: the TypeScript SDK can be configured with a `RemoteAdapter` that uses the daemon end-to-end.

- New module under `cognitive-memory-sdk/sdks/typescript/src/adapters/remote/`.
- Unix-socket client (`net.createConnection`).
- Mirror of protocol enums in TS types. Hand-written for now; consider codegen later.
- Implements existing `Adapter` interface; routes every method to a `Request::Memory` call.
- Reconnect with exponential backoff on connection drop.
- TS test suite exercises the same multi-tenancy isolation tests the existing adapters pass.

**Done when**: existing TS SDK tests pass against `RemoteAdapter` running against a live `cm-daemon`.

## Phase 7 — Python SDK RemoteAdapter

**Goal**: same as Phase 6, for Python.

- New module under `cognitive-memory-sdk/sdks/python/src/cognitive_memory/adapters/remote/`.
- `asyncio` Unix-socket client.
- Same protocol mirror, same `Adapter` interface.
- Multi-tenancy tests mirror Phase 6.

**Done when**: existing Python SDK tests pass against `RemoteAdapter` running against `cm-daemon`.

## Phase 8 — Lifecycle

**Goal**: decay, consolidation, expiry, promotion, scheduler.

- Port v6 algorithms from `cognitive-memory-sdk/sdks/python/src/cognitive_memory/lifecycle/`.
- `crates/lifecycle`: `decay`, `consolidate`, `expire`, `promote` modules.
- Tick scheduler as a background task in the daemon, configurable cadence.
- `Lifecycle::*` request handlers wired into the dispatcher.

**Done when**: a memory's retention decays per the v6 formula; a manual `cm tick` materialises stats; a benchmark against the Python SDK on the same fixture produces identical decay scores within tolerance.

## Phase 9 — Graph and association expansion

**Goal**: associations between memories with weighted edges, n-hop expansion at query time.

- `crates/graph`: `AssociationRepo` extension, n-hop traversal with weight thresholds.
- Bridge discovery (v6 spec §134-156).
- `Memory::Search { graph_expansion: { enabled: true, hops: 1 } }` returns expanded results with provenance.

**Done when**: storing two memories with an explicit link, searching for one, returning the other as an expansion result.

## Phase 10 — LLM extraction

**Goal**: `Memory::ExtractAndStore` takes a transcript and stores extracted memories.

- `crates/llm`: `LlmProvider` trait. `OpenAiProvider`, `AnthropicProvider`. Per-request key override path implemented.
- Extraction cache via `ExtractionCacheRepo`. Same input + same provider + same model = cache hit.
- Per-provider rate limiter (token bucket) shared across all clients.
- `Memory::Ingest` builds on `ExtractAndStore` plus dedup against existing memories.

**Done when**: ingesting the same conversation twice triggers one LLM call; per-request override forces a different provider for one call without changing daemon defaults.

**Reuse**: Provider-trait pattern (one trait, per-provider crate, fake-for-tests) is borrowed from mxr's `provider-gmail` / `provider-imap` / `provider-fake` shape. Algorithm code is ported from the Python SDK.

## Phase 11 — Instrumentation and per-query traces

**Goal**: `Diagnostics::Trace` returns the per-query trace from v6 spec.

- Span hierarchy with stable names; per-stage timings captured.
- Trace storage: in-memory ring buffer of recent traces, fetched by `trace_id`.
- `Diagnostics::Logs` tail.
- `cm doctor` battery (socket, DB, model, providers, time skew, disk).
- Optional Prometheus metrics endpoint behind a feature flag.

**Done when**: `cm search "..." --trace`-equivalent path returns a structured trace matching v6 spec fields.

## Phase 12 — HTTP bridge

**Goal**: `cm-http` binary serves loopback HTTP that proxies to the Unix socket with bearer auth.

- `crates/http-bridge`: axum server bound to `127.0.0.1:7472` (configurable).
- Per-request `Authorization: Bearer` validation against tokens minted by `Diagnostics::MintBridgeToken`.
- URL paths mirror request enum; bodies are payloads.
- No event streaming this phase; SSE and WebSocket are post-v1.

**Done when**: `curl -H "Authorization: Bearer <token>" -X POST http://127.0.0.1:7472/memory/search -d '...'` returns the same payload as the equivalent Unix-socket call.

## Phase 13 — Polish, packaging, parity benchmarks

**Goal**: release v0.1.0 of the daemon and remote SDKs.

- Distribution: `cargo install`-able from crates.io OR a Homebrew tap. Prefer both.
- `cm doctor` exits 0 only when the install is healthy.
- Benchmarks vs in-process SDK on LoCoMo / LongMemEval-S to demonstrate parity (or quantified deviation, with explanation).
- Final license decision (default plan: MIT OR Apache-2.0 dual licence, mxr-aligned).
- v0.1.0 tagged.

**Done when**: a fresh Mac with `brew install cognitive-memory` (or `cargo install cm-daemon`) can `cm store ...` and `cm search ...` within 60 seconds, including model download.

## Post-v1 backlog (uncommitted)

- Event streaming over HTTP (SSE / WebSocket).
- Codegen of TS/Python protocol types from a JSON Schema.
- Out-of-tree provider plugins.
- Per-memory ACLs beyond `user_id`.
- macOS launchd / Linux systemd units for boot-time spawn.
- Observability export to OTLP.
- Anthropic provider for embeddings if/when offered.
- Local LLM extractor option (Ollama, mlx-llm bindings).
