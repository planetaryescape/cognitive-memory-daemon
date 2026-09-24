# IPC Protocol

This document specifies the wire protocol between `cm-daemon` and its clients. It is the public contract; everything else is implementation. Clients in any language can implement against this spec.

Version: `IPC_PROTOCOL_VERSION = 1`. This document and the daemon binary advance the version in lockstep.

## 1. Transport

- **Type**: Unix domain socket, `SOCK_STREAM`.
- **Path resolution** (in order): `COGNITIVE_MEMORY_SOCKET_PATH` env var, then the active runtime identity (`COGNITIVE_MEMORY_INSTANCE`; release default `cognitive-memory`, debug default `cognitive-memory-dev`) under the platform runtime directory.
- **Permissions**: socket is mode 0700; parent directory is mode 0700. Owner-only.
- **Encoding**: UTF-8 JSON.
- **Framing**: 4-byte big-endian length prefix followed by JSON. Maximum frame: 16,777,216 bytes (16 MiB).

A frame on the wire:

```
+----+----+----+----+--------- ... ---------+
| length (u32 BE)   | UTF-8 JSON payload    |
+----+----+----+----+--------- ... ---------+
```

## 2. Connection setup

After `connect()`:

1. The client sends one frame containing a `Hello`:
   ```json
   { "kind": "Hello", "client": "cm-cli/0.1.0", "protocol_version": 1, "user_id": "default" }
   ```
2. The daemon responds with a `Welcome` or rejects with `Error { kind: "ProtocolMismatch", ... }`:
   ```json
   { "kind": "Welcome", "daemon_version": "0.1.0", "protocol_version": 1, "session_id": "01H..." }
   ```
3. From this point, both sides exchange `IpcMessage` frames.

`Hello` and `Welcome` are bare JSON objects without the `IpcMessage` envelope, so connection setup is decoupled from request/response framing.

`user_id` is mandatory in `Hello`. Every subsequent request is implicitly scoped to it. To act as a different user, open a new connection.

## 3. Message envelope

```json
{
  "id": 42,
  "payload": { ... }
}
```

- **`id`** is a `u64`. Clients allocate `id` monotonically per connection, starting at 1. Events sent from daemon to client use `id = 0`.
- **`payload`** is one of `Request`, `Response`, `Event`. The variant is encoded as `{ "kind": "Request", "body": { ... } }` to keep the JSON discriminator-tagged and forward-compatible.

The `Response` to a `Request` echoes the request's `id`. Multiple in-flight requests on one connection are allowed; responses may arrive out of order and are correlated by `id`.

## 4. Buckets

Every `Request` sits in exactly one bucket:

- **`Memory`** — store, batch store, CRUD/listing, search, links, lexical/vector search, batch updates, and subscriptions to memory events.
- **`Lifecycle`** — explicit maintenance operations: tick, consolidation candidates, tier migration, retention updates, and clear.
- **`Diagnostics`** — health, status, traces, shutdown, bridge tokens.

Client-specific UI state intentionally does not cross the wire.

Adding a request type: see `docs/developer/adding-a-request.md`.

## 5. Request catalogue (v1)

Each entry below shows the request payload, response payload on success, and possible error kinds.

### 5.1 Memory bucket

#### `Memory::Store`

Store one memory.

Request:
```json
{
  "bucket": "Memory",
  "op": "Store",
  "user_id": "default",
  "content": "User dislikes brittle integration tests with mocked databases.",
  "category": "semantic",
  "memory_type": "preference",
  "metadata": "{\"project\":\"lazydap\"}",
  "importance": 0.7
}
```

Response:
```json
{ "ok": true, "data": { "kind": "MemoryStored", "id": "mem_01H..." } }
```

Errors: `InvalidPayload`, `ProviderError`, `StorageError`.

#### `Memory::Search`

```json
{
  "bucket": "Memory",
  "op": "Search",
  "query": "How does the user feel about mocks?",
  "user_id": "default",
  "limit": 10,
  "deep_recall": false,
  "hybrid": true,
  "graph_expansion_hops": 1,
  "bridge_discovery": false
}
```

Response:
```json
{
  "ok": true,
  "data": {
    "kind": "MemorySearchResults",
    "results": [ { "memory_id": "mem_01H...", "content": "...", "category": "semantic", "memory_type": "fact", "score": 0.71 } ],
    "bridge_paths": []
  }
}
```

Errors: `InvalidQuery`, `ProviderError`, `StorageError`.

#### `Memory::Get`, `Memory::GetMany`, `Memory::Update`, `Memory::Delete`, `Memory::DeleteMany`, `Memory::List`

Standard CRUD and listing. Full schemas live in `crates/protocol/src/lib.rs` and are covered by golden fixtures under `crates/protocol/tests/fixtures/`.

#### `Memory::StoreBatch`

Store many pre-extracted memories in one call. Memories created together get bidirectional associations by default.

```json
{ "bucket": "Memory", "op": "StoreBatch", "user_id": "default", "memories": [ { "content": "User prefers SQLite for local tools.", "category": "semantic", "memory_type": "preference", "metadata": "{}" } ], "initial_link_weight": 0.5 }
```

#### `Memory::Link`

Create or update an association between two memories.

```json
{ "bucket": "Memory", "op": "Link", "user_id": "default", "source_id": "mem_a", "target_id": "mem_b", "strength": 0.5, "bidirectional": true, "kind": "explicit" }
```

Related link operations are `Unlink`, `GetLinked`, and `GetLinkedMany`.

