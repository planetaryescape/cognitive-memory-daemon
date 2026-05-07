# Installation

Pre-Phase-13 placeholder. The current state of the project is "documentation landed, code starts at Phase 0", so there is no binary to install yet. This page records the install plan so the implementation phase has a target to hit.

## Planned distribution channels

1. **`cargo install cognitive-memory-daemon`** — works as soon as the workspace publishes to crates.io (Phase 13).
2. **Homebrew tap** — `brew install bhekanik/tap/cognitive-memory` for the typical Mac install. Tap repo TBD.
3. **Pre-built binary releases** on the GitHub releases page (`cm-daemon`, `cm`, `cm-http`).
4. **Docker image** — out of scope for v0.1.

## What gets installed

| Path | Content |
| --- | --- |
| `$HOMEBREW_PREFIX/bin/cm` (or `~/.cargo/bin/cm`) | CLI binary |
| `$HOMEBREW_PREFIX/bin/cm-daemon` | Daemon binary |
| `$HOMEBREW_PREFIX/bin/cm-http` | HTTP bridge binary (optional service) |
| `~/Library/Application Support/cognitive-memory/` | Created on first run (mode 0700) |
| `~/Library/Application Support/cognitive-memory/data.db` | SQLite store (mode 0600), created on first run |
| `~/Library/Application Support/cognitive-memory/cm.sock` | Unix socket, created on daemon start (mode 0700) |
| `~/Library/Application Support/cognitive-memory/cm.pid` | PID file (single-instance via signal-probe — see ARCHITECTURE.md §3.2) |
| `~/Library/Application Support/cognitive-memory/models/` | Embedding model files; populated on first embedding call |
| `~/Library/Logs/cognitive-memory/daemon.log` | Daemon log (mode 0600) |
| `~/Library/Logs/cognitive-memory/http.log` | HTTP bridge log if running |

## First-run experience (target)

```sh
$ cm store "User dislikes mocked database tests."
[cm-daemon starting in background...]
[cm-daemon: downloading bge-small-en-v1.5 (130 MB)...]
[cm-daemon ready]
stored: mem_01HZ...
```

The first call may take ~10 s while the model downloads. Subsequent calls are sub-100 ms.

## Boot-time launch

For agents that should always have memory available:

- **macOS**: a `LaunchAgent` plist in `~/Library/LaunchAgents/com.bhekanik.cognitive-memory.plist`. Template ships in Phase 13.
- **Linux**: a `systemd --user` unit. Phase 13.

The daemon is fine to leave to auto-spawn on first CLI use too; the boot-time launch is for users who want subscription events delivered without the first agent paying the cold start.

## Uninstall

```sh
# macOS, after launchd unload if applicable:
brew uninstall cognitive-memory
rm -rf ~/Library/Application\ Support/cognitive-memory
rm -rf ~/Library/Logs/cognitive-memory
```

`cargo install` users: `cargo uninstall cognitive-memory-daemon` plus the same `rm` steps.

## Verification

After install:

```sh
cm doctor
```

Expected output: every check `OK`. Anything else is a bug; file an issue with the doctor report attached.
