# cognitive-memory CLI command reference

Three binaries ship together:
- `cm` — the CLI that agents and humans use day-to-day.
- `cm-daemon` — the long-running service. `cm` auto-spawns it; you rarely run it directly.
- `cm-http` — optional loopback HTTP bridge for browser clients.

## Table of contents
- [Global flags](#global-flags)
- [`cm status`](#cm-status)
- [`cm store`](#cm-store)
- [`cm search`](#cm-search)
- [Output formats](#output-formats)
- [Daemon lifecycle](#daemon-lifecycle)
- [HTTP bridge (`cm-http`)](#http-bridge-cm-http)
- [Memory model](#memory-model)
- [Environment variables](#environment-variables)
- [Exit codes](#exit-codes)

---

## Global flags

These apply to every subcommand:

| Flag | Default | Description |
|---|---|---|
| `--socket <path>` | `~/Library/Application Support/cognitive-memory/cm.sock` (macOS) / `$XDG_RUNTIME_DIR/cognitive-memory/cm.sock` (Linux) | Override the daemon socket path. Also reads `COGNITIVE_MEMORY_SOCKET_PATH`. |
| `--user-id <id>` | `default` | Tenancy key. Memories are isolated by `user_id`. |
| `--json` | off | Emit JSON instead of human-readable output (read commands). |
| `--no-spawn` | off | Disable auto-spawn. With this, `cm` errors if the daemon isn't running instead of forking it. |

---

## `cm status`

Show daemon status: version, memory count, uptime.

```bash
cm status
# daemon: 0.0.1 (memories: 42, uptime: 12345s)

cm status --json
# {"kind":"Status","daemon_version":"0.0.1","uptime_seconds":12345,"memory_count":42}
```

---

## `cm store`

Store a memory. Returns the assigned ULID.

```bash
cm store "I prefer tea over coffee."
# stored: mem_01KR0FVJ3ZHG0NTZ44FD49DBDQ
```

Flags:

| Flag | Default | Values |
|---|---|---|
| `--category <c>` | `semantic` | `episodic` / `semantic` / `procedural` / `core` |
| `--type <t>` | `fact` | `fact` / `preference` / `plan` / `transient_state` / `other` |
| `--metadata <json>` | `{}` | Free-form JSON object string. Common keys: `project`, `source`, `tags`. |

Examples:

```bash
# Preference
cm store --type preference "Prefers vim keybindings."

# Project-scoped fact
cm store \
  --metadata '{"project":"cognitive-memory","source":"plan-review"}' \
  "Vendored mxr's two-pool wrapper rather than reimplement."

# Episodic note
cm store --category episodic \
  "On 2026-04-12 the daemon crashed during heavy concurrent writes; root cause: stale PID file."
```

---

## `cm search`

Semantic search over memories under the connection's `user_id`.

```bash
cm search "favourite drink"
# 0.275  mem_01KR0FVJ...  I prefer tea over coffee.
# -0.234 mem_01KR0FVJ...  Daily standup at 09:00.
```

Flags:

| Flag | Default | Description |
|---|---|---|
| `--limit <n>` | `10` | Maximum results. |
| `--deep-recall` | off | Include expired (`valid_until` past) memories. |
| `--hybrid` | off | Hybrid retrieval: dense + BM25 fused via Reciprocal Rank Fusion. |

Output is `{score}\t{id}\t{content}` per line; with `--json` it's a single JSON object.

Examples:

```bash
# Top 3 results
cm search --limit 3 "what does the user like"

# Hybrid for keyword-heavy queries (rare technical terms, proper nouns)
cm search --hybrid "rust async tokio"

# Machine-readable for piping
cm search --json "deployment runbook" | jq -r '.results[].content'

# Surface even expired memories
cm search --deep-recall "old API key"
```

---

## Output formats

| Command | Default | `--json` |
|---|---|---|
| `cm status` | `daemon: <ver> (memories: <n>, uptime: <s>s)` | `{"kind":"Status",...}` |
| `cm store` | `stored: <id>` | `{"kind":"MemoryStored","id":"<id>"}` |
| `cm search` | `<score>\t<id>\t<content>` per line | `{"kind":"MemorySearchResults","results":[...]}` |

Errors always go to stderr; the exit code is non-zero.

---

## Daemon lifecycle

The daemon (`cm-daemon`) is the long-running process. The CLI auto-spawns
it the first time a command needs it.

```bash
# Manual start in foreground (Ctrl-C to stop):
cm-daemon
# 2026-05-07T... INFO cognitive_memory_daemon::server: socket bound path=/Users/.../cm.sock
# 2026-05-07T... INFO cognitive_memory_daemon::server: daemon listening
```

The daemon respects:

| Env var | Effect |
|---|---|
| `COGNITIVE_MEMORY_SOCKET_PATH` | Override socket location (DB and logs are co-located in the same parent directory). |
| `RUST_LOG` | Tracing filter (e.g. `RUST_LOG=cognitive_memory=debug`). |

Stop: `kill <pid>` or Ctrl-C. The accept loop drains in-flight requests
within a 5-second deadline.

The default install ships with the local embedding model
(`bge-small-en-v1.5` via fastembed-rs). First run downloads ~130 MB;
subsequent runs load from cache.

---

## HTTP bridge (`cm-http`)

Loopback-only HTTP/JSON proxy. Use when a client can't speak Unix sockets
(browsers, certain language runtimes).

```bash
# Start the bridge (mints a token from the daemon at startup):
COGNITIVE_MEMORY_HTTP_MINT_USER=default cm-http
# Token printed once to stderr — store it.

# Then call:
curl -H "Authorization: Bearer <token>" \
  -X POST http://127.0.0.1:7472/memory/store \
  -d '{"content":"HTTP path works."}'

curl -H "Authorization: Bearer <token>" \
  -X POST http://127.0.0.1:7472/memory/search \
  -d '{"query":"HTTP path works.","limit":5}'
```

Routes:

| Method + path | Maps to |
|---|---|
| `POST /memory/store` | `cm store` |
| `POST /memory/search` | `cm search` (accepts `query`, `limit`, `deep_recall`, `hybrid`) |

Unauthorized requests return 401. The bridge refuses to bind anything but
loopback (127.0.0.1 / ::1).

---

## Memory model

Every memory carries:
- `id` — ULID, prefixed `mem_`. Stable.
- `content` — the natural-language form.
- `category` — coarse classification (CoALA-aligned).
- `memory_type` — orthogonal axis from the v6 spec.
- `metadata` — free-form JSON; `project`, `source`, `tags` are conventions.
- `embedding` — 384-dim vector under bge-small (or hosted-provider's dim).
- Lifecycle fields — `last_accessed_at`, `retrieval_count`, `retention_floor`,
  `valid_from`, `valid_until`, `ttl_seconds`. Decay/promotion logic in the
  daemon's lifecycle layer.

`user_id` is the hard tenancy key. Project, source-agent, tags live in
`metadata` — filter on them rather than spawning new tenants.

---

## Environment variables

| Variable | Used by | Effect |
|---|---|---|
| `COGNITIVE_MEMORY_SOCKET_PATH` | all | Override Unix socket path. |
| `COGNITIVE_MEMORY_DAEMON_BIN` | `cm` | Path to `cm-daemon` for auto-spawn (defaults to PATH lookup + sibling-of-cm). |
| `COGNITIVE_MEMORY_HTTP_BIND` | `cm-http` | Bind address (loopback only; default `127.0.0.1:7472`). |
| `COGNITIVE_MEMORY_HTTP_SALT` | `cm-http` | Salt for bearer-token hashing. |
| `COGNITIVE_MEMORY_HTTP_MINT_USER` | `cm-http` | When set, bridge mints a token from the daemon at startup for this user. |
| `COGNITIVE_MEMORY_HTTP_MINT_SCOPE` | `cm-http` | `read` / `write` / `admin` (default `write`). |
| `COGNITIVE_MEMORY_HTTP_BOOTSTRAP_TOKEN` | `cm-http` | Pre-shared token (test/CI fallback when `MINT_USER` isn't set). |
| `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` | daemon | Picked up by hosted-provider extraction calls when configured. |
| `RUST_LOG` | all | Tracing filter. |

---

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. |
| 1 | Connect or argument error (bad flags, daemon unreachable with `--no-spawn`). |
| 2 | Daemon returned a typed error (`InvalidPayload`, `StorageError`, `ProviderError`, etc.). |

---

## Notes for AI agents

- **Auto-spawn is invisible** — never tell the user to "start the daemon
  first". `cm` handles it.
- **Always pass `--user-id`** when the user has multiple identities; don't
  default to `default` if the user has been working in a project namespace.
- **Always pass `--json`** when piping into another tool.
- **For semantically-rich content, use the default search.** For
  keyword-heavy queries with rare terms, add `--hybrid`.
- **Set `--type preference`** for things the user *wants* (preferences,
  conventions). Use `--type plan` for things they *intend to do*.
- **Keep memory content third-person paraphrased.** "User prefers tea over
  coffee" not "I prefer tea over coffee" — the user_id already encodes
  whose preference it is, and third-person reads better for cross-agent
  consumption.
- **Don't over-store.** A memory should be durable and useful in future
  sessions. Ephemeral state belongs in your conversation context, not
  here.
