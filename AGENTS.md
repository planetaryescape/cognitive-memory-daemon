# AGENTS.md

Instructions for AI agents (Claude Code, Cursor, Codex, etc.) working in this repository. Humans and agents read the same file. If you're a human contributor, this is also the operating manual.

`CLAUDE.md` defers here. There is no separate Claude-specific guidance.

## 1. Read first

Before changing anything beyond a typo:

1. `README.md` — what this repo is.
2. `ARCHITECTURE.md` — the load-bearing blueprint.
3. `PROTOCOL.md` — the IPC contract.
4. `ROADMAP.md` — what phase we're in. Don't skip phases. The phasing exists because some surfaces are ill-defined until earlier ones land.
5. The `docs/decisions/` ADRs — read the titles; read the body of any ADR whose subject overlaps your task.

If a doc and the code disagree, fix one of them in the same commit. Do not leave the disagreement.

## 2. Core principles (NON-NEGOTIABLE)

The 12 claims that anchor every implementation decision in this repo. Any change must respect them; cite them in code review when one is at stake. The architectural rules in §3 are the build-time enforcement scaffolding for principles 1, 4, and 8.

1. **Daemon-backed architecture.** The daemon is the memory. Clients (SDK `RemoteAdapter`s, the `cm` CLI, agents, the `cm-http` bridge) talk to it; no client gets privileged access. The crate dependency graph enforces this — `cli`, `client`, and `http-bridge` cannot import `daemon`, `store`, `search`, or any other backend crate. Architecture is enforced by Cargo, not convention.

2. **Local-first.** SQLite is canonical. The default install runs entirely on the user's machine: local embedding model, local DB, local Unix socket. No code path uploads memory contents to a service the user has not explicitly configured.

3. **One canonical truth; derived caches are rebuildable.** SQLite is the source of truth. Embedding cache, trace ring buffer, in-RAM model, search index — all derived. Anything derivable can be discarded and rebuilt without data loss. Crash recovery falls out of this for free.

4. **Protocol-first.** The IPC contract is the public surface; everything else is implementation. Additive changes are free; breaking changes bump `IPC_PROTOCOL_VERSION`. Every wire change lands in `PROTOCOL.md` and a fixture in the same commit. Clients and daemon evolve independently behind the contract.

5. **Lifecycle is a first-class concern.** Memory is a lifecycle, not a database — decay, reinforce, consolidate, expire, promote. The daemon owns the schedule; no client elects itself maintainer. This is what distinguishes cognitive-memory from a vector DB with embeddings on top.

6. **Cross-agent visibility by default.** Under one `user_id`, every agent sees every other agent's writes immediately. Project, source-agent, and tags are metadata filters, not isolation walls. The shared-store property *is* the product.

7. **Provider-agnostic internal model.** LLM and embedding providers live behind traits. The daemon's data model knows nothing about which provider produced which embedding. Provider differences that affect behaviour stay visible — a query that requires a paid model surfaces that, it does not silently fall back.

8. **Single writer to SQLite.** Always. The two-pool wrapper enforces this; no code path opens a second writer. Multi-process WAL contention is sidestepped at the architectural level, not papered over with retries.

9. **Operable without provider keys.** Default install requires no credential. Local embedding model loads at startup; hosted providers are opt-in per-request or per-config. The daemon does something useful out of the box.

10. **CLI-first.** Every capability is reachable from `cm`. The CLI is the canonical scripting and automation surface, not a degraded TUI. Any future GUI is a client of the same protocol; nothing is GUI-only.

11. **Pipeable structured output.** `--json` is a product feature on every read command. Agents and scripts are first-class consumers — not afterthoughts retrofitted onto a UI-shaped output format.

12. **Mutations are previewable; destructive mutations are reversible within a window.** Dry-run is the contract for batch and destructive operations. The event log enables undo within a configurable window. No mutation goes "boom, gone, no recourse" without explicit user opt-in (`--yes --no-undo`).

## 3. Architectural rules that are not negotiable

These rules are checked by Cargo, by CI, and by code review. Breaking them is a revert. They are the build-time scaffolding for the principles above (specifically 1, 4, and 8).

1. **Crate dependency graph is the architecture.** If your change requires `crates/cli` to import from `crates/store`, the change is wrong. Re-route through `crates/protocol` and `crates/client`. The graph is documented in `ARCHITECTURE.md` §5. (Enforces principle 1.)
2. **Every state change goes through the protocol.** No back-doors. Tests do not get to call into `crates/store` directly to set up state when the same setup could be expressed as a `Request::Memory(...)`. Bypasses normalise into shipped bypasses. (Enforces principle 4.)
3. **One writer to SQLite.** Use the writer pool from `crates/store`. Do not open a second `SqlitePool` for writes. The two-pool wrapper exists exactly to make this an architectural property. (Enforces principle 8.)
4. **No silent network calls.** Any code path that contacts the network does so via a typed provider trait (`LlmProvider`, `EmbeddingProvider`). Adding a network call elsewhere is a `SECURITY.md`-violating change and gets flagged in review. (Enforces principles 2 and 7.)
5. **Versioned protocol changes are coordinated.** Adding a request kind, response field, or event kind is additive and does not bump `IPC_PROTOCOL_VERSION`. Renaming, removing, or changing the type of an existing field is breaking and requires bumping the version, updating `PROTOCOL.md`, and an SDK release plan. (Enforces principle 4.)

## 4. Documentation discipline

Documentation in this repo is load-bearing. The user explicitly asked for the doc surface to land before code, so that the API is decided before the implementation. Do not regress this.

