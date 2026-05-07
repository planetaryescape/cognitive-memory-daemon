# ADR 0005 — HTTP bridge: loopback-only with per-request bearer

- **Status**: Accepted
- **Date**: 2026-05-07
- **Deciders**: bhekanik

## Context

Some clients can't speak Unix sockets directly: browsers, some language runtimes, REST-based tooling, certain web extensions. The daemon needs a way to serve them without compromising the security model that makes Unix-socket-only attractive.

The two main axes:
- **Bind**: localhost only, or any interface?
- **Auth**: none / bearer / OAuth / mTLS?

## Decision

A separate `cm-http` binary that:
- Binds `127.0.0.1` only. Refuses to start if configured with a non-loopback address.
- Requires `Authorization: Bearer <token>` on every request.
- Tokens are minted by the daemon via `Diagnostics::MintBridgeToken` (admin scope), shown to the user once, stored only as a salted hash.
- Tokens are scoped (`read`, `write`, `admin`) and have an expiry.
- The bridge process holds no daemon state — it is a translation layer between HTTP/JSON and Unix-socket length-delimited JSON.

## Reasoning

**Why a separate binary, not in-daemon HTTP?**
- The daemon's "no network bind" invariant (`SECURITY.md` §2 T4) is load-bearing: a CI lint can grep for `TcpListener` in `crates/daemon/` and fail. Adding HTTP support inside the daemon would lose that invariant.
- Bridge crash isolation: if `cm-http` crashes (a fuzzer, a buggy client), the daemon is untouched.
- Different users may want different bridge configurations (CORS origins, port). Keeping it separate makes that a per-binary concern.

**Why localhost only?**
- The daemon is a per-OS-user service. Exposing it on a network interface would let other machines reach the user's memory. There is no use case for that in v1.
- Localhost binding plus token auth is the same shape as Jupyter, Vite, and many local dev servers — well-understood by users and clients.

**Why bearer tokens, not OAuth?**
- OAuth is for third-party authorisation against an identity provider. The HTTP bridge has one tenant per token; there's no third party.
- Per-request bearer with mint/revoke endpoints is the simplest thing that works for "give my browser tab access to my memory" and "revoke that script's access".

**Why per-request, not session cookies?**
- Cookies would imply session state in the bridge. Bearer tokens stay stateless on the bridge side; daemon is the source of truth for token validity.
- Cookies invite CSRF concerns; bearer-only avoids them.

**Why hash tokens at rest (don't store the raw token)?**
- If `data.db` leaks, raw tokens shouldn't be reusable. Salted SHA-256 of the token is sufficient — the token itself was never stored.
- Token entropy (≥ 192 bits) makes brute-forcing the hash infeasible.

**Why scopes?**
- A browser-based read dashboard does not need write access. A script that ingests turns does not need admin (mint-token, log-tail) access. Capability minimisation reduces blast radius if a token leaks.

**Why expiry?**
- Long-lived secrets become forgotten secrets. 30-day default forces periodic re-mint, which makes accidental committed-to-git tokens self-limiting.

## Consequences

### Positive

- Daemon stays Unix-socket-only.
- Browser clients have a documented, secure path.
- Tokens are revocable and scope-limited.
- Crash isolation between bridge and daemon.

### Negative

- Two binaries to install and run instead of one. Mitigated by `brew install cognitive-memory` packaging both.
- Token UX: the user has to mint, copy, and store a token themselves on first browser use. The CLI command (`cm mint-bridge-token --scope read --ttl 30d`) is the friendly path.
- No event streaming over HTTP in v1. Browser clients that need live updates can poll `Memory::List ?since=` until SSE/WebSocket lands post-v1.

### Neutral

- CORS off by default. Origins must be allow-listed via `COGNITIVE_MEMORY_HTTP_CORS_ORIGINS`. No `*` wildcard support.
- The bridge does not implement CSRF protection because it does not use cookies; bearer in `Authorization` header is not exploitable via CSRF.

## Alternatives considered

- **In-daemon HTTP** with the same auth model. Rejected for crash isolation and the network-bind invariant.
- **mTLS** between bridge and clients. Rejected as too operationally heavy for the use case (each browser tab needing a client cert is hostile UX).
- **Unix-socket-only, no HTTP at all.** Rejected because browsers and some other clients can't speak Unix sockets without an intermediary anyway. The bridge is the intermediary.
- **OAuth / OIDC.** Rejected as wrong shape (no identity provider, single tenant per token).

## References

- `SECURITY.md` §2 T4, T5.
- `docs/concepts/http-bridge.md` for the implementation contract.
