# ADR 0008 — Clean slate: no migration from existing SDK stores

- **Status**: Accepted
- **Date**: 2026-05-07
- **Deciders**: bhekanik

## Context

The existing `cognitive-memory-sdk` (Python and TypeScript, v0.2.0 on PyPI/npm) ships with several adapters: InMemory, SQLite, Postgres, Convex. Library users may already have data in any of those stores.

The question: when the daemon ships, must it open and migrate an existing SDK SQLite file, or can it start with a clean slate?

The user's call: assume no existing memories. Clean slate.

## Decision

The daemon owns its own SQLite file (`~/Library/Application Support/cognitive-memory/data.db`) and does not migrate from any existing SDK adapter store. Users adopting daemon mode start with an empty memory store. Library-mode users remain on their existing adapter unchanged.

The daemon's schema is owned by `crates/store/migrations/` and is not constrained to be a superset of any SDK adapter schema.

## Reasoning

**Why this is OK:**
- The user explicitly stated they have no existing data to migrate.
- The library-mode SDK keeps working unchanged. Anyone with existing data has a path: keep using library mode against that data, or start fresh in daemon mode.
- A migration tool can be added later if the need emerges (post-v1 backlog). Adding it later is strictly easier than adding it now under the constraint of "the schema we ship in v1 must accommodate four existing schemas".

**Why we don't try to be a drop-in replacement schema-wise:**
- The four existing SDK adapters have four different schemas. Designing a daemon schema that accommodates all of them would be a major design constraint for marginal benefit.
- The daemon has v6 features (memory_type, valid_from/until, ttl, retention_floor, association weights, event log) that some SDK adapters don't have. Forcing the daemon to be backwards-compatible with adapter schemas that lack these would mean adding optional columns and special-casing missing-column behaviour. Schema-design soup.

**What we avoid by going clean:**
- No "phantom column" defaults that break invariants.
- No "is this an old or new memory" branches in handlers.
- No pre-Phase-1 design discussion about which existing SDK adapter shape to mirror.

## Consequences

### Positive

- Schema design is unconstrained by legacy. We pick what's right for the daemon.
- Phase 1 work is self-contained: define the schema, write the migrations, ship.
- No regression surface from adapter compatibility.

### Negative

- Existing library-mode users who want to switch to daemon mode would lose their memories (in v1). Mitigation: a future migration tool, or they can manually export and re-ingest. Not in scope for v1.
- Anyone who *thought* the daemon would be a transparent backend swap will be surprised. Documentation is the mitigation: `README.md`, `docs/concepts/memory-model.md` §1 explicitly call out that library mode and daemon mode are different deployment shapes, not different backends of the same store.

### Neutral

- The SDK's `RemoteAdapter` (Phase 6+, Phase 7+) is the bridge from SDK API to daemon. Existing in-process adapters remain available; users pick the adapter that matches their deployment shape.

## Migration tool (deferred)

If migration ever becomes a need, the shape would be:

```sh
cm import --from sqlite:///path/to/old.db --user-id default
cm import --from postgres://... --user-id default
cm import --from jsonl:///path/to/export.jsonl --user-id default
```

Each importer is a small program that reads the source schema, projects to the daemon's `Memory::Store` shape, and uses `Memory::Ingest` for dedup. Adding this is a few-day project once the daemon is stable.

## Alternatives considered

- **Daemon opens existing SDK SQLite files in-place.** Rejected for schema-shape reasons above.
- **Daemon ships with a built-in migrator from each existing adapter.** Rejected as scope creep for v1; defer to post-v1 if demand emerges.
- **Single SQLite file shared between SDK and daemon, locked at the OS level.** Rejected: two writers to one DB defeats the single-writer invariant; coordination is more trouble than running the daemon's own DB.

## References

- `docs/concepts/memory-model.md` §1 (deployment shapes).
- `ARCHITECTURE.md` §6 (storage).
- ROADMAP Phase 1 (storage scope).
