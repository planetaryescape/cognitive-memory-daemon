# ADR 0006 — Shared `user_id` namespace; project as metadata

- **Status**: Accepted
- **Date**: 2026-05-07
- **Deciders**: bhekanik

## Context

Multiple AI agents (Claude Code, Cursor, custom scripts) on the same machine should share a memory store. The question is *how much* sharing.

The two main shapes considered:

1. **Per-project tenancy.** Each project (or repo, or directory) gets its own `user_id`-equivalent namespace. Memories are isolated by default; cross-project reading is opt-in.
2. **Shared user-id, project-as-metadata.** All agents under the same OS user share the same namespace. The project (or repo, or other slicing dimension) lives in `metadata`. Searches default to all-projects; project filter narrows.

## Decision

Shape (2). The default `user_id` namespace is shared across all agents on the same machine (and the OS user identity). Project, source agent, tags, and other slicing live in `metadata`. Searches default to all-projects unless filtered.

## Reasoning

The user's stated intent for the daemon was "any AI agent I use on my computer to speak with the same memory store". Per-project tenancy fights that intent — it would default to *not* sharing memory across projects, and re-create the fragmentation the daemon exists to remove.

Concrete examples that decide it:

- A user preference learned in one project ("dislikes mocked tests") is just as relevant in the next project. Per-project tenancy means re-learning every preference per project.
- A fact about the user (their dog's name, their timezone) is identity-level, not project-level. It belongs to the user, not a directory.
- An agent debugging an issue in repo A may ask "have I seen this stack trace before?" — and the answer might live in a memory written from repo B. Cross-project visibility is the feature.

The cost is that agents who *want* per-project isolation (rare, but possible) need to opt in. Filter on `metadata.project = "..."`. Also possible to use a different `user_id` for project-level walling, but we expect this to be uncommon.

The mxr pattern is also instructive: mxr is per-OS-user, not per-account-folder, and that has not been a problem because email is identity-scoped, not project-scoped. Cognitive memory is the same shape.

## Consequences

### Positive

- Default behaviour matches user intent.
- Memory is genuinely useful as a unified user-level store, not a per-project notepad.
- Agents can opt into per-project isolation when they need it (filter on `metadata.project`).
- Aligns with the existing SDK's multi-tenancy model: `user_id` is the only hard isolation key.

### Negative

- Cross-project leakage is possible by accident. An agent that should only see project X's memories can see all unless it filters. Mitigation: clients that need isolation set the filter; the daemon does not enforce it.
- Storing project in `metadata` means project-scoped queries don't get a dedicated index automatically. Mitigation: SQLite indexes on common metadata keys (Phase 1+) — `(user_id, metadata->>'project')` is a candidate.

### Neutral

- Multiple `user_id`s are still supported; the daemon doesn't preclude per-tenant isolation when an explicit `user_id` distinction is wanted.
- Agents identify themselves via `metadata.source_agent` (e.g. `"claude-code"`, `"cursor"`). The daemon does not enforce that an agent only sees its own writes; that's also a filter, not a wall.

## Alternatives considered

- **Per-project tenancy as default.** Rejected as it undermines the unified-memory goal.
- **Shared by default, per-project switch.** Equivalent to what we chose; the "switch" is a metadata filter rather than a different `user_id`. Cleaner this way.
- **Per-agent tenancy.** Rejected for the same reason as per-project: re-fragmentation.

## References

- `docs/concepts/memory-model.md` §3 (tenancy).
- `cognitive-memory-sdk/sdks/python/src/cognitive_memory/core.py:75` (existing `user_id` plumbing).
