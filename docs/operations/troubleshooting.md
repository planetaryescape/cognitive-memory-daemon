# Troubleshooting

Start with the simple checks. The daemon is local, so most failures are socket path, process, config, or first-run model load.

## `cm` cannot connect

Run with auto-spawn disabled to separate "daemon missing" from "request failed":

```sh
cm --no-spawn status
```

If that fails, either start the daemon yourself or let `cm` auto-spawn it:

```sh
cm status
cm daemon start
```

For an isolated debug run:

```sh
tmp="$(mktemp -d)"
COGNITIVE_MEMORY_SOCKET_PATH="$tmp/cm.sock" COGNITIVE_MEMORY_LOG=debug cm-daemon --foreground

# another shell
cm --socket "$tmp/cm.sock" --no-spawn status
```

## Socket path confusion

The daemon resolves socket, PID, DB, config, cache, model cache, bridge-token file, and logs from the active runtime identity. If you change only `--socket`, you are changing the socket endpoint; use the `COGNITIVE_MEMORY_*_DIR` overrides for isolated full-state runs.

```sh
COGNITIVE_MEMORY_INSTANCE=my-test cm status
COGNITIVE_MEMORY_SOCKET_PATH=/tmp/cm.sock cm status
```

Use `cm --json status` and `cm counts` to check whether you are looking at the expected store.

## First run is slow

The default daemon build loads `bge-small-en-v1.5` through `fastembed-rs`. First run may download/load the model; later requests reuse the daemon process and cache.

For CI or fast local tests, build without default features so the daemon uses the deterministic fake embedding provider:

```sh
cargo test --workspace --no-default-features
```

## LLM features are disabled

If no `[llm]` section exists in `config.toml`, conflict handling falls back to heuristics and LLM consolidation is skipped.

```sh
cm config-get-llm
cm config-set-llm none
cm config-set-llm openai --api-key-env OPENAI_API_KEY --model gpt-4o-mini
```

Restart the daemon after changing LLM config.

## Searches return nothing

Likely causes:

1. Wrong `user_id`. The CLI defaults to `default`; `--user-id` changes the namespace.
2. Empty store. Check `cm counts`.
3. Cold/superseded/expired filtering. Use deep recall when you intentionally want hidden historical memory.
4. Query/memory mismatch. Use `cm search-lexical` to check whether the text exists lexically.

## HTTP bridge returns 401

The bridge accepts daemon-minted tokens and env-bootstrapped local tokens. Mint a daemon token:

```sh
cm mint-token --scope write
```

or ask the bridge to mint one at startup and write it to its private token file:

```sh
COGNITIVE_MEMORY_HTTP_MINT_USER=default cm-http
```

or bootstrap a dev token:

```sh
COGNITIVE_MEMORY_HTTP_BOOTSTRAP_TOKEN=dev-token cm-http
```

Then call it with:

```sh
curl -H "Authorization: Bearer dev-token" \
  -H "Content-Type: application/json" \
  -d '{"query":"test"}' \
  http://127.0.0.1:7472/memory/search
```

## HTTP bridge refuses to start

`COGNITIVE_MEMORY_HTTP_BIND` must be loopback. Use `127.0.0.1:7472` or another `127.0.0.1:<port>` address.

## HTTP bridge returns 403

Likely causes:

1. Token scope is too small for the route.
2. Host header is not loopback and is not listed in `COGNITIVE_MEMORY_HTTP_ALLOWED_HOSTS`.

## What to include in a bug report

- `cm --version`
- `cm --json status`
- `cm --json counts`
- `cm --json doctor`
- `cm --json trace`
- The exact socket path used
- Reproduction steps
- Relevant daemon or bridge logs from a foreground run with `RUST_LOG=debug`
