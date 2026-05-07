# Embedding strategy

Where embeddings come from, how they're cached, and how clients override the default.

## 1. Default: `bge-small-en-v1.5` via fastembed-rs

The daemon loads `bge-small-en-v1.5` into RAM at startup and serves all embedding requests from the same model instance.

| Property | Value |
| --- | --- |
| Source | BAAI, on Hugging Face |
| Dimension | 384 |
| Model size on disk | ~130 MB (ONNX) |
| RAM footprint | ~150–200 MB after load |
| Cold load | < 2 s on Apple Silicon |
| Warm inference | < 10 ms / query (CPU) |
| MTEB average | ~62 |

Loaded via `fastembed-rs`, which wraps an ONNX runtime. No PyTorch in the daemon. Fallback runtime: `ort` directly if `fastembed-rs` becomes a constraint.

Why this model: ADR `0003-bge-small-default-embedding.md` covers the reasoning. In short: small, fast, stable, ONNX-ready, BAAI's track record, MTEB performance is more than adequate for cognitive-memory queries (which are conversational and not adversarial IR).

## 2. Override: hosted providers per request

Clients may override per request:

```json
{
  "embedding_override": {
    "provider": "OpenAI",
    "model": "text-embedding-3-small",
    "api_key": null
  }
}
```

When `api_key` is null, the daemon uses its configured key for that provider. When set, it uses the per-request key. See `docs/decisions/0007-llm-key-precedence.md` for precedence rules.

Hosted providers in v1 (added per demand): OpenAI. Voyage and Cohere are easy follow-ups.

## 3. Cache

`embedding_cache` table:

```
provider TEXT NOT NULL,
model    TEXT NOT NULL,
text_hash BLOB NOT NULL,    -- SHA-256 of canonical text
embedding BLOB NOT NULL,    -- length depends on model
PRIMARY KEY (provider, model, text_hash)
```

Canonicalisation: trim, normalise whitespace, NFC unicode, lower-case if the model's tokenizer is uncased (bge-small is). The exact canonicalisation lives in `crates/embeddings/src/canonical.rs` and is part of the cache contract — changing it requires a cache version bump.

The cache is shared across:
- All `user_id`s (caching is by content, not by tenant — content hash collisions across tenants are not a leak because the cache returns the embedding only, not surrounding context).
- All clients (this is the whole point — agent A and agent B paying for the same conversation pay once).
- All requests (a `Memory::Store` and a later `Memory::Search` on the same string both hit cache).

Cache eviction: bounded by row count (default 1M). LRU pruning via `embeddings::cache_pruner` background task.

## 4. Dimension handling

Different providers return different dimensions. The store schema does not pin a dimension; embeddings are length-prefixed when serialised.

Search sees mixed dimensions only if the user actively switches default models or uses overrides. The current strategy:

- Searches by default use the daemon's default-provider embedding.
- A search with `embedding_override` re-embeds the query under the override and searches against memories' embeddings *of the same provider+model*.
- If a `(provider, model)` slice is sparse, the search may fall back to re-embedding stored memories on demand under the override, with results cached. Phase 3 calls this trade-off.

## 5. Local model lifecycle

- **First run**: model downloaded from Hugging Face on demand to `~/Library/Application Support/cognitive-memory/models/`. Daemon refuses to start in offline mode if the model is missing and no override is configured.
- **Updates**: model file is content-addressed. A new model version is a new directory. `cm doctor` reports the active model and any newer one available on disk.
- **Switching**: changing the default model via config requires a daemon restart. Changing requires no migration — old embeddings remain in cache under their `(provider, model)` key.

## 6. Future: local LLM extraction

The daemon's local model is for embeddings only. Local *extraction* (running an LLM extractor offline via Ollama or `mlx-llm`) is post-v1 backlog in `ROADMAP.md`. The same provider-trait shape will accommodate it; embeddings and extraction stay separate concerns.
