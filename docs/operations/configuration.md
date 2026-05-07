# Configuration

The daemon resolves configuration in this order (later sources override earlier):

1. Compiled-in defaults.
2. Config file at `~/Library/Application Support/cognitive-memory/config.toml` (if it exists).
3. Environment variables.
4. Command-line flags to `cm-daemon`.
5. Per-request overrides on individual `Request` payloads.

## 1. Config file

```toml
# ~/Library/Application Support/cognitive-memory/config.toml

[daemon]
log_level = "info"           # error | warn | info | debug | trace
request_concurrency_limit = 64
shutdown_grace_seconds = 5

[socket]
# Override path. Default: ~/Library/Application Support/cognitive-memory/cm.sock
path = ""

[store]
# Override DB path. Default: ~/Library/Application Support/cognitive-memory/data.db
db_path = ""
reader_pool_size = 4         # writer pool is always 1

[embeddings]
default_provider = "local"   # local | openai
local_model = "bge-small-en-v1.5"
cache_max_rows = 1_000_000

[embeddings.openai]
# Optional: enables OpenAI as default if default_provider = "openai"
api_key_env = "OPENAI_API_KEY"
default_model = "text-embedding-3-small"

[llm]
default_provider = "openai"  # openai | anthropic
[llm.openai]
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4o-mini"
rate_limit_rps = 10
[llm.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-haiku-4-5-20251001"
rate_limit_rps = 10

[lifecycle]
tick_cadence_seconds = 21_600   # 6 h
decay_model = "exponential"      # exponential | power
beta_c_default = 1.0
core_retention_floor = 0.6
power_decay_gamma = 0.7

[retrieval]
default_alpha = 0.5              # exponent on retention factor in score
default_limit = 10
hybrid_default = false           # opt-in hybrid retrieval
rerank_default = false

[http_bridge]
bind = "127.0.0.1:7472"
token_default_ttl_seconds = 2_592_000   # 30 d
cors_origins = []
```

Every section is optional. Missing sections take defaults.

## 2. Environment variables

Variables override the config file. Use these in CI, ephemeral environments, or to keep secrets out of disk files.

| Variable | Purpose |
| --- | --- |
| `COGNITIVE_MEMORY_SOCKET_PATH` | Override socket path. |
| `COGNITIVE_MEMORY_DB_PATH` | Override DB path. |
| `COGNITIVE_MEMORY_LOG_LEVEL` | Override log level. |
| `COGNITIVE_MEMORY_HTTP_BIND` | Override HTTP bridge bind. Loopback only; non-loopback values cause `cm-http` to refuse. |
| `COGNITIVE_MEMORY_HTTP_CORS_ORIGINS` | Comma-separated CORS origins for `cm-http`. |
| `COGNITIVE_MEMORY_LLM_PROVIDER` | Default LLM provider. |
| `COGNITIVE_MEMORY_EMBEDDING_PROVIDER` | Default embedding provider. |
| `OPENAI_API_KEY` | Picked up by the OpenAI provider (LLM and/or embeddings). |
| `ANTHROPIC_API_KEY` | Picked up by the Anthropic provider. |
| `RUST_LOG` | Standard `tracing-subscriber` filter; overrides `COGNITIVE_MEMORY_LOG_LEVEL`. |

## 3. Command-line flags (daemon)

```sh
cm-daemon --foreground                # don't detach; log to stderr
cm-daemon --config /path/to/config.toml
cm-daemon --socket /path/to/cm.sock
cm-daemon --log-level debug
```

Flags override env vars.

## 4. Per-request overrides

Most `Memory::*` requests accept `embedding_override` and `llm_override`:

```json
{
  "bucket": "Memory",
  "op": "Search",
  "query": "...",
  "embedding_override": {
    "provider": "OpenAI",
    "model": "text-embedding-3-large",
    "api_key": null
  }
}
```

`api_key: null` means "use the daemon's configured key for this provider". A non-null `api_key` overrides the daemon's key for this single request only and is never persisted.

## 5. Precedence reference

| For this property | The winning source is | Then | Then | Then | Then |
| --- | --- | --- | --- | --- | --- |
| LLM API key | per-request override | env var (`OPENAI_API_KEY`, …) | config file `llm.<provider>.api_key_env` | (no key → request fails) | — |
| Embedding provider | per-request override | env var `COGNITIVE_MEMORY_EMBEDDING_PROVIDER` | config `embeddings.default_provider` | compiled-in `local` | — |
| Socket path | env `COGNITIVE_MEMORY_SOCKET_PATH` | `--socket` flag | config `socket.path` | compiled-in default | — |
| Log level | `RUST_LOG` | env `COGNITIVE_MEMORY_LOG_LEVEL` | `--log-level` | config `daemon.log_level` | compiled-in `info` |

The full precedence rules live in code at `crates/daemon/src/config.rs` (Phase 4) and are exercised by tests under `crates/daemon/tests/config_precedence.rs`.

## 6. Operator hygiene

- Treat `config.toml` as configuration, not secrets. Real keys belong in env vars or the OS keychain. The keyring path lands in Phase 13.
- Don't commit `config.toml` to a repo. The defaults are sane; per-machine config goes in `~/Library/Application Support/cognitive-memory/`.
- Changing the embedding model default requires a daemon restart and does not invalidate cached embeddings.
- Changing the decay model (`exponential` ↔ `power`) takes effect immediately at next retrieval; stored memories are not rewritten.
