# Test discipline

This file codifies how we write and grade tests. The testing *strategy* (what gets tested at which level — unit / integration / E2E) is in [`testing-strategy.md`](./testing-strategy.md). This file is the *discipline*: TDD methodology, the 5-question gate, the rubric, mutation testing, per-PR procedure.

It is not optional. Tests below the per-class rubric threshold are rewritten before merge. The discipline exists because shipped sycophantic tests provide false confidence — exactly the kind of bug that hides under green CI for a year.

## 1. Why TDD here

Three reasons specific to this project:

1. **The protocol is the public surface.** Tests written after implementation tend to mirror the implementation; tests written before implementation describe the contract. We need the contract description.
2. **Vendored code coexists with net-new code.** Without TDD discipline, "vendor + write tests after" becomes "vendor + skip tests because mxr already has them" — and the contracts we depend on go untested.
3. **v6 algorithms are ported from Python.** Writing the Rust port test-first against the Python output (parity tests) is the only way to catch silent numeric drift. Writing the Rust port first and then testing it tends to mirror the Rust formula instead of validating against the spec.

## 2. The TDD loop

Vertical slicing. **One behaviour → one test → minimum code → refactor → next behaviour.** Horizontal slicing (write all tests first, then all code) is banned.

For each behaviour:

1. **5-question gate** (§3). All five must pass before writing the test.
2. **RED**: write the test. Run it. Confirm it fails *for the right reason* (not for a missing import or a typo). If it fails for a wrong reason, fix the reason without weakening the test.
3. **GREEN**: write the minimum code to pass the current test. Don't anticipate the next test. Don't add fields that aren't exercised. Don't pre-emptively add error paths that no test demands. (For vendored mechanical code, GREEN can be `cp mxr/path/to/file.rs crates/<crate>/src/file.rs && rename_idents`. Vendoring is GREEN, not test-skipping.)
4. **REFACTOR**: with the test green, extract duplication, deepen modules, apply SOLID where natural. Run tests after each refactor step. Never refactor while RED.
5. **Score** the test against the rubric (§5). Below threshold → rewrite the test before continuing.
6. **Move to the next behaviour.** Don't batch test writing.

## 3. The 5-question gate

Before writing any test, answer:

1. **Mutation kill.** Would this test catch a representative mutation in the code under test? (Flip `>` to `>=`, swap `&&`/`||`, change `+` to `-`, replace return value with `Default::default()`.) If no, the assertions are too vague.
2. **Spec-not-impl values.** Are the expected values in the test taken from the **specification** (a hardcoded literal from `PROTOCOL.md`, a number from a v6 spec table, a property of the contract) or do they mirror what the implementation will compute? If the latter, the test will pass *because* the implementation has the same formula, even if both are wrong.
3. **Edge case coverage.** Does the test suite (this test + siblings) cover boundary, null/empty, and error conditions, not just the happy path?
4. **Delete test.** If you replaced the function under test with `unimplemented!()` (or its equivalent), would this test fail? If no, the test is tautological — it asserts on something the function isn't actually responsible for.
5. **Implementation swap.** Could you replace the implementation entirely (different algorithm, different data structures, different crate) and have this test still pass, assuming behaviour is preserved? If no, the test is coupled to implementation and will break under refactoring even when nothing is wrong.

If any answer is "no", redesign the test before writing it.

## 4. Three classes of code, three thresholds

