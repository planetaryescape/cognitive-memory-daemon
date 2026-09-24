# HTTP bridge (`cm-http`)

A separate binary that translates loopback HTTP into Unix-socket IPC, for clients that can't speak Unix sockets directly (browsers, some language runtimes, REST-based tools).

## 1. Why a separate binary

The daemon binds Unix sockets only — that property is load-bearing for the security model (see `SECURITY.md` §2 T4). Adding HTTP support inside the daemon would compromise the "no network code path in the daemon" invariant.

`cm-http` is a thin proxy: bind `127.0.0.1`, validate auth, forward to the Unix socket. Crashing the bridge does not affect the daemon. Compromising the bridge process does not give an attacker access beyond what the bridge's bearer tokens allow.

## 2. Topology

```
Browser / REST client
        |
        | HTTP/JSON over 127.0.0.1:7472, Bearer token
        v
+-------------------+
|     cm-http       |
| - validate token  |
| - translate path  |
|   to Request enum |
+--------+----------+
         |
         | Unix socket, length-delimited JSON
         v
+--------+----------+
|     cm-daemon     |
+-------------------+
```

Per HTTP request:

1. Validate Host header and `Authorization: Bearer <token>`.
2. Accept the token from the bridge's local bootstrap map or validate it against daemon-owned token hashes.
3. Map URL path + method to a `Request` payload.
4. Send the `Request` on a daemon connection scoped to the token's `user_id`.
5. Await the matching `Response`.
6. Translate the `Response` to an HTTP body and status.

## 3. Bind and discoverability

- Default bind: `127.0.0.1:7472`. Configurable via `COGNITIVE_MEMORY_HTTP_BIND`.
- Will not bind on a non-loopback address. If `COGNITIVE_MEMORY_HTTP_BIND` resolves to a non-loopback address, the bridge logs an error and exits.
- The bridge is opt-in: `cm-daemon` does not auto-spawn it. Run `cm-http` as a launchd / systemd unit, a `brew services` service, or a manual background process.

## 4. Authentication: localhost + per-request bearer

### 4.1 Token mint

Preferred path: ask the daemon over the Unix socket to mint a token:

```json
{
  "id": 1,
  "payload": {
    "kind": "Request",
    "body": { "bucket": "Diagnostics", "op": "MintBridgeToken", "user_id": "default", "scope": "write", "ttl_seconds": 2592000 }
  }
}
```

Response:
```json
{ "ok": true, "data": { "kind": "BridgeToken", "token": "cmb_<64-hex-chars>", "expires_at_unix": 1893456000 } }
```

`cm-http` can also mint and register a token at startup:

```sh
COGNITIVE_MEMORY_HTTP_MINT_USER=default \
COGNITIVE_MEMORY_HTTP_MINT_SCOPE=write \
cm-http
```

The startup-minted token is written to the private bridge token file reported by the active runtime identity. It is not written to logs.

For tests and local scripts, bootstrap a token directly from env:

```sh
COGNITIVE_MEMORY_HTTP_BOOTSTRAP_TOKEN=dev-token \
COGNITIVE_MEMORY_HTTP_BOOTSTRAP_USER=default \
COGNITIVE_MEMORY_HTTP_BOOTSTRAP_SCOPE=write \
cm-http
```

The token has at least 192 bits of entropy when minted by the daemon. The daemon stores only a salted SHA-256 of the token in `kv`. Lost tokens are revoked and reminted, not recovered.

### 4.2 Token use

Every HTTP request to the bridge requires:

```
Authorization: Bearer cmb_<token>
```

The bridge:
1. Checks the Host header against loopback defaults or `COGNITIVE_MEMORY_HTTP_ALLOWED_HOSTS`.
2. Checks its in-memory bootstrap token map.
3. If not found locally, asks the daemon to validate the token via `Diagnostics::ValidateBridgeToken`.
4. Confirms the token has the required scope for the request.
5. Opens a daemon connection using the token's `user_id` and forwards the request.

### 4.3 Token scopes

- `read`: can call read routes such as `/memory/search`.
- `write`: can call read routes and write routes such as `/memory/store`.
- `admin`: accepted by the token model; no admin-only HTTP routes are exposed yet.

Scope set on mint; not changeable after.

### 4.4 What the bridge does not do

- The bridge does not implement OAuth, sessions, or cookies. Bearer tokens only.
- The bridge does not stream events (no SSE, no WebSocket) in v1. Event subscription is a Unix-socket-only feature in v1.
- The bridge does not cache requests or responses.
- The bridge does not multiplex unrelated clients onto a single daemon connection unless they share `user_id` — different `user_id`s get different daemon connections.

## 5. URL surface

The current bridge intentionally exposes a small surface:

| Method + path | Request |
| --- | --- |
| `POST /memory/store` | `Memory::Store` |
| `POST /memory/search` | `Memory::Search` |

Bodies are the request payloads from `PROTOCOL.md`. Response status codes:
- 200: `Response { ok: true, ... }`.
- 400: `InvalidPayload`, `InvalidQuery`.
- 401: missing/invalid/expired token.
- 403: scope insufficient.
- 404: `NotFound`.
- 409: `Conflict`.
- 429: `RateLimited`.
- 500: `Internal`, `StorageError`, `ProviderError` (with `retriable=false`).
- 503: `ShuttingDown`, `ProviderError` (with `retriable=true`).

Response body is the `Response` envelope as JSON.

## 6. Logging

The bridge logs startup configuration and rejected unknown bearer tokens. It does **not** log full tokens or request bodies. Per-request access logging is a later observability pass.

## 7. CORS

The bridge installs a CORS layer with an explicit origin allowlist. Configure it with `COGNITIVE_MEMORY_HTTP_ALLOWED_ORIGINS` or `COGNITIVE_MEMORY_HTTP_CORS_ORIGINS` as a comma-separated list. There is no wildcard origin.
