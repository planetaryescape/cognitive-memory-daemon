# ADR 0001 — Rust as the daemon language

- **Status**: Accepted
- **Date**: 2026-05-07
- **Deciders**: bhekanik

## Context

`cognitive-memory-daemon` is a long-running service that holds an embedding model in RAM, accepts concurrent connections from multiple AI-agent clients, owns a SQLite store, and is expected to run for weeks at a time without restart.

The natural starting question was: should the daemon be Rust (matching `mxr` and `lazydap`, the user's existing daemon-pattern projects) or Python (matching the existing `cognitive-memory-sdk` engine that already implements the v6 algorithms)?

## Decision

The daemon is written in Rust.

## Reasoning

Three load-bearing reasons for Rust over Python in this specific shape:

1. **GIL contention defeats the architectural goal.** The daemon's main reason to exist is the shared embedding model in RAM and shared LLM extractor across many clients. In Python, any client request that triggers CPU-bound work on the local embedding model serialises *every* other client behind it (the GIL). The mitigation — process pool with one model per worker — re-fragments the cache that the daemon was built to centralise. Rust's free-threaded model has no such tradeoff.

2. **Long-uptime daemons drift in Python.** Production Python services typically need periodic restart cycles to control memory growth. The cognitive-memory daemon is intended to run for weeks. Rust's predictable allocation and absence of GC make this a non-issue.

3. **The IPC and storage shell already exists in Rust.** `mxr` provides a production-tested template for: Unix-socket accept loop, length-delimited JSON codec, two-pool SQLite, idempotent migrations, structured tracing, graceful shutdown, request semaphore. Reusing this shell means the work is "translate the v6 algorithms into Rust", not "design and write the daemon plumbing".

The counter-argument was that the v6 algorithms (decay, hybrid retrieval, graph expansion, instrumentation) already exist in Python and porting them is wasted work. We weighed this and decided:
- The algorithms are small and well-specified — algebra and graph traversal, not neural-network research.
- The Python implementation will continue to exist as the in-process SDK option; it is not deprecated.
- The local-embedding ecosystem in Rust (`fastembed-rs`, `ort`, `candle`) is mature enough to support the daemon's needs without falling back to Python interop.

## Consequences

### Positive

- Free concurrency. Many clients, one shared model, no GIL.
- Predictable memory behaviour over multi-week uptime.
- Mxr's plumbing (workspace layout, codec, store wrapper, dispatcher pattern) ports almost verbatim.
- Single binary distribution; no Python runtime dependency for users.
- Stronger compile-time guarantees (`sqlx` macros, type-checked enums for the protocol).

### Negative

- v6 algorithms must be ported from Python to Rust. Estimated several weeks of work in Phases 8 (lifecycle), 9 (graph), 10 (LLM extraction).
- The team that maintains `cognitive-memory-sdk` (Python-primary) gains a Rust component to maintain alongside it. Documentation and tests in this repo are the mitigation.
- Iteration on retrieval/scoring algorithms is slower in Rust than Python. We accept this; the algorithms are stable enough at v6 that Rust's slower iteration is the right trade.

### Neutral

- TS SDK was always going to be a remote client. The choice between Python and Rust on the daemon side does not change the TS SDK's situation.
- Python SDK gains a `RemoteAdapter` so that Python clients can use the Rust daemon transparently.

## Alternatives considered

- **Python daemon.** Rejected for the GIL and uptime reasons above.
- **Rust daemon hosting Python via PyO3 for the algorithm layer.** Rejected as the worst of both worlds: Rust complexity plus a Python interpreter in process plus marshalling overhead, with the GIL still present for the Python parts.
- **Two daemons (Python and Rust) sharing a SQLite file.** Rejected as a coordination nightmare; two writers to the same DB defeats the single-writer architecture.
- **Defer the daemon and continue with library-mode SDK + shared SQLite.** Discussed and rejected because it does not deliver the cross-agent visibility, shared cache, or central lifecycle that motivate the daemon in the first place.

## References

- `ARCHITECTURE.md` §1 (goals).
- Reference projects: `mxr` at `/Users/bhekanik/code/planetaryescape/mxr`, `lazydap` at `/Users/bhekanik/code/planetaryescape/lazydap`.
- Obsidian notes: `The Local Daemon Pattern.md`, `Headless Core + Multiple Clients.md`.