- A new feature lands with: code change, ADR (if it makes a non-obvious choice), `PROTOCOL.md` update (if the wire surface changed), and `ARCHITECTURE.md` update (if a subsystem boundary moved).
- An ADR for the architectural form: ADRs are short. Title, status, context, decision, consequences. Don't write essays.
- Don't duplicate. If a fact lives in `PROTOCOL.md`, link to it from `ARCHITECTURE.md` rather than restating it.
- `README.md` is the orientation surface. Keep it tight; longer content moves to `docs/`.

## 5. Coding style

Default to Rust idioms. The specific rules:

- `cargo fmt` is enforced. `rustfmt.toml` is the law.
- `cargo clippy -- -D warnings` is enforced. Don't `#[allow(clippy::...)]` without a comment explaining why.
- `unwrap()` and `expect()` are allowed in tests, in `main.rs` initialisation paths, and inside `// SAFETY:` blocks. Anywhere else, use `?` and propagate.
- Errors are typed (`thiserror` for libraries, `anyhow` only at binary boundaries).
- No `unsafe` without a `// SAFETY:` block explaining the invariant.
- Async by default in I/O code. CPU-bound code is sync; the daemon spawns it on `tokio::task::spawn_blocking` if necessary.
- Module names are nouns. Function names are verbs. Types are nouns or adjectives.
- One type per file is not a rule. One concept per file usually is.

## 6. Testing

The testing strategy is in `docs/developer/testing-strategy.md`. The TDD discipline and rubric scoring is in `docs/developer/test-discipline.md`. Summary:

- **TDD by default.** Vertical slicing: one behaviour → one test → minimum code → refactor. Horizontal slicing (write all tests, then all code) is banned — it produces tests shaped by implementation knowledge.
- **5-question gate before every test**: mutation kill, spec-not-impl values, edge case coverage, delete-test, refactor-survival. All five must pass.
- **Three classes of code, three rubric thresholds**: net-new ≥ 24/30, adapted ≥ 21/30, vendored mechanical ≥ 18/30.
- **Unit tests** alongside code (`#[cfg(test)] mod tests { ... }` in the same file).
- **Integration tests** in `tests/` per crate. These exercise crate boundaries with realistic state.
- **Protocol conformance tests** via the golden fixtures in `crates/protocol/tests/fixtures/`. Adding a request kind requires adding at least one fixture.
- **End-to-end tests** in `crates/daemon/tests/` spin up a real daemon against a temp SQLite, run real client requests, assert. No mocks for the store; mocks only at provider boundaries (LLM, embeddings).
- **No mocks for SQLite.** Use a temp file. The mxr lesson here is that mocked SQLite hides bugs that matter (WAL behaviour, locking, migration recovery).

## 7. Working in phases

If you're picking up a phase from `ROADMAP.md`:

1. Read the phase's "done when" criterion; that's the contract.
2. Don't add scope from later phases. If you find yourself wanting Phase 10's extraction code while doing Phase 4's dispatcher, stop — the phasing exists because earlier phases shake out the API for later ones.
3. If a phase's design needs to change after you start it, propose the change in an ADR before writing the code that depends on it.

## 8. Commits

- Conventional commit prefixes (`feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`).
- One logical change per commit. A phase usually maps to many commits.
- No Co-Authored-By footer. No Claude/AI attribution. The user has codified this preference.
- Body explains *why*, not *what*. The diff already shows what.

## 9. Pull requests

- PR description references the relevant ROADMAP phase and any ADRs that govern the change.
- A PR that touches the wire protocol updates `PROTOCOL.md`, the affected SDK protocol mirror, and adds a fixture under `crates/protocol/tests/fixtures/`.
- A PR that introduces a new dependency justifies it. Default: don't add a dependency for what `std` does adequately.
- A PR with new tests records rubric scores for those tests in the description (per `docs/developer/test-discipline.md`).

## 10. When in doubt

The four reference projects to learn the pattern from:

- `mxr` at `/Users/bhekanik/code/planetaryescape/mxr` — production-grade Rust daemon-pattern email client. Two-pool SQLite, length-delimited JSON, four-bucket protocol, idempotent migrations, no auto-spawn. This is the style template *and the source of vendored code*. See `docs/developer/code-reuse.md`.
- `lazydap` at `/Users/bhekanik/code/planetaryescape/lazydap` — same pattern for a debugger. Auto-spawn, broadcast events, `--wait` async-to-sync bridge, dry-run mutations. Source of the auto-spawn and event-broadcast *design*; the lazydap daemon itself is a placeholder, so we write fresh from its docs.
- `cognitive-memory-sdk` next door — the algorithmic source of truth for v6 features being ported into this daemon.
- The user's Obsidian vault has a topic note `Cognitive Memory Daemon.md` (and the broader `The Local Daemon Pattern.md`, `Headless Core + Multiple Clients.md`, `How Daemons Work.md`, `Local IPC vs HTTP.md`) — read those if you want the conceptual framing, not the implementation.

**Vendoring discipline** (codified in [ADR 0009](./docs/decisions/0009-vendor-mxr-lazydap-not-shared-crate.md)): when copying code from mxr or lazydap, copy whole files, mark provenance in a top-of-file comment (`// Adapted from mxr <commit-sha>:<path>`), do mechanical renames in the same commit, and file upstream issues for bug fixes that apply both places. Do not extract a shared crate yet — three projects is too few; revisit at five.

## 11. Out of scope (do not do without asking)

- Implementing distributed memory or multi-machine sync.
- Adding a network listener that binds anything other than `127.0.0.1` or a Unix socket.
- Caching or storing LLM API keys outside of process memory and the OS keychain.
- Removing the SDK's in-process adapters. They serve a different deployment shape and stay first-class.
- Bumping `IPC_PROTOCOL_VERSION` without an SDK release plan.
