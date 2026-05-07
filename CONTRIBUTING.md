# Contributing

Contributions are welcome before there is a contribution surface, but the rough shape:

## Setup

```sh
# Rust toolchain pinned in rust-toolchain.toml
rustup show

# Once Phase 0 lands:
cargo build
cargo test
```

## Workflow

1. Pick a phase or sub-task from [`ROADMAP.md`](./ROADMAP.md).
2. Read [`AGENTS.md`](./AGENTS.md) — the architectural rules and doc discipline apply to everyone.
3. If your change makes a non-obvious decision, write an ADR under `docs/decisions/` *before* the code change. The ADR is what gets reviewed first.
4. If your change touches the wire format, update [`PROTOCOL.md`](./PROTOCOL.md) and add a golden fixture under `crates/protocol/tests/fixtures/` in the same PR.
5. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `cargo deny check` locally before pushing.

## Commits

Conventional commits. One logical change per commit. No AI attribution footers.

## PRs

- Title in conventional-commit format.
- Body cites the ROADMAP phase and any ADRs that govern the change.
- Reviewer asks: does the architectural boundary it crosses still make sense, or has the boundary moved?

## Reporting bugs

A bug report is most useful if it includes:

- Daemon version (`cm --version`).
- Output of `cm doctor`.
- Steps to reproduce, with the exact `cm` invocations or SDK call sites.
- Daemon log excerpt (`~/Library/Logs/cognitive-memory/daemon.log`), redacted of any sensitive content.

## Code of conduct

Be a decent person. The shorter version of every code-of-conduct.
