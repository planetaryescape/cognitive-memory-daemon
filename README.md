# cognitive-memory-daemon

**The daemon is the memory. SDKs, CLIs, agents, and the HTTP bridge are clients.**

A long-running daemon process owns a SQLite memory store and serves multiple AI-agent clients (Claude Code, Cursor, custom scripts, the TypeScript and Python SDKs in remote mode) over a Unix domain socket. One memory, all your agents on this machine.

This is the daemon-mode counterpart to [cognitive-memory](https://github.com/planetaryescape/cognitive-memory) (currently at v0.4.0). The SDK keeps working unchanged as an in-process library — that's a different deployment shape, not deprecated. See [`docs/concepts/memory-model.md`](./docs/concepts/memory-model.md) for when each fits.

## Status

**v0.1.0 ready (2026-05-07): all phases + previously-deferred items shipped.** 96 passing tests across 12 crates; full CI chain green (`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `cargo deny check`). What's done beyond the original 14 ROADMAP phases:

- **Local embedding model wired in by default.** Daemon binary's default Cargo features pull `local-model`; `cm-daemon` ships with `bge-small-en-v1.5` via fastembed-rs and falls back to the deterministic fake provider when built `--no-default-features` (CI fast path).
- **OpenAI + Anthropic providers** with reqwest-based real HTTP, structured JSON-output prompt, per-request key override, rate-limit + 429 → `LlmError::RateLimited` mapping. Tested against `wiremock` mocks (no live network in CI).
- **BM25 hybrid retrieval via FTS5.** Migration v2 adds the FTS5 virtual table with sync triggers; `Searcher` accepts a `hybrid: bool` and fuses dense + BM25 via Reciprocal Rank Fusion (k=60). `cm search --hybrid ...` and HTTP `POST /memory/search { "hybrid": true }` both wired.
- **`Diagnostics::MintBridgeToken`** protocol kind + daemon handler. Token raw shown once, salted SHA-256 hash persisted in `kv`. `cm-http` boots via `COGNITIVE_MEMORY_HTTP_MINT_USER` to mint from the daemon at startup; env-var bootstrap remains the test/CI fallback.
- **Auto-spawn from CLI.** `cm` probes the socket, forks `cm-daemon` detached, polls up to 2s, then connects. `--no-spawn` flag opts out (the path tests rely on).
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

A Rust binary (`cm-daemon`) auto-spawns on first CLI invocation, binds a Unix socket at `~/Library/Application Support/cognitive-memory/cm.sock` (mode 0700, macOS-first), and accepts length-delimited JSON messages categorised into four buckets — `Memory`, `Lifecycle`, `Diagnostics`, `ClientSpecific`. Clients send `Request`, daemon replies with matching `Response` and may push `Event`s on connections that have subscribed. SQLite (WAL mode, two-pool: 1 writer + N readers) is canonical state; embedding model and provider adapters are owned by the daemon process. A separate binary, `cm-http`, proxies localhost HTTP to the same Unix socket for browser-based or non-Unix clients, with per-request bearer auth.

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
├── CLAUDE.md                Claude-specific entry; defers to AGENTS.md
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
| [`cognitive-memory-sdk`](https://github.com/planetaryescape/cognitive-memory) | Library: embed memory directly into your app process | Python, TypeScript | Shipped — v0.4.0 on PyPI / npm |
| `cognitive-memory-daemon` (this) | Service: long-running daemon owned by a single user, multi-agent | Rust | Pre-implementation — docs only |
| [`cognitive-memory-benchmarks`](https://github.com/planetaryescape/cognitive-memory-benchmarks) | Evaluation harness — LoCoMo, LongMemEval-S, LTI-Bench | Python | Active runs against v0.4.0 |

The daemon does not replace the SDK. The SDK's `RemoteAdapter` (planned, lands alongside the daemon) is the daemon's first-class client — when the SDK is configured with `RemoteAdapter`, all storage, retrieval, extraction and lifecycle calls go through the daemon over the socket. When configured with the existing in-process adapters (`InMemoryAdapter`, `JsonlFileAdapter`, `PostgresAdapter`, `ConvexAdapter`), the daemon is not involved.

## License

Pending — defaulting to MIT OR Apache-2.0 dual to match `mxr`. Decision tracked in [`ROADMAP.md`](./ROADMAP.md) Phase 13.
