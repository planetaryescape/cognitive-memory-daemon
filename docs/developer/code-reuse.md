# Code reuse from mxr and lazydap

This is the concrete what-to-copy map. The strategy is **vendor and adapt**, not "extract a shared crate" — see ADR 0009 for the reasoning. Every file listed here has a known home in `mxr` (`/Users/bhekanik/code/planetaryescape/mxr`) or `lazydap` (`/Users/bhekanik/code/planetaryescape/lazydap`), both of which are the user's own dual-MIT-Apache code.

When you copy a file from upstream, the discipline is:

1. Copy it whole. Don't paraphrase or "improve" while copying — that just creates two slightly-different versions and loses the bug-fix pedigree.
2. At the top of the file, add: `// Adapted from mxr <commit-sha>:<path>` (or `lazydap`).
3. Rename `mxr_*` / `lazydap_*` identifiers to `cognitive_memory_*` mechanically. Do not rename anything else in the same commit.
4. If you fix a bug here that also exists upstream, file an issue or PR upstream. If you fix a bug upstream that's relevant here, port it.

Do not generalise across the three projects yet. Three is too few to justify a shared crate. Revisit at five.

## Phase 0 — Workspace and protocol crate

| Target | Source | Notes |
| --- | --- | --- |
| `Cargo.toml` (workspace) | mxr root `Cargo.toml` | Strip mxr-specific crates and packages; keep `[workspace.dependencies]` patterns and lints config. |
| `rust-toolchain.toml` | mxr same | Verbatim. |
| `rustfmt.toml` | mxr same | Verbatim. |
| `clippy.toml` | mxr same | Verbatim. |
| `deny.toml` | mxr same | Adjust license allow-list to match the project's choice (default: MIT OR Apache-2.0 dual). |
| `.github/workflows/ci.yml` | mxr same | Adapt job names and matrix; CI shape (fmt → clippy → test → deny) is a verbatim port. |
| `crates/protocol/src/codec.rs` | mxr `crates/protocol/src/codec.rs` | The `IpcCodec` over `tokio_util::codec::LengthDelimitedCodec`. Verbatim with rename of crate refs. |
| `crates/protocol/src/lib.rs` | mxr `crates/protocol/src/lib.rs` | `IPC_PROTOCOL_VERSION = 1` (start at 1 for cognitive-memory), `IpcMessage` envelope shape. The `IpcPayload` enum here adopts the three-kinds form — `Request`, `Response`, `Event` — so do not copy mxr's two-kind variant; replace it with the three-kind variant from `PROTOCOL.md` §3. |
| `crates/protocol/tests/round_trip.rs` | mxr equivalent if present, else write fresh from the fixture loader pattern | Loads every `tests/fixtures/*.json` and asserts round-trip. |

What you're writing fresh in Phase 0:
- The actual `Request` / `Response` / `Event` enum bodies — these are cognitive-memory-specific and live in the protocol crate's `memory.rs`, `lifecycle.rs`, `diagnostics.rs`, `event.rs`. The shape comes from `PROTOCOL.md`, not from mxr.
- Golden fixtures under `crates/protocol/tests/fixtures/` — one per request kind from `PROTOCOL.md` §5.

## Phase 1 — Storage

| Target | Source | Notes |
| --- | --- | --- |
| `crates/store/src/pool.rs` | `mxr/crates/store/src/pool.rs` | Two-pool wrapper (`Store` struct: 1-writer + N-reader pools), per-connection `PRAGMA` setup (WAL, foreign_keys), **and** the inline migration engine (`run_migrations`, `apply_migration`, `is_migration_applied`, `validate_schema` at lines 60–180; `Migration` struct + `const MIGRATIONS: &[Migration]` at line 326). Verbatim with crate-name rename. **Correction:** mxr does NOT have a `crates/store/src/migrations/` directory — migrations are inline `const` arrays in `pool.rs`. We follow the same pattern. Revisit moving to `migrations/*.sql` files only if/when we exceed ~20 migrations. |
| `crates/store/src/error.rs` | `mxr/crates/store/src/error.rs` | `StoreError` enum + `thiserror` setup. Adapt variants to cognitive-memory's actual error cases. |

What you're writing fresh in Phase 1:
- All migration entries in the `MIGRATIONS` array — cognitive-memory's schema, not mxr's email schema. Migrations are inline Rust `Migration` values referencing inline SQL strings; that's the mxr pattern, vendored.
- Repositories: `MemoryRepo`, `AssociationRepo`, `EventLogRepo`, `ExtractionCacheRepo`, `EmbeddingCacheRepo`, `KvRepo`. Patterns to follow are mxr's repo style (one struct per table, methods take `&self` + pool refs, errors typed).

## Phase 2 — Embeddings

No precedent in mxr or lazydap. Write fresh.

References:
- `fastembed-rs` docs and examples: https://github.com/Anush008/fastembed-rs.
- The cache key shape is described in `docs/concepts/embedding-strategy.md` §3.

## Phase 3 — Search

