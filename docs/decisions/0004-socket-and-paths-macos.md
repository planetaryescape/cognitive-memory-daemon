# ADR 0004 — Socket and filesystem paths (macOS-first)

- **Status**: Accepted
- **Date**: 2026-05-07
- **Deciders**: bhekanik

## Context

The daemon needs filesystem locations for:
- The Unix socket itself.
- The PID file (single-instance enforced by signal-probe — `SIGZERO` to the recorded PID via `nix::sys::signal::kill`, not `flock`; matches mxr).
- The SQLite database file.
- The embedding model files.
- Log files.

The user's primary platform is macOS. Linux is in-scope for later but not v1.

## Decision

| Asset | Path (macOS) |
| --- | --- |
| Socket | `~/Library/Application Support/cognitive-memory/cm.sock` |
| PID file | `~/Library/Application Support/cognitive-memory/cm.pid` |
| SQLite DB | `~/Library/Application Support/cognitive-memory/data.db` |
| Embedding models | `~/Library/Application Support/cognitive-memory/models/` |
| Daemon log | `~/Library/Logs/cognitive-memory/daemon.log` |
| HTTP bridge log | `~/Library/Logs/cognitive-memory/http.log` |
| Config file (optional) | `~/Library/Application Support/cognitive-memory/config.toml` |

Parent directories: mode `0700`. Socket: `0700`. DB / PID / logs: `0600`. Created on daemon start with `umask 077`.

Override: `COGNITIVE_MEMORY_SOCKET_PATH` (and per-asset env vars listed in `docs/operations/configuration.md` §2). All overrides validated against the same permission rules.

Linux fallback (when added): `$XDG_RUNTIME_DIR/cognitive-memory/cm.sock` for the socket, `$XDG_DATA_HOME/cognitive-memory/` (default `~/.local/share/cognitive-memory/`) for data, `$XDG_CACHE_HOME/cognitive-memory/` (default `~/.cache/cognitive-memory/`) for the model cache, `$XDG_STATE_HOME/cognitive-memory/` (default `~/.local/state/cognitive-memory/`) for logs.

## Reasoning

**Why `~/Library/Application Support/` and not `$TMPDIR/` for the socket?**
- macOS reaps `$TMPDIR/` (`/var/folders/...`) periodically and on reboot. A socket living there would vanish, sometimes. Application Support is durable.
- Socket and DB live in the same directory: `cm doctor` and uninstall instructions stay simple.

**Why `~/Library/Application Support/` and not `~/.config/`?**
- macOS convention. Tools that try to look like macOS-native belong in `Library/`. Mxr uses `Library/Application Support/mxr/`. The convention transfers.
- `~/.config/` works on Linux but is non-standard on macOS; we'd be importing Linux conventions.

**Why `~/Library/Logs/` for logs?**
- The Console.app convention, which any macOS user can find. Crash dumps and logs colocate. macOS's log rotation tooling looks here.

**Why mode 0700 / 0600?**
- Owner-only access. The threat model (`SECURITY.md` §1, T1) names same-user processes as in-scope; non-owner OS users are out of scope at the OS level. 0700/0600 is the minimum that satisfies T1 and T2 (key handling, since keys live in process memory but the log file might transitively expose them in a regression).

**Why `cm.sock` and not `cm.socket` or `daemon.sock`?**
- Mxr precedent (`mxr.sock`). Short, obviously a socket from the extension, not collide-prone with subdirectory names.

## Consequences

### Positive

- One install location per user; uninstall is one `rm -rf`.
- Predictable for documentation and `cm doctor` reporting.
- Mxr-aligned, so tooling and habits transfer.
- Linux paths follow XDG, the closest thing Linux has to a convention.

### Negative

- Hardcoded paths require env-var override for non-standard installs (e.g., a sandboxed test environment wanting an isolated install). The override mechanism is the mitigation; tests use `COGNITIVE_MEMORY_SOCKET_PATH` plus a `tempfile::tempdir()` per test.
- macOS `$HOME` resolution from a daemon spawned by a different process inherits the spawner's `$HOME`. Unlikely to bite given how `cm` spawns `cm-daemon` (same user, same env), but worth being aware of.

### Neutral

- iCloud Drive: `~/Library/Application Support/` is *not* iCloud-synced unless an app opts in. We do not opt in. The cognitive memory store stays local.
- Apple's "Containers" sandbox path (`~/Library/Containers/...`) is irrelevant — this daemon is not a Mac App Store app.

## Alternatives considered

- **`$TMPDIR/cognitive-memory/cm.sock`.** Rejected for ephemerality.
- **`~/.cognitive-memory/cm.sock`.** Rejected for not following macOS conventions.
- **`/usr/local/var/cognitive-memory/cm.sock`.** Multi-user-shared paths conflict with the per-OS-user-daemon model.
- **Per-installation random path.** Discoverability nightmare; rejected.

## References

- `SECURITY.md` for the permission rationale.
- `docs/operations/installation.md` for the full filesystem footprint.
- mxr: `crates/config/src/resolve.rs:72-89` for the equivalent path resolution.
