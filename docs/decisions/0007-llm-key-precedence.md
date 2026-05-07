# ADR 0007 — LLM key precedence

- **Status**: Accepted
- **Date**: 2026-05-07
- **Deciders**: bhekanik

## Context

The daemon needs LLM API keys for extraction (`Memory::ExtractAndStore`, `Memory::Ingest`) and may need embedding-provider keys when a hosted embedding provider is the default or per-request override.

Multiple sources can supply a key:
- Daemon process environment at startup.
- Daemon config file (`config.toml`).
- Per-request override field in `Request::Memory(...)`.

The user explicitly chose "both" — daemon-owned keys *and* per-request override. This ADR documents the precedence rules so behaviour is deterministic.

## Decision

For any given LLM or embedding provider, the key is resolved in this order (highest priority wins):

1. **Per-request override** (`llm_override.api_key` or `embedding_override.api_key`, when non-null).
2. **Process environment** (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc., as named in the provider's config).
3. **Config file** (`llm.<provider>.api_key_env` resolves to an env var name; the value of that env var is the key — `config.toml` does not contain the key directly).
4. **OS keychain** (Phase 13). Provider keys can be stored in the macOS Keychain via `cm key set <provider>`; the daemon reads from the keychain at startup or on first use.

If no source supplies a key, calls to that provider return `Response::Error { kind: NoLlmConfigured | NoEmbeddingProviderConfigured, message: "..." }`.

## Reasoning

**Why per-request override at the top?**
- Different agents may have different relationships with providers. A personal script might use the user's dev key; a collaborative agent might use a sandbox key with stricter limits. Per-request override lets each request pay from its own bucket without changing daemon defaults.
- Keys passed per-request live only in the request's lifetime — never persisted, never logged.

**Why env over config file?**
- Env vars are the conventional place for secrets. Config files are more likely to be accidentally committed to git.
- The config file points to the *name* of the env var (`api_key_env = "OPENAI_API_KEY"`), not the value. So the config file is safe to share if env vars are not.

**Why keychain only at the lowest priority?**
- Keychain is the most user-friendly long-term store, but its access semantics depend on the OS prompt model. Falling back to it when neither env nor config is set is the right default; preferring it would make environment-variable overrides surprisingly invisible.

**Why never log keys?**
- Obvious in spirit; codified explicitly because regressions creep in. `tracing-subscriber` redacts known-key field names; the request `llm_override.api_key` is a named field flagged for redaction in the dispatcher's tracing layer.

## Consequences

### Positive

- Deterministic: any reader can predict which key wins.
- Composable: a daemon configured with one default key can serve agents that override per-call.
- Secure-by-default: keys live in env or keychain, not in `data.db` or `config.toml`.

### Negative

- Four sources is more than the minimum. Some users may forget the order; the cheat sheet in `docs/operations/configuration.md` §5 is the mitigation.
- Per-request keys must be transmitted on the wire (the wire is a Unix socket, mode 0700 — only the OS user can read it, but it's still in process memory of both sides).

### Neutral

- The daemon enforces no rate limit on per-request-key calls beyond what the provider itself returns. A bad request-side actor can blow up someone else's quota. Per-request keys are scoped to that request only; the daemon does not cache them.

## Alternatives considered

- **Daemon-only keys, no per-request override.** Rejected: removes a useful capability (different agents using different keys) for marginal simplicity.
- **Per-request-only keys, no daemon defaults.** Rejected: hostile UX — every agent has to handle key plumbing. Daemon defaults are the convenient path.
- **Auto-rotate keys.** Out of scope for v1.

## References

- `docs/operations/configuration.md` §5 (precedence cheat sheet).
- `SECURITY.md` §2 T2 (key handling threat).
- `PROTOCOL.md` (`llm_override`, `embedding_override` fields on `Memory::*` requests).
