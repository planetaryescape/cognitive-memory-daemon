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

The bridge holds one (or a small pool of) connections to the daemon. Per HTTP request:

1. Validate `Authorization: Bearer <token>` against tokens minted by the daemon.
2. Map URL path + method to a `Request` payload.
3. Send the `Request` on a daemon connection.
4. Await the matching `Response`.
5. Translate the `Response` to an HTTP body and status.

## 3. Bind and discoverability

- Default bind: `127.0.0.1:7472`. Configurable via `COGNITIVE_MEMORY_HTTP_BIND`.
- Will not bind on a non-loopback address. If `COGNITIVE_MEMORY_HTTP_BIND` resolves to a non-loopback address, the bridge logs an error and exits.
- The bridge is opt-in: `cm-daemon` does not auto-spawn it. Run `cm-http` as a launchd / systemd unit, a `brew services` service, or a manual background process.

## 4. Authentication: localhost + per-request bearer

### 4.1 Token mint

A client first asks the daemon (over Unix socket — the bridge does not exist yet from its perspective) to mint a token:

```json
{
  "id": 1,
  "payload": {
    "kind": "Request",
    "request": { "bucket": "Diagnostics", "op": "MintBridgeToken", "scopes": ["read", "write"], "ttl_seconds": 2592000 }
  }
}
```

Response:
```json
{ "ok": true, "data": { "token": "cmb_<24-bytes-base64url>" } }
```

The token has at least 192 bits of entropy. It is shown to the user **once**; the daemon stores only a salted SHA-256 of the token in `kv`. Lost tokens are revoked and reminted, not recovered.

### 4.2 Token use

Every HTTP request to the bridge requires:

```
Authorization: Bearer cmb_<token>
```

The bridge:
1. Hashes the token with the per-installation salt.
2. Looks up the hash in `kv` via the daemon's `Diagnostics::ResolveBridgeToken` request (Phase 12 internal request, not in v1 client surface).
3. Confirms the token is unexpired and has the required scope for the request.
4. Forwards the request, attaching the token's `user_id` to the daemon `Hello` (the bridge maintains a separate daemon connection per `user_id`).

### 4.3 Token scopes

- `read`: can call `Memory::Search`, `Memory::Get`, `Memory::List`, `Diagnostics::Status`, `Diagnostics::Trace`.
- `write`: in addition to read, can call `Memory::Store`, `Memory::Update`, `Memory::Delete`, `Memory::Link`, `Memory::Ingest`, `Memory::ExtractAndStore`, `Lifecycle::*` except destructive purge.
- `admin`: in addition to write, can call `Lifecycle::Expire { mode: "purge" }`, `Diagnostics::MintBridgeToken`, `Diagnostics::Logs`.

Scope set on mint; not changeable after.

### 4.4 What the bridge does not do

- The bridge does not implement OAuth, sessions, or cookies. Bearer tokens only.
- The bridge does not stream events (no SSE, no WebSocket) in v1. Event subscription is a Unix-socket-only feature in v1.
- The bridge does not cache requests or responses.
- The bridge does not multiplex unrelated clients onto a single daemon connection unless they share `user_id` — different `user_id`s get different daemon connections.

## 5. URL surface

URL paths mirror the request enum so a competent reader can predict them.

| Method + path | Request |
| --- | --- |
| `POST /memory/store` | `Memory::Store` |
| `POST /memory/search` | `Memory::Search` |
| `GET /memory/:id` | `Memory::Get` |
| `PATCH /memory/:id` | `Memory::Update` |
| `DELETE /memory/:id` | `Memory::Delete` |
| `GET /memory` | `Memory::List` |
| `POST /memory/link` | `Memory::Link` |
| `POST /memory/ingest` | `Memory::Ingest` |
| `POST /memory/extract-and-store` | `Memory::ExtractAndStore` |
| `POST /lifecycle/tick` | `Lifecycle::Tick` |
| `GET /diagnostics/status` | `Diagnostics::Status` |
| `POST /diagnostics/mint-bridge-token` | `Diagnostics::MintBridgeToken` (admin scope only) |

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

The bridge logs each request: timestamp, method, path, response status, latency, scope used, token prefix (first 8 chars). It **never** logs the full token or the request body content.

## 7. CORS

By default, `cm-http` does not set CORS headers. The bridge is for tools running locally; serving a browser app from a different origin requires opt-in via `COGNITIVE_MEMORY_HTTP_CORS_ORIGINS` (comma-separated allow-list). No `*`-wildcard support.
