# Recipe: adding a new request type

This recipe is the canonical path for extending the protocol. Following it keeps the wire format, handler dispatch, SDK clients, and tests in sync.

Worked example: adding `Memory::Pin` (mark a memory as pinned, raising its retention floor).

## 1. Decide the bucket

Pinning a memory affects retention — that's `Memory`-level state, not `Lifecycle` policy. → `Memory` bucket.

Unsure? `PROTOCOL.md` §4 lists what each bucket is for. When still unsure, default to `Memory`; rebucket later if the surface clarifies.

## 2. Spec it in `PROTOCOL.md`

Add an entry under §5.1 with the request payload, response payload, and possible error kinds. Worked example:

```
#### `Memory::Pin`

Pin a memory to raise its retention floor.

Request:
```json
{ "bucket": "Memory", "op": "Pin", "memory_id": "mem_01H...", "floor": 0.8 }
```

Response:
```json
{ "ok": true, "data": { "previous_floor": 0.0 } }
```

Errors: `NotFound`, `InvalidPayload`, `StorageError`.
```

## 3. Add the variant to the protocol enum

In `crates/protocol/src/memory.rs`:

```rust
pub enum MemoryRequest {
    // ... existing
    Pin { memory_id: MemoryId, floor: f32 },
}

pub enum MemoryResponseData {
    // ... existing
    Pinned { previous_floor: f32 },
}
```

`serde` derives are already on these enums; the variant gets serialised under `op: "Pin"` automatically by the existing tag config.

## 4. Add a fixture

`crates/protocol/tests/fixtures/memory_pin_request.json`:

```json
{
  "id": 1,
  "payload": {
    "kind": "Request",
    "request": { "bucket": "Memory", "op": "Pin", "memory_id": "mem_01H1234567890", "floor": 0.8 }
  }
}
```

`crates/protocol/tests/fixtures/memory_pin_response.json`:

```json
{
  "id": 1,
  "payload": { "kind": "Response", "response": { "ok": true, "data": { "kind": "Pinned", "previous_floor": 0.0 } } }
}
```

The round-trip test in `crates/protocol/tests/round_trip.rs` picks these up automatically.

## 5. Implement the handler

In `crates/daemon/src/handler/memory.rs`:

```rust
async fn handle_pin(state: &AppState, memory_id: MemoryId, floor: f32) -> Result<MemoryResponseData, Error> {
    if !(0.0..=1.0).contains(&floor) {
        return Err(Error::InvalidPayload("floor must be in [0.0, 1.0]".into()));
    }
    let previous = state.store.memory_repo().pin(memory_id, floor).await?;
    state.events.publish(Event::MemoryUpdated { memory_id, fields: vec!["retention_floor".into()] });
    Ok(MemoryResponseData::Pinned { previous_floor: previous })
}
```

Wire it into the dispatcher:

```rust
match request {
    MemoryRequest::Pin { memory_id, floor } => handle_pin(state, memory_id, floor).await,
    // ...
}
```

## 6. Implement the store method

In `crates/store/src/memory_repo.rs`:

```rust
impl MemoryRepo {
    pub async fn pin(&self, id: MemoryId, floor: f32) -> Result<f32, StoreError> {
        // returns previous floor
    }
}
```

Use the writer pool (mutating). Errors typed via `StoreError` and `?`-propagated.

## 7. Test

- **Unit** test the handler with a fake `MemoryRepo` (errors-only path that doesn't need the trait — handler is small enough that an integration test covers it adequately).
- **Integration** test the repo against a real SQLite (`crates/store/tests/pin_memory.rs`).
- **End-to-end** test in `crates/daemon/tests/e2e_pin.rs`: store a memory, pin it with floor 0.8, search and confirm `retention_floor == 0.8` in the result.

## 8. Update the CLI (if exposing)

If pinning should be reachable from `cm`:

```sh
cm pin <memory_id> --floor 0.8
```

In `crates/cli/src/commands/pin.rs`:

```rust
pub async fn run(args: PinArgs) -> Result<()> {
    let client = client::connect_or_spawn().await?;
    let resp = client.memory_pin(args.memory_id, args.floor).await?;
    println!("pinned: previous floor was {}", resp.previous_floor);
    Ok(())
}
```

Plus a clap subcommand registration. Update `cm --help` snapshot test.

## 9. Update SDK clients

In TS SDK `RemoteAdapter` (Phase 6+):

```ts
async pin(memoryId: string, floor: number): Promise<{ previousFloor: number }> {
  const resp = await this.client.send({
    bucket: "Memory",
    op: "Pin",
    memory_id: memoryId,
    floor,
  });
  return { previousFloor: resp.data.previous_floor };
}
```

Same shape in Python SDK `RemoteAdapter`. The two SDKs mirror by hand for now; fixture-driven codegen is post-v1 backlog.

## 10. Update docs

If the new request changes a documented surface (it does — `cm` gains a subcommand, `Memory::*` table grows):

- Append the new entry to `PROTOCOL.md` (already done in step 2).
- Add the CLI command to `README.md` quick-start if it's a primary action.
- If the change makes a non-obvious decision (e.g., default floor value, behavior on already-pinned), write a short ADR.

## 11. PR checklist

- [ ] `PROTOCOL.md` updated.
- [ ] Variant added to the protocol enum.
- [ ] Fixtures added; round-trip tests pass.
- [ ] Handler implemented.
- [ ] Store method implemented.
- [ ] Unit / integration / E2E tests added.
- [ ] CLI subcommand (if exposing).
- [ ] SDK `RemoteAdapter` methods (TS and Python).
- [ ] ADR (if a non-obvious decision).
- [ ] `cargo fmt`, `cargo clippy`, `cargo test`, `cargo deny check` all green locally.

That's the recipe. Following it keeps the protocol cohesive and the SDKs in lockstep with the daemon.
