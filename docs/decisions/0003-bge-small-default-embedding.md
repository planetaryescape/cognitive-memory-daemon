# ADR 0003 — Default embedding model: bge-small-en-v1.5

- **Status**: Accepted
- **Date**: 2026-05-07
- **Deciders**: bhekanik

## Context

The daemon needs a default local embedding model loaded into RAM at startup so that an out-of-the-box install does not require any provider credentials to function. The model needs to:

- Load fast and stay loaded (the daemon may run for weeks).
- Run on CPU adequately on Apple Silicon (the user's primary platform).
- Have low enough RAM footprint to coexist with normal workstation use (target: < 250 MB resident steady-state).
- Have an ONNX or Rust-native inference path (the daemon is Rust; pulling in PyTorch is not acceptable).
- Have stable provenance (the model itself shouldn't disappear from distribution channels).
- Have good enough retrieval quality for cognitive-memory's queries (conversational memories, not adversarial information retrieval).

## Decision

The default local embedding model is `bge-small-en-v1.5` from BAAI, served via `fastembed-rs`.

## Reasoning

| Property | bge-small-en-v1.5 |
| --- | --- |
| Dimension | 384 |
| Disk size (ONNX) | ~130 MB |
| RAM after load | ~150–200 MB |
| Cold load on M-series | < 2 s |
| Warm inference | < 10 ms / query (CPU) |
| MTEB average | ~62 |
| ONNX availability | Yes, official |
| `fastembed-rs` ships it | Yes (default model) |

The shortlist also included `bge-base-en-v1.5` (768-dim, ~440MB, MTEB ~63.5), `nomic-embed-text-v1.5` (768-dim, Matryoshka, MTEB ~62), and `all-MiniLM-L6-v2` (384-dim, ~90MB, MTEB ~56).

Why bge-small over bge-base:
- The MTEB delta (~1.5 points) is smaller than the cost delta (3× model size, slower inference). For conversational memory retrieval — where queries are full sentences and the corpus is small (thousands, not millions) — the smaller model is on the right side of the price/quality curve.

Why bge-small over nomic-embed-v1.5:
- Nomic's Matryoshka property is interesting (truncate to 256/512 for index size) but adds operational complexity. For a small per-user store, the savings don't justify the extra surface.

Why bge-small over MiniLM-L6:
- MTEB is meaningfully lower (~56 vs ~62) and the size difference (90MB vs 130MB) is irrelevant in practice. We're not optimising for the last 40 MB.

Why `fastembed-rs` over raw `ort`:
- `fastembed-rs` ships the model loading, tokenization, and pooling out of the box; raw `ort` requires implementing all of those. Drop down to `ort` later if `fastembed-rs` becomes a constraint.

## Consequences

### Positive

- Default install works offline (after one-time model download). No credentials required to start using the daemon.
- 384-dim embeddings are cheap to store (1.5 KB per memory) and cheap to compare.
- Operational footprint is small enough to leave the daemon running with a browser open.

### Negative

- Hosted-provider embeddings (OpenAI `text-embedding-3-small`, Voyage, Cohere) generally edge out bge-small on quality. Users who care can override per-request. The default trades that quality for offline-capable, free, fast.
- Switching the default model later is a config-only change but does not re-embed existing memories. The cache remains valid only for `(provider, model, text_hash)` tuples it was originally written under.
- The model lives in RAM continuously. On low-memory Macs (< 8 GB), this is meaningful. We do not currently auto-unload on idle; ADR for that lands if/when a user reports the issue.

### Neutral

- The local provider is in addition to, not in place of, hosted providers. Users can configure OpenAI/Anthropic-as-default in `config.toml`; the local model still loads but is unused.

## Alternatives considered

- **bge-base-en-v1.5 as default.** Better quality, 3× the cost. Could be the right call if a user's corpus is large enough that the quality matters more than the cost; not the right default.
- **nomic-embed-text-v1.5.** Matryoshka is neat but adds complexity for marginal benefit at this scale.
- **all-MiniLM-L6-v2.** Smaller and faster but materially weaker on MTEB. Wrong end of the curve.
- **No default; require provider configuration before first use.** Rejected: hostile first-run experience. The whole point of the daemon is "install and it works".
- **Apple MLX-based local model on macOS.** Considered as a future optimisation. Currently, MLX integration in Rust requires more glue than `fastembed-rs` and the perf difference at our query rates is not the bottleneck. Revisit if the daemon ever needs sub-millisecond embedding latency.

## Migration

If we ever change the default model:
- Existing memories retain their old `(provider, model)` embeddings in the cache.
- Searches using the new default re-embed query and search against memories embedded under the new default; results from old-default memories require either re-embedding (expensive) or cross-model fallback (Phase 3 detail).
- An explicit `cm reembed --provider local --model bge-base-en-v1.5` command will exist in Phase 13 for users who want to re-embed their corpus on demand.

## References

- `docs/concepts/embedding-strategy.md` for the full embedding pipeline.
- `fastembed-rs`: https://github.com/Anush008/fastembed-rs (docs may have moved; check crates.io).
- BGE models: https://huggingface.co/BAAI/bge-small-en-v1.5.
- MTEB leaderboard: https://huggingface.co/spaces/mteb/leaderboard.
