# cognitive-memory-daemon

**The daemon is the memory. SDKs, CLIs, agents, and the HTTP bridge are clients.**

A long-running daemon process owns a SQLite memory store and serves multiple AI-agent clients (Claude Code, Cursor, custom scripts, the TypeScript and Python SDKs in remote mode) over a Unix domain socket. One memory, all your agents on this machine.

This is the daemon-mode counterpart to [cognitive-memory](https://github.com/planetaryescape/cognitive-memory) (source at v0.5.1; PyPI published at v0.5.1; npm still at v0.4.0 as of 2026-05-31). The SDK keeps working unchanged as an in-process library — that's a different deployment shape, not deprecated. See [`docs/concepts/memory-model.md`](./docs/concepts/memory-model.md) for when each fits.

## Status

**Implementation complete for local daemon mode (2026-06-01).** The daemon is no longer docs-only: all 12 crates exist, the CLI auto-spawns the daemon, search/store paths run through Unix-socket IPC, lifecycle/event subscriptions work, and the HTTP bridge is implemented. The current workspace version is `0.0.2`; the v0.1.0 release tag still needs a packaging/pass-through review.

Current local validation: `cargo check --workspace --all-targets` and targeted daemon/bridge/protocol/CLI tests pass after the daemon-hardening pass. Full `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` are the release gate.

What's done beyond the original 14 ROADMAP phases:

- **Local embedding model wired in by default.** Daemon binary's default Cargo features pull `local-model`; `cm-daemon` ships with `bge-small-en-v1.5` via fastembed-rs and falls back to the deterministic fake provider when built `--no-default-features` (CI fast path).
- **OpenAI + Anthropic providers** with reqwest-based real HTTP, structured JSON-output prompt, per-request key override, rate-limit + 429 → `LlmError::RateLimited` mapping. Tested against `wiremock` mocks (no live network in CI).
- **BM25 hybrid retrieval via FTS5.** Migration v2 adds the FTS5 virtual table with sync triggers; `Searcher` accepts a `hybrid: bool` and fuses dense + BM25 via Reciprocal Rank Fusion (k=60). `cm search --hybrid ...` and HTTP `POST /memory/search { "hybrid": true }` both wired.
- **`Diagnostics::MintBridgeToken` + daemon token validation.** Minted tokens are high-entropy `cmb_...` values; the daemon persists only a salted SHA-256 hash in `kv`. `cm-http` validates daemon-minted tokens per request and still supports env-var bootstrap for tests/local scripts.
- **Hardened auto-spawn from CLI.** `cm` probes the socket, recovers stale PID/socket state, forks `cm-daemon` detached, polls status with PID confirmation, then connects. `--no-spawn` opts out.
- **Runtime identity.** `COGNITIVE_MEMORY_INSTANCE` scopes socket, PID, DB, config, cache, bridge token file, and logs. Debug builds default to `cognitive-memory-dev`; release builds default to `cognitive-memory`.
- **Packaging.** `release-please-config.json` + `.release-please-manifest.json` for tag-driven releases. Homebrew formula skeleton at `packaging/homebrew/cognitive-memory.rb`. Release workflow builds macOS arm64/x86_64 + Linux x86_64 tarballs on tag push.
- **Parity benchmark.** `crates/lifecycle/benches/parity_bench.rs` runs Rust `compute_retention` against 8 hardcoded Python-derived reference values (1e-4 absolute tolerance) and reports throughput (~6 ns / call). Run with `cargo bench -p cognitive-memory-lifecycle`.

Read order for any new contributor or future session: [`README.md`](./README.md) (this file) → [`ARCHITECTURE.md`](./ARCHITECTURE.md) → [`PROTOCOL.md`](./PROTOCOL.md) → [`ROADMAP.md`](./ROADMAP.md) → [`SECURITY.md`](./SECURITY.md) → [`AGENTS.md`](./AGENTS.md) → [`docs/`](./docs/README.md). For TDD workflow specifically: [`docs/developer/test-discipline.md`](./docs/developer/test-discipline.md).

## Why a daemon

Existing cognitive-memory consumers each load the model, hold their own SQLite handle, run their own LLM extraction, and run their own maintenance pass. Two agents on the same machine working on the same conversation extract twice, embed twice, and risk write contention against the shared SQLite file.

The daemon collapses the M-clients × N-backends fan-out into one process:

- One copy of the local embedding model in RAM (~130MB, not 130MB × number of agents).
- One LLM extractor with shared rate limits and a single token budget.
- Single SQLite writer (sidesteps multi-process WAL contention entirely).
- Cross-agent visibility: facts agent A stores are immediately searchable by agent B, with optional pub/sub on memory events.
- Centralised lifecycle (decay, consolidation, expiry) on a daemon-owned schedule, not a randomly-elected client.

## Architecture in one paragraph

A Rust binary (`cm-daemon`) auto-spawns on first CLI invocation, binds an owner-only Unix socket for the active runtime identity, writes PID/status/log truth, and accepts length-delimited JSON messages in three wire buckets: `Memory`, `Lifecycle`, and `Diagnostics`. Client-specific UI state stays out of the protocol. Clients send `Request`, daemon replies with matching `Response`, and subscribed connections receive `Event` frames. SQLite (WAL mode, two-pool: 1 writer + N readers) is canonical state; embedding model and provider adapters are owned by the daemon process. A separate binary, `cm-http`, proxies loopback HTTP to the same Unix socket with Host/CORS checks and per-request bearer auth.

Full blueprint: [`ARCHITECTURE.md`](./ARCHITECTURE.md). Wire format: [`PROTOCOL.md`](./PROTOCOL.md). Phasing: [`ROADMAP.md`](./ROADMAP.md).

## Repository layout

```
cognitive-memory-daemon/
├── README.md                this file
├── ARCHITECTURE.md          full blueprint
├── PROTOCOL.md              IPC wire format and request/response/event catalogue
├── ROADMAP.md               implementation phases
├── SECURITY.md              threat model, socket perms, key handling
├── AGENTS.md                rules for AI agents working on this codebase
├── CONTRIBUTING.md          how to contribute, code style, PR flow
├── docs/
│   ├── concepts/            deeper dives on individual subsystems
│   ├── operations/          install, configure, observe, troubleshoot
│   ├── developer/           how to extend the daemon (add a request, etc.)
│   └── decisions/           architecture decision records (ADRs)
└── crates/                  (created in Phase 0) Rust workspace
```

## Quick orientation by audience

- **You want to use it from an AI agent**: read `docs/operations/configuration.md` and the relevant SDK README (TS or Python) for `RemoteAdapter` usage.
- **You want to operate it**: `docs/operations/installation.md` and `docs/operations/observability.md`.
- **You want to contribute or extend it**: `AGENTS.md`, `ARCHITECTURE.md`, `docs/developer/`.
- **You want to know why we made some specific decision**: `docs/decisions/`.

## Relationship to the rest of the cognitive-memory project

| Component | Role | Language | Status |
| --- | --- | --- | --- |
| [`cognitive-memory-sdk`](https://github.com/planetaryescape/cognitive-memory) | Library: embed memory directly into your app process | Python, TypeScript | Source v0.5.1; PyPI v0.5.1; npm v0.4.0 |
| `cognitive-memory-daemon` (this) | Service: long-running daemon owned by a single user, multi-agent | Rust | Implemented locally; v0.1.0 release tag not cut |
| [`cognitive-memory-benchmarks`](https://github.com/planetaryescape/cognitive-memory-benchmarks) | Evaluation harness — LoCoMo, LongMemEval-S, LTI-Bench | Python | Active May 2026 current-refresh artifacts |

The daemon does not replace the SDK. The SDK `RemoteAdapter` source is the daemon's first-class client — when the SDK is configured with `RemoteAdapter`, all storage, retrieval, extraction and lifecycle calls go through the daemon over the socket. In v0.5.1 the published TypeScript package does not yet export `adapters/remote`; use source builds or the daemon docs for that path. When configured with the existing in-process adapters (`InMemoryAdapter`, `JsonlFileAdapter`, `PostgresAdapter`, `ConvexAdapter`), the daemon is not involved.

## License

MIT OR Apache-2.0, matching the rest of the local daemon projects.