Mostly fresh. The hybrid pattern (BM25 fused with dense) has a parallel in mxr's hybrid mail search (`crates/search` uses Tantivy for BM25 + dense embeddings with RRF) — the *pattern* is reusable, the *code* mostly isn't because the data model differs. Read mxr's `crates/search/src/` for the fusion approach.

| Target | Source | Notes |
| --- | --- | --- |
| Score-fusion module | mxr `crates/search/src/hybrid.rs` (or equivalent) | Read for the RRF pattern, adapt to cognitive-memory's score formula `sim * R^alpha`. |

## Phase 4 — Daemon binary

This is the **largest** copy from mxr. Most of `crates/daemon/` plumbing is reusable.

| Target | Source | Notes |
| --- | --- | --- |
| `crates/daemon/src/main.rs` | mxr `crates/daemon/src/main.rs` (or wherever the binary entrypoint is) | Tracing init, config load, store init, model load (cognitive-memory-specific), socket bind, accept loop spawn, signal handling. Adapt initialisation order. |
| `crates/daemon/src/server.rs` | mxr `crates/daemon/src/server.rs` (lines 33-100 are the core) | Socket inspection (`inspect_socket_state`), stale-socket cleanup, `UnixListener::bind`, accept loop, `REQUEST_CONCURRENCY_LIMIT` semaphore (default 64), graceful shutdown drain (default 5s). Verbatim with renames. |
| `crates/daemon/src/handler/mod.rs` | mxr `crates/daemon/src/handler/mod.rs:71-100` (dispatch pattern) | The `dispatch()` function pattern matches `Request` and routes to handlers. The pattern is verbatim; the cases are cognitive-memory's. Per-request span creation (lines 53-59 in mxr) is verbatim. |
| `crates/daemon/src/ipc_client.rs` | mxr `crates/daemon/src/ipc_client.rs:14-67` | Client connection wrapper used by tests. Verbatim with renames. |
| `crates/daemon/src/shutdown.rs` | mxr equivalent | Broadcast-channel shutdown signal. Background tasks subscribe; accept loop publishes on SIGTERM/SIGINT. |
| `crates/daemon/src/concurrency.rs` (if separate) | mxr equivalent | Semaphore wrapper + per-request guard. |
| Socket-path resolution | mxr `crates/config/src/resolve.rs:72-89` | Adapt the path defaults to cognitive-memory's (`~/Library/Application Support/cognitive-memory/cm.sock`); the resolution logic (env override → platform default) is the same shape. |
| PID file + signal-probe single-instance | `mxr/crates/daemon/src/server.rs` lines 463–492 (`daemon_pid_file_path`, `write_daemon_pid_file`, `read_daemon_pid_file`, `clear_daemon_pid_file`, plus `nix::sys::signal::kill` with `SIGZERO`) | Verbatim with renames. Mxr does not use `flock`; it uses signal-probe on the recorded PID. |
| Tracing setup | mxr `crates/daemon/src/tracing.rs` (or wherever `init_tracing` lives) | Foreground vs detached split, JSON to file in detached, `tracing-appender` for rotation. Verbatim. |
| Auto-spawn (re-exec daemon) | **Write fresh** from lazydap design docs and obsidian `How Daemons Work.md` | **Correction:** lazydap's daemon binary at `lazydap/crates/daemon/src/main.rs` is a 24-line placeholder (M5 not landed). Auto-spawn, broadcast events, and `--wait` are *documented* in lazydap's `ARCHITECTURE.md` and `docs/blueprint/` but not yet implemented as code. Write fresh per the documented design: probe socket → if absent, double-fork + setsid + redirect stdio + write PID file (signal-probe single-instance, ARCHITECTURE.md §3.2) → poll for socket up to 2s → connect. Mxr is manual-start so cannot be vendored here. |

Adapt (same shape, cognitive-memory cases):
- Handler implementations themselves (`handler/memory.rs`, `handler/lifecycle.rs`, `handler/diagnostics.rs`).

## Phase 5 — CLI binary

| Target | Source | Notes |
| --- | --- | --- |
| `crates/cli/src/main.rs` | mxr CLI entrypoint | Clap subcommand setup, `--json` output flag, exit-code conventions. Adapt subcommands to cognitive-memory's (`store`, `search`, `get`, `list`, `tick`, `status`, `daemon`). |
| Subcommand pattern | mxr `crates/daemon/src/commands/*.rs` | Each subcommand is one file, async function, takes parsed args, returns `Result<()>`. Pattern is verbatim. |
| Auto-spawn integration | Write fresh — lazydap's CLI is placeholder; mxr does not auto-spawn. | Same as Phase 4 row above: probe socket, fork+exec daemon, poll up to 2s. |

## Phase 6 / 7 — SDK RemoteAdapter

No mxr/lazydap precedent (those are CLI-only; no SDK clients). Write fresh per `docs/developer/adding-a-request.md` step 9.

## Phase 8 — Lifecycle

