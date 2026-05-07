# Testing strategy

The shape of the test suite, what mocks are allowed, and what they're not.

## 1. Levels

Four levels, ordered cheapest to most expensive:

1. **Unit tests** alongside the code (`#[cfg(test)] mod tests`). Pure-function logic, decay math, scoring formulas, codec round-trips. Fast (< 1 ms per test). No I/O.
2. **Integration tests** in each crate's `tests/` directory. Cross-module logic with real dependencies inside the crate boundary — e.g., `crates/store/tests/migrations_idempotent.rs` opens a real temp SQLite and runs migrations twice.
3. **Protocol conformance tests** under `crates/protocol/tests/fixtures/`. Golden JSON files describe `Request` / `Response` / `Event` shapes; the test harness round-trips every fixture and asserts equality. Adding a new request kind in code requires adding a fixture in the same PR.
4. **End-to-end tests** in `crates/daemon/tests/`. A test spins up a real `cm-daemon` (in-process, on a temp socket, against a temp SQLite) and drives it with a real client. No mocks for the store. Mocks allowed only for outbound provider calls (LLM, hosted embeddings).

## 2. Mock policy

The mxr lesson, codified:

| Subsystem | Mock allowed | Why |
| --- | --- | --- |
| SQLite store | **No** | Real SQLite has WAL behaviour, locking semantics, and migration recovery that mocks hide. Tests use a temp DB. |
| Local embedding model | **Yes**, in unit tests | Loading bge-small-en-v1.5 takes seconds; per-test cost is unacceptable. Integration tests share a process-wide model loaded once. |
| Hosted embedding provider | **Yes** | Network calls in tests are slow and flaky. Use a `FakeEmbeddingProvider` that returns deterministic vectors. |
| LLM provider | **Yes** | Same. `FakeLlmProvider` returns scripted responses. |
| Filesystem (config files, log files) | **No** | Use `tempfile::tempdir`. |
| Time | **Yes**, via a `Clock` trait | Decay tests need to advance time deterministically. |

The fakes live in `crates/llm/src/fakes.rs`, `crates/embeddings/src/fakes.rs`. They are first-class types tested in unit tests of their own.

## 3. Test naming

`<situation>_<expected_outcome>`:

- `decay_with_zero_floor_approaches_zero`
- `migrations_idempotent_on_replay`
- `protocol_search_request_round_trips`
- `dispatch_unknown_request_returns_invalid_payload`

No `test_` prefix (Rust doesn't need it). No vague names (`it_works`, `basic`, `smoke`).

## 4. Property tests

`proptest` for properties that should hold over inputs:

- Codec round-trip: `decode(encode(msg)) == msg` for arbitrary `IpcMessage`.
- Migration idempotence: applying any subset of migrations followed by all of them is equivalent to applying all of them.
- Decay monotonicity: `R(t1) >= R(t2)` if `t1 <= t2` and no reinforcement events between.
- Embedding cache key stability: canonicalisation is the identity on canonical inputs.

## 5. End-to-end test pattern

Each E2E test follows the same shape:

```rust
#[tokio::test]
async fn e2e_store_then_search_returns_inserted_memory() {
    let env = TestEnv::start().await;          // temp dir, temp socket, daemon spawned
    let client = env.client().await;           // performs Hello/Welcome handshake

    let stored = client.memory_store(...).await.unwrap();
    let results = client.memory_search(...).await.unwrap();

    assert_eq!(results[0].memory.id, stored[0].id);
}
```

`TestEnv` owns the temp dir and shuts the daemon down on drop. Tests can run in parallel because each gets its own socket.

The daemon binary used in E2E tests is the same binary built by `cargo build`. Tests do not stub-out the daemon — they exercise the whole stack.

## 6. Coverage expectations

- `crates/protocol`: every variant of every enum has at least one fixture.
- `crates/store`: every public method of every repository has at least one integration test against a real SQLite.
- `crates/daemon`: every request handler has at least one E2E test covering the happy path and at least one error path.
- `crates/lifecycle`, `crates/search`, `crates/embeddings`, `crates/llm`: pure-function logic has unit tests; provider trait impls have integration tests with fakes.

Coverage isn't measured as a number. PRs that add code without tests get bounced; the rule is enforced by review, not by a percentage gate.

## 7. Bench

`benches/` per crate, `criterion` for microbenchmarks. Phase 13 stands up a parity benchmark against the Python SDK on LoCoMo so we can quantify any deviation.

## 8. CI

Required on every PR:

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --workspace`
- `cargo deny check`
- Protocol fixtures present for any new request kinds (CI script greps for `kind: "Request"` in fixtures).
- `cargo doc --no-deps` succeeds (no broken intra-doc links).

E2E tests are part of `cargo test`. They are not behind a feature flag.

## 9. What we do not test for

- Performance regression in CI. Phase 13 may add a budget; until then, perf is asserted by manual `cargo bench` runs against the parity benchmark.
- Cross-platform Linux-specific behaviour. macOS-first; Linux CI matrix arrives when we add Linux paths to the codebase.
- Long-running soak tests. The daemon is intended to run for weeks; we do not currently have a test that runs for weeks. If a memory leak is suspected, run a 24-hour soak manually with `tracing` heap profiling.
