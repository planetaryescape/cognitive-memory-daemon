# Observability

Three pillars: logs, traces, doctor. Metrics are a Phase 11 follow-up.

## 1. Logs

`tracing` from line one of `main`. Output target depends on launch mode.

| Mode | Output |
| --- | --- |
| `cm-daemon --foreground` | Stderr, human-readable (`tracing-subscriber::fmt`) |
| Detached (auto-spawned by CLI, or via launchd) | `~/Library/Logs/cognitive-memory/daemon.log`, JSON lines (`tracing-subscriber::fmt::json`) |

Rotation: daily, keep 14 days, via `tracing-appender::rolling::Builder`.

Default level: `info`. Override with `RUST_LOG` (full `tracing-subscriber` syntax) or `COGNITIVE_MEMORY_LOG_LEVEL` (simpler, accepts `error|warn|info|debug|trace`).

Sample log lines you should see:

```
INFO  cm_daemon::server: socket bound path=/Users/.../cm.sock
INFO  cm_daemon::handler: dispatch request_id=42 bucket=Memory op=Search user_id=default
DEBUG cm_search::hybrid: dense_ms=4.2 sparse_ms=1.3 candidates=58
INFO  cm_daemon::handler: complete request_id=42 status=ok elapsed_ms=8
```

What you should **not** see, regardless of level:

- Memory content at INFO or below.
- Any LLM or embedding API key, in any field.
- Any bridge token, in any field.

A regression test under `crates/daemon/tests/no_secrets_in_logs.rs` (Phase 11) asserts this for known test secrets.

## 2. Traces (per-query)

The v6 spec calls for per-query traces with stage timings. The daemon implements this via tracing spans plus an in-memory ring buffer.

A `Memory::Search` produces:

```
search_request{request_id=42, user_id=default}
├── query_embed{provider=local, model=bge-small-en-v1.5, cache_hit=false, dur_ms=8}
├── vector_search{candidates=200, dur_ms=4}
├── lexical_search{candidates=58, dur_ms=1}     [if hybrid]
├── score_fusion{dur_ms=0.4}
├── graph_expansion{enabled=false}
├── rerank{enabled=false}
└── format_response{dur_ms=0.1}
```

The ring buffer stores the last N traces (default 1000). Retrievable via:

```
Diagnostics::Trace { trace_id: "..." }
```

`trace_id` is returned in every `Response` so a client can pair its own logs with daemon traces. Tracing fields included in the response:

```json
{
  "trace": {
    "trace_id": "tr_01H...",
    "request_id": 42,
    "stages": {
      "embed_ms": 8.2,
      "vector_ms": 4.1,
      "lexical_ms": 1.3,
      "fusion_ms": 0.4,
      "rerank_ms": null,
      "format_ms": 0.1
    },
    "tokens": { "embed_input": 12, "rerank_input": null, "rerank_output": null }
  }
}
```

Phase 11 wires this end to end. Earlier phases populate `trace_id` and the basic stages.

## 3. Doctor

`cm doctor` runs a battery of checks and prints a structured report (default human, `--json` for machine output).

```
cognitive-memory doctor (cm-daemon 0.1.0, protocol v1)

  socket reachable           ok       /Users/.../cm.sock
  database writable          ok       /Users/.../data.db (8.2 MB)
  embedding model loaded     ok       bge-small-en-v1.5 (192 MB resident)
  openai provider            warn     OPENAI_API_KEY not set; extraction will fail
  anthropic provider         ok       configured, last call 12s ago, 200
  time skew vs ntp           ok       +3 ms
  disk free                  ok       147 GB free
  daemon log size            ok       4.2 MB / 100 MB cap
  http bridge                skip     not running
```

Exit codes: `0` if all `ok`/`skip`, `1` if any `warn`, `2` if any `error`.

Phase 11 implements the full battery; earlier phases ship subset checks.

## 4. Metrics (Phase 11)

Two delivery options under consideration:

- **`Diagnostics::Metrics`** — request returns a snapshot (counters, histograms). Best for the always-Unix-socket use case.
- **Prometheus endpoint on the HTTP bridge** — feature-flagged. Useful if you want to scrape into Grafana.

Counters and histograms include: requests by bucket+op (count, latency p50/p95/p99), embedding cache hit rate, LLM call rate by provider, store query plan stats, background-task run counts and durations.

## 5. Tracing for development

In foreground mode:

```sh
RUST_LOG=cognitive_memory=debug,cm_daemon=trace cm-daemon --foreground
```

The crate prefixes `cognitive_memory_*` and `cm_*` are filterable independently. Set `cognitive_memory_store=trace` to get SQL-statement-level logs (only useful when chasing a store bug).

## 6. What gets logged where: cheat sheet

| Concern | Where |
| --- | --- |
| Daemon startup, socket bind, shutdown | `daemon.log`, INFO |
| Per-request dispatch start/end | `daemon.log`, INFO with `request_id` |
| Per-stage timings inside a request | DEBUG (default off); always in trace ring buffer |
| LLM call: provider, model, tokens, latency, status | INFO |
| LLM call: prompt or completion content | TRACE only (default off) |
| Embedding cache hit/miss | DEBUG |
| Background task tick start/end | INFO |
| HTTP bridge requests | `http.log`, INFO |
| HTTP bridge `Authorization` header | never |
