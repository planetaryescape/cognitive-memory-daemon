# Observability

Current observability is CLI-visible: persistent daemon logs, structured status, `cm doctor`, and recent request traces. Metrics are still release hardening work.

## Logs

`cm-daemon` writes human-readable logs to the identity-scoped log file reported by `cm status`. `cm-http` logs to stderr unless supervised by your process manager.

Control verbosity with `RUST_LOG`:

```sh
COGNITIVE_MEMORY_LOG=info cm-daemon
COGNITIVE_MEMORY_LOG=cognitive_memory_daemon=debug,cognitive_memory_store=warn cm-daemon
```

When `cm` auto-spawns the daemon, it redirects daemon stdio to null and the daemon writes to its log file. For debugging, use:

```sh
cm status
cm trace
cm doctor
```

## CLI-visible diagnostics

Use these today:

```sh
cm status
cm counts
cm doctor
cm trace
cm --json status
cm --json counts
cm --json doctor
cm --json trace
```

`status` returns daemon version, protocol version, build id, PID, instance, socket path, PID path, DB path, log path, uptime, and memory count. `counts` returns hot/cold/stub/total for the selected `user_id`. `doctor` returns check results with an exit-code severity. `trace` returns recent request trace entries from the daemon's bounded ring.

## HTTP bridge diagnostics

`cm-http` logs startup, bind failures, daemon token minting, and rejected unknown bearer tokens. It does not log full tokens or request bodies.

## What is not shipped yet

- Metrics snapshots or a Prometheus endpoint.
- Log tailing through the protocol.