Port from `cognitive-memory-sdk/sdks/python/src/cognitive_memory/lifecycle/` to `crates/lifecycle/`. No mxr/lazydap precedent.

The Python implementation is the algorithmic source of truth. The Rust port must produce numerically equivalent results within tolerance — the Phase 8 "done" criterion calls for a parity benchmark.

## Phase 9 — Graph

Port from `cognitive-memory-sdk/sdks/python/src/cognitive_memory/graph/` (or wherever the v6 graph code lives) to `crates/graph/`. No mxr/lazydap precedent.

## Phase 10 — LLM extraction

Port from `cognitive-memory-sdk/sdks/python/src/cognitive_memory/extraction/` (or equivalent) to `crates/llm/`. The provider trait pattern in mxr (`provider-gmail`, `provider-imap`, `provider-smtp`, `provider-fake`) is a useful shape reference for organising multiple LLM providers behind one trait, with a `FakeLlmProvider` for tests.

| Target | Source | Notes |
| --- | --- | --- |
| Provider trait shape | mxr `crates/core/src/provider.rs` (or wherever `MailProvider` is defined) | Pattern: trait + per-provider crate. Adapt to `LlmProvider`. |
| Fake provider for tests | mxr `crates/provider-fake` | Pattern: in-process fake that implements the same trait, returns scripted responses. Adapt to `FakeLlmProvider`. |

## Phase 11 — Instrumentation, doctor, metrics

| Target | Source | Notes |
| --- | --- | --- |
| `cm doctor` battery | mxr's `mxr doctor` if implemented | Pattern: structured check list, each check returns `ok`/`warn`/`error` + message. Exit code from worst result. |
| Per-query trace ring buffer | Write fresh, but use mxr's `tracing` patterns | The trace shape is in `PROTOCOL.md` §`Diagnostics::Trace`. |

## Phase 12 — HTTP bridge

No mxr precedent (mxr is socket-only). Write fresh on `axum`.

## Discovered additional vendor candidates

These mxr crates were missed in the initial map. Each has a phase where it earns its place; revisit when that phase opens.

| Mxr crate | Path | When relevant | What we'd reuse |
| --- | --- | --- | --- |
| `test-support` | `mxr/crates/test-support` | **Phase 0 onward** — useful immediately | Test fixtures, common harness utilities (temp dirs, sample data, in-process daemon spawning). Vendor what's small and applicable; resist generalising. |
| `keychain` | `mxr/crates/keychain` | **Phase 13** (keychain-stored API keys per ADR 0007) | OS keychain wrapper (macOS Keychain, etc.). Adapt key namespacing to `cognitive-memory.<provider>`. |
| `search` | `mxr/crates/search` (Tantivy wrapper) | **Phase 3** (hybrid retrieval) | Tantivy schema setup, BM25 query construction, RRF score fusion shape. The mail-domain types stripped; the IR plumbing kept. |
| `sync` | `mxr/crates/sync` (provider orchestration) | **Phase 10** (LLM extractor) | Orchestration shape for "fan out across providers, collect results, dedup, persist". Adapt to LLM extraction over multiple providers; the email-sync semantics stripped. |

Default posture: read these crates when entering the relevant phase, vendor selectively, do not vendor whole crates wholesale.

## Phase 13 — Polish, packaging

| Target | Source | Notes |
| --- | --- | --- |
| Homebrew tap formula | mxr's tap if it exists | Pattern. Otherwise standard Homebrew formula. |
| `release-please` config | mxr `release-please-config.json` | Pattern. |
| Crate manifests for publishing | mxr `Cargo.toml` patterns | Each publishable crate gets `description`, `license = "MIT OR Apache-2.0"`, `repository`, `keywords`, `categories`. |

## What this means for effort

The phases that ride on mxr (0, 1, 4, 5, 11, 13) get a substantial discount. The phases that don't (2, 3, 6–10, 12) are net-new work, with Python SDK as the algorithmic reference for 8–10.

Rough effort estimate, post-doc-landing:

| Phase | New code? | Notes |
| --- | --- | --- |
| 0 | ~30% new (enums, fixtures); rest copied | Days |
| 1 | ~50% new (schema, repos); rest copied | Days |
| 2 | ~95% new | Days |
| 3 | ~80% new; ~20% pattern-borrowed | Days–week |
| 4 | ~30% new (handlers, dispatch cases); rest copied | Week |
| 5 | ~40% new (subcommands); rest copied | Days |
| 6 | ~100% new | Week |
| 7 | ~100% new | Week |
| 8 | ~100% new (port from Python) | Week–weeks |
| 9 | ~100% new (port from Python) | Week |
| 10 | ~70% new (provider implementations); ~30% pattern-borrowed | Week |
| 11 | ~60% new; ~40% pattern-borrowed | Week |
| 12 | ~100% new | Week |
| 13 | ~50% new; ~50% pattern-borrowed | Days–week |

These are deliberately rough. Mxr's IPC + store + accept-loop code is genuinely battle-tested — the discount on phases 0/1/4 is real.