| Class | Definition | Examples | Min rubric score | Min mutation kill |
| --- | --- | --- | --- | --- |
| **Net-new** | Code with no upstream precedent; we are inventing the contract. | Schema, repositories, lifecycle math, hybrid retrieval scoring, dispatch *cases* (not the dispatch shell), CLI subcommand bodies, our protocol enums. | **24/30** | **80%** |
| **Adapted** | mxr's pattern with cognitive-memory's cases. | Handler dispatcher (mxr's shape, our request kinds), CLI subcommand structure, provider trait implementations. | **21/30** | **70%** |
| **Vendored mechanical** | Copied wholesale from mxr; we depend on the contract, not the implementation. | IPC codec, two-pool wrapper, accept loop, signal-probe PID single-instance, tracing setup. | **18/30** | **60%** |

A test below its class threshold is rewritten or deleted before merge. Above-threshold tests ship.

## 5. The 10-dimension rubric (summary)

Authoritative source: the `test-quality-rubric` skill (read it once if you haven't). Brief restatement:

| # | Dimension | 0 (worst) → 3 (best) |
| --- | --- | --- |
| 1 | **Assertion specificity** | Vacuous (`is_some()`, `>=0`) → exact-value match from spec |
| 2 | **Behavioural focus** | Asserts on internal state/calls → asserts on observable contract |
| 3 | **Edge case coverage** | Happy path only → boundaries + errors + null/empty + overflow |
| 4 | **Mutation resilience** | < 40% kill rate → > 80% kill rate |
| 5 | **Mock hygiene** | Everything mocked / tautological → mocks only at system boundaries |
| 6 | **Test independence** | Order-dependent / shared state → fully isolated |
| 7 | **Readability as spec** | `test1` / `test_basic` → reads as a behaviour spec |
| 8 | **Single responsibility** | Tests multiple unrelated behaviours → one behaviour per test |
| 9 | **Redundancy (inverse)** | Multiple tests cover the same equivalence class → each test covers a distinct class |
| 10 | **Failure authenticity** | Test cannot fail (delete test passes) → test fails precisely when behaviour is violated |

Score = sum of all 10. Max = 30. Per-class thresholds in §4.

## 6. Mutation testing

Tool: `cargo-mutants`. Run scope: per crate per PR (not full-workspace; too slow).

Procedure:

```sh
cargo mutants --package cognitive-memory-protocol --no-shuffle
```

Read the report. Three outcomes per mutation:

- **Killed** (some test failed): good. The test suite catches this mutation.
- **Missed** (all tests passed): the test suite does not catch this mutation. Either the mutation is on dead/equivalent code, or the test suite has a gap. Investigate.
- **Timeout / build error**: investigate; usually means the mutation produced uncompilable code or infinite loop.

Kill-rate target per class in §4. If you miss the target, the action is to add tests, not to lower the target.

Document any *intentional* misses (genuinely equivalent mutations, dead code being removed in the same PR) in the PR description. "Could not be bothered" is not a documented reason.

## 7. Per-PR procedure

For every PR (or every phase if you don't PR per phase):

1. **Run all tests.** `cargo test --workspace` green.
2. **Run mutation tests** on changed crates. Record kill rates.
3. **Score each new test.** In the PR description, include a table:
   ```
   | Test | Class | Score | Notes |
   | --- | --- | --- | --- |
   | ipc_message_round_trips_through_json | net-new | 28/30 | edge cases handled by sibling tests |
   | ...  |
   ```
   Tests below class threshold do not merge; rewrite or delete.
4. **Delete-test spot-check.** Pick at least one test per new module. Replace the function under test with `unimplemented!()` (or comment the body and return a default). Confirm the test fails. Revert. If the test passed, the test is tautological — fix it.
5. **Implementation Swap Test spot-check.** Pick the most contract-y test in the PR. Mentally (or on paper) describe an alternate implementation that satisfies the same contract. Convince yourself the test would still pass against it. If you can't easily imagine an alternate implementation, the test is probably mirroring what you wrote.

## 8. Anti-patterns we refuse

These are the named anti-patterns from the rubric. Reviewers reject PRs that ship them:

1. **Tautological mock passthrough.** Mock returns X; function passes X through; test asserts X. Detectable: expected value matches mock return exactly.
2. **Implementation mirroring.** Expected value is computed by the same formula as production code. If the production formula is wrong, the test confirms the wrong answer. Hardcode literals from the spec instead.
3. **Happy path tunnel vision.** Only one test per function, only one input, no boundaries, no errors, no `should_panic` / `Err`-asserting tests. Add edge cases.
4. **Vacuous assertions.** `assert!(result.is_some())`, `assert!(x >= 0)`, type-checks. Assert on the value, not its existence or type.
5. **Over-mocking universe.** More `mock_*` lines than `assert_*` lines. The test is verifying a simulation, not the code. Use real instances of internal collaborators; mock only at provider boundaries.
6. **Equivalence class duplication.** Three tests for `add(1, 2)`, `add(2, 3)`, `add(4, 5)` — same code path, different inputs that all take the same branch. One test is enough.
7. **Framework testing.** `#[test] fn it_compiles() {}` — verifying that Rust works, not that your code works. Delete.
8. **Snapshot overreliance.** `assert_snapshot!(result)` everywhere encodes current behaviour (including bugs) as correct. Use specific assertions.
9. **Unfailable try-catch.** Both branches of an error-handling test pass. The test cannot fail. Restructure so success and failure paths have distinct, falsifiable assertions.

## 9. Special discipline: parity tests (Phases 8, 9, 10)

When porting a v6 algorithm from Python:

- Run the Python implementation on a fixture input. Capture its output verbatim. Save it as a JSON or CSV fixture under `crates/<crate>/tests/parity/`.
- Write the Rust test against the captured output, not against a Rust formula. The test asserts: "Rust port produces the same output as Python on input X."
- Tolerance: `1e-4` absolute on retention/score floats; exact match on category/discrete decisions. (Per the build plan at `~/.claude/plans/now-create-a-plan-validated-yao.md`.)
- If a parity test fails after Rust changes, **the Rust port is wrong**. Do not "fix" the test by widening tolerance unless you've understood the deviation, can defend it on numerical grounds (f32 vs f64, ordering of summation), and document it in an ADR.

## 10. Special discipline: vendored code

For vendored mechanical code (codec, pool, accept loop, signal-probe):

- Tests assert the **contract we depend on**, not internal state.
  - Codec: "encoded then decoded equals original" — yes. "Encodes with a 4-byte prefix" — only if prefix size is part of the contract someone else implements against (yes for clients).
  - Two-pool: "second writer waits for first" — yes. "Pool uses semaphore internally" — no, that's an implementation detail.
  - Accept loop: "concurrent connections handled up to the limit" — yes. "Spawns a tokio task per connection" — no.
- Do not write tests that mirror tests we know exist upstream. Adding `mxr_pool_writer_size_is_one` would be implementation mirroring of mxr's own test suite.
- The Implementation Swap Test is the canonical check. If we rewrote the codec from scratch tomorrow, the contract tests should still pass. They pin the behaviour we depend on; mxr's internal tests pin the implementation.

## 11. References

- The `tdd` skill in your Claude Code skills directory — the canonical TDD methodology this file follows.
- The `test-quality-rubric` skill — the canonical 10-dimension scoring rubric.
- [`testing-strategy.md`](./testing-strategy.md) — the test-level architecture (unit / integration / E2E / property) this discipline operates within.
- The build plan at `~/.claude/plans/now-create-a-plan-validated-yao.md` — phase-by-phase TDD walkthrough with worked examples.
