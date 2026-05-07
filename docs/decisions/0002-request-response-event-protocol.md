# ADR 0002 — Request / Response / Event protocol

- **Status**: Accepted
- **Date**: 2026-05-07
- **Deciders**: bhekanik

## Context

The IPC layer can be modelled in several ways:

1. Pure request/response (HTTP-style).
2. Bidirectional streaming with no built-in correlation (raw socket).
3. Request/Response with a separate Event channel.
4. Full RPC framework (gRPC, JSON-RPC).

Cognitive memory has a property the average request/response service does not: state changes initiated by one client are interesting to other clients. Agent A storing a fact is something agent B wants to know about, immediately, without polling.

## Decision

The protocol has three payload kinds in one envelope: `Request`, `Response`, `Event`. Events are pushed by the daemon on connections that have explicitly subscribed to event kinds. Requests carry a monotonic per-connection `id`; responses echo it; events use `id = 0`.

## Reasoning

Why three payload kinds in one envelope rather than a separate event channel:

- **Single connection per client.** Subscribers do not need to open a second socket. This keeps clients simple — open one connection, send `Hello`, optionally `Subscribe`, then exchange messages.
- **Multiplexing is cheap on local IPC.** Unix-socket bandwidth is plentiful; any concern about events stomping on responses is overblown for the volumes we handle.
- **Forward-compatibility.** A new event kind is a new variant in the `Event` enum. Existing clients ignore unknown variants. Adding events does not require new connections, new sockets, or new handshakes.
- **`lazydap` validates the shape.** lazydap's `tokio::sync::broadcast` channel pattern with kind-filtered subscribers proved out the design in a sibling project.

Why not full gRPC / JSON-RPC:

- gRPC would force protobuf, code generation, and a heavier client surface than `cm-cli`, the SDKs, or `cm-http` need. The wire-format simplicity of length-delimited JSON is worth more than gRPC's typed code generation.
- JSON-RPC has the right shape but its standard does not natively support server-pushed messages on the same connection. We'd either invent a non-standard JSON-RPC extension or graft on a separate channel — both worse than just defining what we need.

Why request `id`s and event `id = 0`:

- A client may have multiple in-flight requests. `id` correlates each response to its request without ordering constraints.
- Events have no associated request, so `id = 0` is a sentinel. Clients dispatch by `IpcPayload` variant, then by `id` for `Response`.
- 64-bit `id` overflows after ~600 years at one request per nanosecond — non-issue.

## Consequences

### Positive

- One client, one connection, one mental model.
- Server push is first-class without a second transport.
- Out-of-order responses are explicitly supported (clients pipeline freely).
- Easy to inspect on the wire — `nc -U` plus a JSON pretty-printer is the developer console.

### Negative

- Clients must implement a small dispatch layer (`IpcPayload` variant → response handler / event handler). Trivial in practice but more work than a pure request/response protocol.
- Event delivery is best-effort. Disconnected clients miss events; durable replay is not in v1. This is a tradeoff we accept; durable replay can be added in a future version (a `since: event_id` parameter on `Subscribe`) without breaking the wire format.

### Neutral

- HTTP bridge in v1 does *not* implement events. Adding SSE or WebSocket support is a Phase-12-or-later addition (`docs/concepts/http-bridge.md` §4.4). Unix-socket clients get events; HTTP clients do not, in v1.

## Alternatives considered

- **Pure request/response, clients poll for changes.** Rejected: polling at the latency cognitive memory is interesting at (sub-second cross-agent visibility) is wasteful and clumsy.
- **Separate event socket.** Rejected: doubles the connection management surface for marginal gain.
- **gRPC, JSON-RPC.** Rejected for surface-area reasons above.
- **Length-delimited JSON without a discriminator on `IpcPayload`.** Rejected: makes the wire harder to read and forward-compatibility weaker.

## References

- `PROTOCOL.md`, especially §3 (envelope) and §6 (events).
- `mxr` for request/response framing (`crates/protocol/codec.rs`).
- `lazydap` `ARCHITECTURE.md` decision D015 — equivalent pattern for debugger events.