#### `Memory::VectorSearch`, `Memory::SearchLexical`, `Memory::BatchUpdate`

Specialized adapter-parity operations: raw-vector search for clients that already embedded the query, BM25-only lexical search, and batch retention-floor updates.

#### `Memory::Subscribe`

Subscribe to memory events on this connection.

```json
{ "bucket": "Memory", "op": "Subscribe", "replay_snapshot": true }
```

Response is `{ "ok": true, "data": { "kind": "Subscribed", "replay_snapshot_sent": true } }`. From this point, the daemon may push `Event` messages on the connection (see §6).

### 5.2 Lifecycle bucket

#### `Lifecycle::Tick`

```json
{ "bucket": "Lifecycle", "op": "Tick", "synchronous": false }
```

Async by default; returns immediately. With `synchronous: true`, response carries a summary of work performed.

Other lifecycle requests are `FindFading`, `FindStable`, `MarkSuperseded`, `MigrateToCold`, `MigrateToHot`, `ConvertToStub`, `UpdateRetention`, and `Clear`.

### 5.3 Diagnostics bucket

#### `Diagnostics::Status`

```json
{ "bucket": "Diagnostics", "op": "Status" }
```

Response:
```json
{
  "ok": true,
  "data": {
    "daemon_version": "0.1.0",
    "protocol_version": 1,
    "build_id": "0.1.0:/path/to/cm-daemon:...",
    "instance": "cognitive-memory",
    "daemon_pid": 12345,
    "socket_path": ".../cm.sock",
    "pid_path": ".../cm-daemon.pid",
    "db_path": ".../data.db",
    "log_path": ".../daemon.log",
    "uptime_seconds": 12345,
    "memory_count": 4321
  }
}
```

#### `Diagnostics::RecentTraces`

Fetch recent entries from the bounded request trace ring.

#### `Diagnostics::Doctor`

Run health checks; return structured report.

#### `Diagnostics::Shutdown`

Ask the daemon to shut down gracefully.

#### `Diagnostics::MintBridgeToken`

Issue a bearer token for `cm-http` use.

```json
{ "bucket": "Diagnostics", "op": "MintBridgeToken", "user_id": "default", "scope": "write", "ttl_seconds": 2592000 }
```

Response: `{ "ok": true, "data": { "token": "cmb_..." } }`.

#### `Diagnostics::ValidateBridgeToken`

Validate a raw bridge token against daemon-owned token hashes. Used by `cm-http`.

## 6. Events

After `Memory::Subscribe`, the daemon pushes `Event` messages on the same connection.

```json
{
  "id": 0,
  "payload": {
    "kind": "Event",
    "body": {
      "kind": "CurrentState",
      "memory_count": 12,
      "occurred_at": "2026-05-07T12:34:56Z"
    }
  }
}
```

Event kinds in v1: `CurrentState`, `TickCompleted`, `EventStreamLagged`.

Events are best-effort. A subscribed client missing events while disconnected does not get replay. Clients that need durable replay should poll current state after reconnect; durable event replay is not in the v1 protocol surface.

## 7. Errors

Every error response has a typed kind for programmatic handling:

```json
{
  "ok": false,
  "error": {
    "kind": "ProviderError",
    "message": "OpenAI returned 429 after 3 retries",
    "retriable": true,
    "details": { "provider": "OpenAI", "status": 429 }
  }
}
```

Error kinds (initial set):
- `ProtocolMismatch` — version mismatch at `Hello`.
- `InvalidPayload` — malformed JSON or schema mismatch.
- `InvalidQuery` — request semantically invalid (e.g., unknown filter).
- `NotFound` — id does not resolve.
- `Conflict` — write conflict (e.g., duplicate id under user).
- `ProviderError` — LLM or embedding provider failure.
- `NoLlmConfigured` — extraction requested but no provider configured.
- `StorageError` — SQLite-level failure.
- `RateLimited` — daemon-internal rate limit hit.
- `ShuttingDown` — daemon is mid-shutdown.
- `Internal` — bug.

Clients distinguish retriable from non-retriable via the `retriable` flag.

## 8. Versioning

- A new request type, response field, or event kind is **additive** and does not bump `IPC_PROTOCOL_VERSION`. Clients ignore unknown fields.
- A breaking change (renaming a field, changing a type, removing a request) bumps `IPC_PROTOCOL_VERSION` and is gated by daemon and SDK release coordination.
- Clients send their compiled-in `protocol_version` in `Hello`. Daemon compares; on mismatch, refuse the connection with `ProtocolMismatch` and a clear message.

## 9. Reference encoding

The Rust definitions in `crates/protocol/` are the source of truth. The TS and Python SDK `RemoteAdapter`s mirror them by hand for now; codegen from a JSON Schema is a follow-up.

## 10. Concurrency

Each accepted connection runs in its own tokio task. Inside that connection, individual requests run as child tasks, so one slow bulk request does not block a later hot request or subscribed events on the same socket. Responses may arrive out of order and are correlated by `id`.

Hot requests use `REQUEST_CONCURRENCY_LIMIT = 64`; bulk lifecycle/diagnostic/batch operations use `BULK_CONCURRENCY_LIMIT = 8`.

Lifecycle maintenance is explicit in this release: clients call `Lifecycle::Tick` through `cm tick` or IPC. Periodic scheduling remains an additive future surface.

## 11. Test fixtures

Golden fixtures live under `crates/protocol/tests/fixtures/`. Client implementations can replay them against the daemon or a fake for protocol-conformance tests.
