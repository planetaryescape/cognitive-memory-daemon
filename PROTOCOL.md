# IPC Protocol

This document specifies the wire protocol between `cm-daemon` and its clients. It is the public contract; everything else is implementation. Clients in any language can implement against this spec.

Version: `IPC_PROTOCOL_VERSION = 1`. This document and the daemon binary advance the version in lockstep.

## 1. Transport

- **Type**: Unix domain socket, `SOCK_STREAM`.
- **Path resolution** (in order): `COGNITIVE_MEMORY_SOCKET_PATH` env var, `~/Library/Application Support/cognitive-memory/cm.sock` (macOS), `$XDG_RUNTIME_DIR/cognitive-memory/cm.sock` (Linux).
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
- **`payload`** is one of `Request`, `Response`, `Event`. The variant is encoded as `{ "kind": "Request", "request": { ... } }` to keep the JSON discriminator-tagged and forward-compatible.

The `Response` to a `Request` echoes the request's `id`. Multiple in-flight requests on one connection are allowed; responses may arrive out of order and are correlated by `id`.

## 4. Buckets

Every `Request` sits in exactly one bucket:

- **`Memory`** — CRUD on memories, search, ingest, extraction, subscriptions to memory events.
- **`Lifecycle`** — decay, consolidation, expiry, promotion, scheduled tick.
- **`Diagnostics`** — health, status, traces, version, log access, bridge tokens.
- **`ClientSpecific`** — UI state, never crosses the wire (mentioned for completeness).

Adding a request type: see `docs/developer/adding-a-request.md`.

## 5. Request catalogue (v1)

Each entry below shows the request payload, response payload on success, and possible error kinds.

### 5.1 Memory bucket

#### `Memory::Store`

Store one or more memories.

Request:
```json
{
  "bucket": "Memory",
  "op": "Store",
  "memories": [
    {
      "content": "User dislikes brittle integration tests with mocked databases.",
      "category": "semantic",
      "memory_type": "preference",
      "metadata": { "project": "lazydap", "source_agent": "claude-code" },
      "valid_from": "2026-05-07T10:00:00Z",
      "ttl_seconds": null
    }
  ],
  "embedding_override": null,
  "llm_override": null
}
```

Response:
```json
{ "ok": true, "data": { "stored": [{ "id": "mem_01H..." }] } }
```

Errors: `InvalidPayload`, `ProviderError`, `StorageError`.

#### `Memory::Search`

```json
{
  "bucket": "Memory",
  "op": "Search",
  "query": "How does the user feel about mocks?",
  "filters": {
    "memory_types": ["preference", "fact"],
    "categories": null,
    "metadata": { "project": "lazydap" }
  },
  "limit": 10,
  "deep_recall": false,
  "include_expired_transients": false,
  "hybrid": true,
  "graph_expansion": { "enabled": false, "hops": 1 },
  "rerank": false,
  "embedding_override": null
}
```

Response:
```json
{
  "ok": true,
  "data": {
    "results": [ { "memory": {...}, "score": 0.71, "retention": 0.93, "trace_id": "..." } ],
    "trace": { "stages": { "dense_ms": 4.2, "sparse_ms": 1.3, "score_ms": 0.4, "rerank_ms": null } }
  }
}
```

Errors: `InvalidQuery`, `ProviderError`, `StorageError`.

#### `Memory::Get`, `Memory::Update`, `Memory::Delete`, `Memory::List`

Standard CRUD. Full schemas in `crates/protocol/src/memory.rs` once Phase 0 lands.

#### `Memory::Link`

Create or update an association between two memories.

```json
{ "bucket": "Memory", "op": "Link", "source_id": "mem_a", "target_id": "mem_b", "weight": 0.5, "kind": "explicit" }
```

#### `Memory::Ingest`

Ingest a turn or message and let the daemon decide whether to store, update, or merge.

```json
{ "bucket": "Memory", "op": "Ingest", "turn": { "role": "user", "content": "..." }, "context": [...], "llm_override": null }
```

#### `Memory::ExtractAndStore`

Run LLM extraction over a transcript and store extracted memories.

```json
{ "bucket": "Memory", "op": "ExtractAndStore", "transcript": [...], "llm_override": null, "embedding_override": null }
```

#### `Memory::Subscribe`

Subscribe to memory events on this connection.

```json
{ "bucket": "Memory", "op": "Subscribe", "kinds": ["Stored", "Updated", "Deleted", "Expired"] }
```

Response is `{ "ok": true, "data": { "subscribed": [...] } }`. From this point, the daemon may push `Event` messages on the connection (see §6).

#### `Memory::Unsubscribe`

Symmetric.

### 5.2 Lifecycle bucket

#### `Lifecycle::Tick`

```json
{ "bucket": "Lifecycle", "op": "Tick", "synchronous": false }
```

Async by default; returns immediately. With `synchronous: true`, response carries a summary of work performed.

#### `Lifecycle::Consolidate`

Trigger consolidation candidate scan, optionally limited.

#### `Lifecycle::Expire`

Hard-filter or remove expired transients. By default, expiry is logical (hidden from default retrieval); `Expire { mode: "purge" }` removes from storage.

#### `Lifecycle::PromoteToCore`

Manually promote a memory.

#### `Lifecycle::DecayStats`

Read-only retention distribution snapshot.

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
    "uptime_seconds": 12345,
    "memory_count": 4321,
    "embedding_model": "bge-small-en-v1.5",
    "providers": [{ "kind": "OpenAI", "configured": true }],
    "background_tasks": [{ "name": "tick_scheduler", "last_run": "..." }]
  }
}
```

#### `Diagnostics::Trace`

Fetch a per-query trace by `trace_id`.

#### `Diagnostics::Doctor`

Run health checks; return structured report.

#### `Diagnostics::Version`

Return daemon and protocol versions.

#### `Diagnostics::Logs`

Tail recent log lines.

#### `Diagnostics::MintBridgeToken`

Issue a bearer token for `cm-http` use.

```json
{ "bucket": "Diagnostics", "op": "MintBridgeToken", "scopes": ["read", "write"], "ttl_seconds": 2592000 }
```

Response: `{ "ok": true, "data": { "token": "cmb_..." } }`.

## 6. Events

After `Memory::Subscribe`, the daemon pushes `Event` messages on the same connection.

```json
{
  "id": 0,
  "payload": {
    "kind": "Event",
    "event": {
      "kind": "MemoryStored",
      "memory_id": "mem_01H...",
      "user_id": "default",
      "metadata": { "project": "lazydap", "source_agent": "claude-code" },
      "occurred_at": "2026-05-07T12:34:56Z"
    }
  }
}
```

Event kinds in v1: `MemoryStored`, `MemoryUpdated`, `MemoryDeleted`, `MemoryExpired`, `TickCompleted`, `ConsolidationCompleted`, `ProviderRateLimited`.

Events are best-effort. A subscribed client missing events while disconnected does not get replay. Clients that need durable replay should poll `Memory::List` with `since` after reconnect, or pull from `events` table via `Diagnostics::ReplayEvents` (Phase 11).

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

## 10. Test fixtures

Phase 0 ships golden fixtures: a directory of `.json` request/response pairs that any client implementation can replay against the daemon (or a fake) for protocol-conformance tests. Location: `crates/protocol/tests/fixtures/`.
