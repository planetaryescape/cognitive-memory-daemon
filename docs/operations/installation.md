# Installation

The daemon is implemented and runnable from source. A v0.1.0 release tag has not been cut yet, so the reliable install path today is a local build.

## From source

```sh
git clone https://github.com/bhekanik/cognitive-memory.git
cd cognitive-memory/cognitive-memory-daemon
cargo build --workspace
```

The build produces three binaries:

| Binary | Purpose |
| --- | --- |
| `target/debug/cm` | CLI client. Auto-spawns `cm-daemon` unless `--no-spawn` is set. |
| `target/debug/cm-daemon` | Local Unix-socket daemon. Owns SQLite, embeddings, lifecycle, and IPC. |
| `target/debug/cm-http` | Optional loopback HTTP bridge for clients that cannot speak Unix sockets. |

## First run

```sh
target/debug/cm status
target/debug/cm store "User prefers concise docs."
target/debug/cm search "documentation preference"
```

`cm` probes the socket and starts `cm-daemon` if it is missing. The first run may spend time loading or downloading `bge-small-en-v1.5`; later calls reuse the daemon process and the shared embedding cache.

## Runtime paths

By default on macOS:

| Path | Content |
| --- | --- |
| Identity-scoped runtime dir, `cm.sock` | Unix socket. |
| Identity-scoped runtime dir, `cm-daemon.pid` | PID file. |
| Identity-scoped data dir, `data.db` | SQLite store. |
| Identity-scoped config dir, `config.toml` | Optional daemon config file. |
| Identity-scoped cache dir, `models/` | Local LLM downloads from `cm download-model`. |
| Identity-scoped log dir, `daemon.log` | Daemon log file. |

Override the socket with:

```sh
COGNITIVE_MEMORY_SOCKET_PATH=/tmp/cm.sock target/debug/cm status
```

For an isolated full-state run, prefer a throwaway instance and temp dirs:

```sh
tmp="$(mktemp -d)"
COGNITIVE_MEMORY_INSTANCE=smoke \
COGNITIVE_MEMORY_CONFIG_DIR="$tmp/config" \
COGNITIVE_MEMORY_DATA_DIR="$tmp/data" \
COGNITIVE_MEMORY_RUNTIME_DIR="$tmp/runtime" \
COGNITIVE_MEMORY_CACHE_DIR="$tmp/cache" \
COGNITIVE_MEMORY_LOG_DIR="$tmp/logs" \
target/debug/cm status
```

## HTTP bridge

Start the bridge only when you need HTTP:

```sh
target/debug/cm mint-token --scope write
COGNITIVE_MEMORY_HTTP_BOOTSTRAP_TOKEN="<token>" target/debug/cm-http
```

The bridge binds `127.0.0.1:7472` by default and refuses non-loopback addresses.

## Planned release channels

- crates.io / `cargo install`
- Homebrew formula from `packaging/homebrew/cognitive-memory.rb`
- GitHub release tarballs for macOS and Linux

Those artifacts are prepared in the repo, but the release has not been cut.

## Verification

For local development:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

For a quick live check, use an isolated socket:

```sh
tmp="$(mktemp -d)"
COGNITIVE_MEMORY_SOCKET_PATH="$tmp/cm.sock" target/debug/cm status
```
