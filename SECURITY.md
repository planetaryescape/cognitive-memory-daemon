# Security

Threat model and the mitigations the daemon implements. Keep this in sync with the code; if a mitigation regresses, this file is wrong.

## 1. Threat model

The daemon is a per-OS-user process holding the user's cognitive memory. Threats considered:

| # | Threat | Severity | In scope |
| --- | --- | --- | --- |
| T1 | Another local OS user reads or writes memory | High | Yes |
| T2 | Another local OS user reads the LLM/embedding API keys | High | Yes |
| T3 | A malicious local process running as the same OS user reads memory | Medium | Limited (see §3) |
| T4 | Network attacker reaches the daemon | High | Yes (mitigated by no network bind) |
| T5 | Network attacker reaches the HTTP bridge | High | Yes |
| T6 | Memory contents leak via logs or error messages | Medium | Yes |
| T7 | Memory contents leak to provider that the user did not configure | High | Yes |
| T8 | Daemon crash corrupts the SQLite store | Medium | Yes |
| T9 | Stale or hostile installation directory contents are loaded as data | Low | Yes |
| T10 | Supply chain attack on a Rust dependency | Medium | Yes |

Out of scope: physical attacks on the disk, OS-kernel compromises, attackers with sudo on the user's machine. Cognitive memory is at-rest unencrypted; an attacker with disk access reads it.

## 2. Mitigations

### T1 — local OS-user isolation

- Unix socket is mode `0700`. Parent directory `~/Library/Application Support/cognitive-memory/` is mode `0700`. The OS denies non-owner access.
- SQLite file `data.db` is mode `0600`. Created with `umask 077` set on daemon startup before any file is touched.
- PID file is mode `0600`. Log files in `~/Library/Logs/cognitive-memory/` are mode `0600`.

### T2 — API-key handling

- Keys are read from environment at startup, or from per-request override fields, or from the OS keychain (planned, Phase 13).
- Keys are stored only in process memory and (for daemon-config keys) in keychain. Never in `data.db`. Never in `kv`.
- Keys are redacted in any tracing field (`tracing::field` redaction list) and in error messages. A key never appears in `~/Library/Logs/cognitive-memory/daemon.log`.
- A regression test asserts that grepping the log file for a known test key after a request finds no matches.

### T3 — same-user process isolation

- Same-OS-user processes can connect to the socket. We accept this as a tradeoff of the per-user-daemon model. The mitigation is: do not run untrusted code as the same OS user as your cognitive memory.
- Future: the protocol can grow per-connection capability tokens (`Hello { user_id, capability_token }`) so that, e.g., a sandboxed agent gets a read-only token. Not in v1.

### T4 — no network bind

- `cm-daemon` only binds Unix sockets. It does not open TCP sockets in any code path.
- A grep-level CI rule (`forbidden_patterns` lint) flags any code introducing `TcpListener`, `bind("0.0.0.0", ...)`, or similar in the `daemon` crate.

### T5 — HTTP bridge

- `cm-http` binds `127.0.0.1` only. Loopback only.
- Every request requires `Authorization: Bearer <token>`. Tokens are minted by `Diagnostics::MintBridgeToken` and stored in `kv`. Token format: `cmb_<random_24_bytes_base64url>`. Token entropy ≥ 192 bits.
- Tokens are scoped to a `user_id` and a capability set.
- Tokens have an expiry (default 30d). Expired tokens reject with HTTP 401.
- Token storage in `kv` stores only a salted hash of the token, not the token itself. Token shown to the user once at mint time.
- Bridge logs HTTP requests but redacts the `Authorization` header.

### T6 — leak via logs / errors

- Memory contents are not logged at INFO or below. DEBUG/TRACE logs may include content but only when `RUST_LOG=cognitive_memory=trace` is explicitly set. Default level is INFO.
- Error messages returned to clients omit raw memory content unless the request was a `Memory::Get` for that exact memory.
- Provider error messages are passed through but pre-filtered for known sensitive patterns.

### T7 — provider boundary

- Memory contents leave the daemon only via configured `LlmProvider` and `EmbeddingProvider` calls.
- The provider used for any specific request is logged (provider name + model, no key).
- The local embedding provider (`bge-small-en-v1.5`) makes no network calls. Verifiable.

### T8 — crash safety

- SQLite WAL journal mode. `synchronous = NORMAL`. Crashes mid-write recover on next open.
- Migrations are idempotent; a half-applied migration replays cleanly.
- No use of `unsafe` in code paths that touch the store.

### T9 — install integrity

- The daemon runs from `~/Library/Application Support/cognitive-memory/` only for *data*. The binary itself is invoked by absolute path or via the user's `PATH`.
- The daemon does not load plugins or external code at runtime in v1. No `dlopen`. (Phase 1 may load `sqlite-vec` extension; that decision lands in an ADR with its own threat analysis.)

### T10 — supply chain

- `cargo-deny` enforces an allow-list of licenses and rejects yanked / known-vulnerable crates in CI.
- Dependency graph is reviewed on every `Cargo.lock` update. PRs that change `Cargo.lock` highlight new transitive dependencies in the description.

## 3. Reporting a vulnerability

Pre-v0.1.0: file an issue with the `security` label.
Post-v0.1.0: the project will publish a `SECURITY.md` disclosure address. Until then, issue tracking is fine since the user is the only deployment.

## 4. Cryptographic note

- Hashing for caches: SHA-256.
- Token randomness: `getrandom` (CSPRNG via OS).
- Token comparison: constant-time (`subtle::ConstantTimeEq`).
- No bespoke cryptography. If a future feature needs it (e.g., at-rest encryption), it gets an ADR and an external review.
