# ADR 0009 — Vendor IPC and daemon plumbing from mxr/lazydap; do not extract a shared crate

- **Status**: Accepted
- **Date**: 2026-05-07
- **Deciders**: bhekanik

## Context

Cognitive-memory-daemon will reuse a substantial chunk of the IPC and daemon-process code from `mxr` and (for some patterns) `lazydap`. Both upstream projects are the user's own work, dual MIT-OR-Apache-2.0 licensed, so the legal layer is trivial.

The architectural question: how do we organise the reuse?

1. **Vendor (copy + adapt).** Copy specific files from upstream into this repo, mark provenance in a comment, rename identifiers, evolve independently. Discipline: when fixing a bug in vendored code that exists upstream, file an issue or PR upstream.
2. **Extract a shared crate.** Pull the IPC codec, two-pool SQLite wrapper, accept-loop framework, etc. into a new repo (`local-daemon-toolkit` or similar), depend on it from mxr, lazydap, and cognitive-memory-daemon.
3. **Reference-and-rewrite.** Read mxr's code as a pattern reference, write fresh in cognitive-memory-daemon. No shared identifiers; both implementations evolve independently from day one.

## Decision

Vendor and adapt (option 1).

Concrete file-by-file mapping is in [`docs/developer/code-reuse.md`](../developer/code-reuse.md).

## Reasoning

**Why not extract a shared crate (yet)?**

- Three projects is too few to amortise the coordination cost. A shared crate creates a release pipeline (semver bumps, changelog, CI matrix across consumers), version-pinning headaches, and a "whose responsibility is this bug?" question that doesn't exist when each project owns its own copy.
- The patterns are similar but not identical. Mxr uses SQLite as canonical state; lazydap uses TOML for small state. Mxr is per-OS-user; lazydap is per-project. Mxr requires manual daemon start; lazydap auto-spawns. A shared crate would either bake in one set of choices and leak them into all three, or expand to N-axis configurability that's harder to read than N independent implementations.
- Premature extraction creates the worst kind of abstraction — one shaped by two consumers' shared subset, which breaks when the third consumer arrives with a slightly different need. We don't have a fourth or fifth consumer to validate the shared shape against.
- The user has codified "minimal blast radius" and "do it right" globally. Vendoring lets each project change at its own pace; extraction couples the three.

**Why not reference-and-rewrite?**

- Mxr's code is genuinely production-tested. Rewriting from a reference for the sake of avoiding upstream coupling re-introduces every bug mxr has already fixed.
- The legal layer makes literal copying free. The only cost of vendoring over rewriting is the discipline of keeping a backport channel open — much cheaper than re-debugging a known issue.

**Vendor discipline:**

1. Copy the file whole. No paraphrasing, no "improvements" in the same commit.
2. Add a top-of-file comment: `// Adapted from mxr <commit-sha>:<path>` (or `lazydap`).
3. Mechanical rename of `mxr_*` / `lazydap_*` identifiers to `cognitive_memory_*`.
4. Bug fixes that apply to upstream get filed as issues or PRs upstream in the same week.
5. Re-pull periodically (every few months) — diff against current upstream, port relevant fixes.

## Revisit triggers

Move from vendor to extracted shared crate when **any two** of the following hold:

- A fourth or fifth project adopts the same pattern.
- A bug or feature touches the same file in three+ consumers in the same quarter.
- A consumer is rewritten that would otherwise be a near-verbatim copy of an existing consumer's plumbing.

Until then, vendor.

## Consequences

### Positive

- Each project is independently buildable and releasable.
- Bug fixes happen at the consumer's own pace.
- No shared-crate semver coordination.
- New patterns can be tried in one project without affecting the others.
- Vendoring is reversible — extracting a shared crate later is the standard refactor; un-extracting is harder.

### Negative

- Bug fixes need manual backports across consumers. Mitigation: the file-level provenance comment makes "where does this code come from" obvious; a `make sync-from-mxr` script can later automate diff-and-merge if it becomes painful.
- Consumers drift over time. Acceptable: drift is information about what the patterns actually need to be when generalised.
- New contributors need to learn that "vendored from mxr" means "the upstream is the conceptual source of truth even though we own this copy".

### Neutral

- The three projects (mxr, lazydap, cognitive-memory-daemon) effectively form a meta-pattern even without a shared crate. The obsidian notes (`The Local Daemon Pattern`, `How Daemons Work`) are the conceptual shared layer; the code-level shared layer is deferred.

## Alternatives considered

- **Shared crate (`local-daemon-toolkit`).** Rejected for amortisation and premature-abstraction reasons above. Reconsider at the trigger conditions.
- **Reference-and-rewrite.** Rejected — re-paying for fixed bugs.
- **Fork mxr into a generic daemon framework.** Rejected — turns mxr into a library it wasn't designed to be; adds a dependency the user doesn't want from a project the user doesn't want to constrain.

## References

- `docs/developer/code-reuse.md` for the file-by-file map.
- `AGENTS.md` §9 (the four reference projects).
- Obsidian: `The Local Daemon Pattern.md`, `How Daemons Work.md`, `Headless Core + Multiple Clients.md`.
