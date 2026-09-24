# Configuration

The current daemon has a small configuration surface. Runtime topology mostly comes from environment variables; `config.toml` is for LLM provider selection and lifecycle tuning.

## Config file

The daemon reads the active identity's config file:

```text
<config-dir>/<COGNITIVE_MEMORY_INSTANCE>/config.toml
```

Debug builds default to `cognitive-memory-dev`; release builds default to `cognitive-memory`. The base config directory comes from `dirs::config_dir()` unless `COGNITIVE_MEMORY_CONFIG_DIR` is set.

Supported shape:

```toml
[llm]
provider = "none"

# or:
# provider = "openai"
# api_key_env = "OPENAI_API_KEY"
# model = "gpt-4o-mini"

# or:
# provider = "anthropic"
# api_key_env = "ANTHROPIC_API_KEY"
# model = "claude-haiku-4-5-20251001"

# or, with the `local-llm` feature enabled:
# provider = "local"
# model_path = "/absolute/path/to/model.gguf"

[lifecycle.base_decay_rates]
semantic = 240.0
episodic = 45.0
core = 120.0
procedural = inf
```

Every section is optional. Missing `[llm]` means heuristic conflict handling and no LLM consolidation. Missing `[lifecycle]` means the daemon uses the built-in tuned lifecycle defaults.

Use the CLI for LLM edits:

```sh
cm config-get-llm
cm config-set-llm none
cm config-set-llm openai --api-key-env OPENAI_API_KEY --model gpt-4o-mini
cm config-set-llm anthropic --api-key-env ANTHROPIC_API_KEY --model claude-haiku-4-5-20251001
```

## Environment variables

| Variable | Purpose |
| --- | --- |
| `COGNITIVE_MEMORY_INSTANCE` | Runtime identity. Scopes config, data, runtime, cache, logs, PID, socket, and bridge-token paths. |
| `COGNITIVE_MEMORY_SOCKET_PATH` | Socket path for `cm`, `cm-daemon`, and `cm-http`. Overrides only the socket path. |
| `COGNITIVE_MEMORY_CONFIG_DIR` | Base config directory override. |
| `COGNITIVE_MEMORY_DATA_DIR` | Base data directory override; `data.db` lives under this identity-scoped directory. |
| `COGNITIVE_MEMORY_RUNTIME_DIR` | Base runtime directory override; socket and PID live under this identity-scoped directory unless socket/PID are explicitly overridden. |
| `COGNITIVE_MEMORY_CACHE_DIR` | Base cache directory override. |
| `COGNITIVE_MEMORY_LOG_DIR` | Base log directory override. |
| `COGNITIVE_MEMORY_DB_PATH` | Daemon SQLite file override. Used by the CLI when auto-spawning. |
| `COGNITIVE_MEMORY_PID_PATH` | Daemon PID file override. Used by the CLI when auto-spawning. |
| `COGNITIVE_MEMORY_LOG_PATH` | Daemon log file override. Used by the CLI when auto-spawning. |
| `COGNITIVE_MEMORY_LOG` | Tracing filter for `cm-daemon`; falls back to `RUST_LOG`. |
| `COGNITIVE_MEMORY_EMBEDDINGS` | Set to `fake` for fast local/CI daemon runs without loading the local model. |
| `COGNITIVE_MEMORY_DAEMON_BIN` | CLI auto-spawn override for the daemon binary path. Useful in source builds and tests. |
| `RUST_LOG` | Standard tracing filter fallback. |
| `OPENAI_API_KEY` | Read when config points OpenAI at this env var. |
| `ANTHROPIC_API_KEY` | Read when config points Anthropic at this env var. |
| `COGNITIVE_MEMORY_HTTP_BIND` | HTTP bridge bind address. Default `127.0.0.1:7472`; non-loopback values are refused. |
| `COGNITIVE_MEMORY_HTTP_SALT` | Salt for the bridge's in-memory bearer-token hashes. |
| `COGNITIVE_MEMORY_HTTP_BOOTSTRAP_TOKEN` | Registers a bridge token from env at startup. Handy for tests/local scripts. |
| `COGNITIVE_MEMORY_HTTP_BOOTSTRAP_USER` | User namespace for the bootstrap token. Default `default`. |
| `COGNITIVE_MEMORY_HTTP_BOOTSTRAP_SCOPE` | `read`, `write`, or `admin` for the bootstrap token. Default `write`. |
| `COGNITIVE_MEMORY_HTTP_MINT_USER` | Ask the daemon to mint a bridge token at `cm-http` startup and write it to the private bridge-token file. |
| `COGNITIVE_MEMORY_HTTP_MINT_SCOPE` | Scope for minted startup token. Default `write`. |
| `COGNITIVE_MEMORY_HTTP_ALLOWED_HOSTS` | Extra accepted Host headers, comma-separated. Loopback hosts are accepted by default. |
| `COGNITIVE_MEMORY_HTTP_ALLOWED_ORIGINS` | CORS origin allowlist, comma-separated. |
| `COGNITIVE_MEMORY_HTTP_CORS_ORIGINS` | Backward-compatible alias for the CORS origin allowlist. |

## Command-line configuration

`cm` has global flags:

```sh
cm --socket /tmp/cm.sock --user-id alice status
cm --socket /tmp/cm.sock --user-id alice --no-spawn search "..."
cm --json counts
```

`cm-daemon` accepts `--foreground`, `--instance`, `--socket`, `--db`, `--pid`, `--log`, and `--json-logs`. The CLI normally supplies these when it auto-spawns. `cm-http` uses environment variables for bind/Host/CORS/token setup.

## Precedence

- Runtime identity: `COGNITIVE_MEMORY_INSTANCE`; otherwise release/debug default.
- Socket path: CLI `--socket` for `cm`; otherwise `COGNITIVE_MEMORY_SOCKET_PATH`; otherwise the identity-scoped runtime default.
- Daemon data path: `COGNITIVE_MEMORY_DB_PATH`; otherwise identity-scoped data dir `data.db`.
- LLM provider: `config.toml`.
- LLM API key: the env var named by `config.toml`.
- HTTP bridge token source: local bootstrap token if present; otherwise daemon-minted tokens are validated against daemon-owned token storage per request. Startup mint writes a private token file for convenience.

## Operator hygiene

- Keep real provider keys in environment variables, not in `config.toml`.
- Restart the daemon after changing `[llm]`.
- Lifecycle overrides take effect on daemon restart because the config is loaded at startup.
- Use a temporary `COGNITIVE_MEMORY_INSTANCE` and temp `COGNITIVE_MEMORY_*_DIR` overrides for smoke tests so you do not touch your real memory store.
