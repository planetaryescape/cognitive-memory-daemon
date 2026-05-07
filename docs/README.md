# docs

Index for the daemon's deeper documentation. Top-level docs (`README.md`, `ARCHITECTURE.md`, `PROTOCOL.md`, `ROADMAP.md`, `SECURITY.md`, `AGENTS.md`, `CONTRIBUTING.md`) live in the repo root.

## Concepts

Subsystem-level explanations of how things work and why.

- [`concepts/memory-model.md`](./concepts/memory-model.md) — what the daemon stores, how multi-tenancy works, library vs daemon deployment shapes.
- [`concepts/embedding-strategy.md`](./concepts/embedding-strategy.md) — local default, hosted overrides, cache, dimension handling.
- [`concepts/http-bridge.md`](./concepts/http-bridge.md) — `cm-http` topology, bearer auth, URL surface.

## Operations

How to install, configure, run, observe, and troubleshoot.

- [`operations/installation.md`](./operations/installation.md) — install plan and filesystem footprint.
- [`operations/configuration.md`](./operations/configuration.md) — config file, env vars, flags, precedence.
- [`operations/observability.md`](./operations/observability.md) — logs, traces, doctor.
- [`operations/troubleshooting.md`](./operations/troubleshooting.md) — common failure modes.

## Developer

How to extend the daemon.

- [`developer/testing-strategy.md`](./developer/testing-strategy.md) — test levels, mock policy, CI.
- [`developer/test-discipline.md`](./developer/test-discipline.md) — TDD methodology, the 5-question gate, rubric scoring, mutation testing, per-PR procedure. Not optional.
- [`developer/adding-a-request.md`](./developer/adding-a-request.md) — recipe for adding a new request type end-to-end.
- [`developer/code-reuse.md`](./developer/code-reuse.md) — file-by-file map of what to vendor from mxr and lazydap, per phase.

## Decisions (ADRs)

Architecture decision records. Numbered. Each one captures a non-obvious decision with context, reasoning, and consequences.

- [`decisions/0001-rust-as-daemon-language.md`](./decisions/0001-rust-as-daemon-language.md)
- [`decisions/0002-request-response-event-protocol.md`](./decisions/0002-request-response-event-protocol.md)
- [`decisions/0003-bge-small-default-embedding.md`](./decisions/0003-bge-small-default-embedding.md)
- [`decisions/0004-socket-and-paths-macos.md`](./decisions/0004-socket-and-paths-macos.md)
- [`decisions/0005-http-bridge-localhost-bearer.md`](./decisions/0005-http-bridge-localhost-bearer.md)
- [`decisions/0006-shared-userid-project-metadata.md`](./decisions/0006-shared-userid-project-metadata.md)
- [`decisions/0007-llm-key-precedence.md`](./decisions/0007-llm-key-precedence.md)
- [`decisions/0008-clean-slate-no-migration.md`](./decisions/0008-clean-slate-no-migration.md)
- [`decisions/0009-vendor-mxr-lazydap-not-shared-crate.md`](./decisions/0009-vendor-mxr-lazydap-not-shared-crate.md)

When adding an ADR, copy the format of an existing one. ADRs are immutable once accepted; supersede with a new ADR rather than editing.
