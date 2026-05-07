# Troubleshooting

Common failure modes and the first diagnostic step. When in doubt: `cm doctor`, then check the log.

## Daemon won't start

### `cm` reports "daemon failed to come up"

```
$ cm status
error: daemon socket not reachable; daemon failed to come up
       see ~/Library/Logs/cognitive-memory/daemon.log
```

Diagnostic order:

1. **Tail the log.** `tail -50 ~/Library/Logs/cognitive-memory/daemon.log`. The crash reason is almost always there.
2. **Check the PID file.** `cat ~/Library/Application\ Support/cognitive-memory/cm.pid`. If a PID exists, the daemon signal-probes it on startup (sends `SIGZERO` via `nix::sys::signal::kill`); a live PID blocks a new start, a stale PID is reclaimed automatically. Manual cleanup (`rm cm.pid`) should not normally be needed; if you find it is, file a bug — the signal-probe path failed.
3. **Check for port-equivalent collisions.** Another instance bound to the same socket path. `lsof ~/Library/Application\ Support/cognitive-memory/cm.sock` shows the holder.
4. **Run in foreground.** `cm-daemon --foreground` shows tracing on stderr in real time.

### `address already in use` on socket

A previous daemon crashed without removing the socket file.

```sh
rm ~/Library/Application\ Support/cognitive-memory/cm.sock
```

The new daemon will rebind. Permission errors here mean the file is owned by a different user; that's a deeper config issue.

### Model download stalls

Model is `bge-small-en-v1.5` from Hugging Face. First run downloads to `~/Library/Application Support/cognitive-memory/models/`. If the download stalls:

1. Check connectivity to `huggingface.co`.
2. Manually pre-download and place the ONNX file in the models dir; `fastembed-rs` picks it up.
3. If you're offline by design, configure a hosted embedding provider as default in `config.toml`.

## Requests fail

### `ProtocolMismatch` error

Daemon and client disagree on `IPC_PROTOCOL_VERSION`. Upgrade the lagging side.

```sh
cm --version          # client (CLI) protocol version
cm status             # daemon version + protocol if reachable
```

If the SDK is the lagging side: pin the SDK and daemon together to the same protocol version.

### `NoLlmConfigured` on `Memory::ExtractAndStore`

No LLM key resolves. Check precedence in [`configuration.md`](./configuration.md):

```sh
echo $OPENAI_API_KEY
echo $ANTHROPIC_API_KEY
cm doctor            # the providers section is the canonical answer
```

Either set the env var and restart the daemon, or attach `llm_override.api_key` per request.

### `ProviderError` with `retriable: true`

The provider returned a transient error (429, 5xx, network). The client should retry with backoff. The daemon already retries up to a small budget internally; reaching the client means the budget was exhausted.

Look at the error `details.status` and `details.provider` to identify whether you need to throttle, switch model, or check provider status pages.

### Searches return nothing

Likely causes, in order:

1. **Wrong `user_id`.** `Memory::Search` only sees memories under the connection's `user_id`. The CLI defaults to `default`; the SDK requires it explicit. Confirm by `cm list --user default | head` (or whatever `user_id` you stored under).
2. **Validity filter.** Memories with `valid_until` in the past are hidden by default. Add `deep_recall: true` or `include_expired_transients: true`.
3. **Embedding provider mismatch.** A search with `embedding_override` against an OpenAI model only finds memories whose embeddings exist under that `(provider, model)`. Phase-3-and-later behaviour around fallback/re-embedding is described in `docs/concepts/embedding-strategy.md` §4.
4. **Empty store.** `cm doctor` shows `memory_count`. Zero is zero.

## Performance feels off

### Search > 200 ms

Hot daemon should be sub-100 ms. Slowness causes:

1. **Cold model.** First call after restart pays load cost. Subsequent calls are fast.
2. **Cache miss embedding.** Search query was novel. Cache warm-up will help repeat queries.
3. **Hybrid + rerank both on.** Disable rerank to confirm; rerank dominates if the LLM provider is slow.
4. **Large candidate set.** Default `limit=10` and the daemon's pre-filter usually keep candidate sets small. Custom `limit > 200` plus graph expansion can blow this up.

`cm search "..." --trace` returns the per-stage timings (Phase 11+). Read the slowest stage; that's the answer.

### Daemon RAM growing without bound

Expected steady-state: ~200–300 MB after model load and a few hundred memories. If RAM grows much beyond this:

- Check `embeddings::cache_pruner` is running (`cm doctor` background tasks section).
- Check trace ring buffer size; default 1000 is fine, larger custom values eat RAM.
- File a bug with `tracing` heap profile attached.

## State is corrupted

### `StorageError: SQLITE_CORRUPT`

Rare but possible after a hard crash on a filesystem with caching pathologies. Recovery:

1. Stop the daemon.
2. `sqlite3 ~/Library/Application\ Support/cognitive-memory/data.db "PRAGMA integrity_check"`.
3. If the report is anything other than `ok`, dump and reload:
   ```sh
   sqlite3 data.db ".dump" > dump.sql
   mv data.db data.db.broken
   sqlite3 data.db < dump.sql
   ```
4. Restart the daemon.

Always keep `data.db.broken` until you've verified post-recovery state.

### Memories appear duplicated

The daemon does not enforce content-level dedup; identical content from two stores creates two memories. Use `Memory::Ingest` instead of raw `Store` for dedup-aware writes. Phase 10 wires this; until then, dedup is the client's responsibility if it matters.

## HTTP bridge

### 401 Unauthorized

Token invalid, expired, or never minted. Mint a new one via `Diagnostics::MintBridgeToken` (over the Unix socket).

### Bridge refuses to start

`COGNITIVE_MEMORY_HTTP_BIND` resolves to a non-loopback address. The bridge refuses by design (`SECURITY.md` §2 T5). Use `127.0.0.1:7472` or `localhost:7472`.

## When to file a bug

If `cm doctor` shows no warnings, the log shows no errors, and behaviour still surprises you, file a bug. Include:

- Daemon version (`cm --version`).
- Full doctor output (`cm doctor --json`).
- Reproduction steps as a sequence of `cm` invocations or SDK calls.
- Last 100 lines of `daemon.log`, redacted.
