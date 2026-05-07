//! Embedding-layer tests.
//!
//! Covers the trait contract via `FakeEmbeddingProvider`, the cache wrapper,
//! and canonicalisation properties. Real-model tests for `LocalProvider`
//! live behind the `local-model` feature and are not part of CI.

#![allow(clippy::panic, clippy::unwrap_used)]

use cognitive_memory_embeddings::{CachedEmbeddings, EmbeddingProvider, FakeEmbeddingProvider};
use cognitive_memory_store::Store;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn fake_provider_embed_is_deterministic() {
    let provider = FakeEmbeddingProvider::new("local", "bge-small-en-v1.5", 384);

    let v1 = provider.embed("Hello, world!").await.unwrap();
    let v2 = provider.embed("Hello, world!").await.unwrap();

    assert_eq!(v1.len(), 384);
    assert_eq!(v1, v2, "embedding must be deterministic for the same input");
}

#[tokio::test]
async fn fake_provider_distinct_inputs_produce_distinct_vectors() {
    let provider = FakeEmbeddingProvider::new("local", "bge-small-en-v1.5", 384);

    let v1 = provider.embed("apple").await.unwrap();
    let v2 = provider.embed("orange").await.unwrap();

    assert_ne!(v1, v2);
}

#[tokio::test]
async fn cached_embeddings_returns_cached_value_on_second_call() {
    let store = Store::in_memory().await.unwrap();
    let provider = FakeEmbeddingProvider::new("test", "test-model", 16);
    let cached = CachedEmbeddings::new(provider, &store);

    let first = cached.embed("the quick brown fox").await.unwrap();
    let second = cached.embed("the quick brown fox").await.unwrap();

    assert_eq!(first, second);

    // Confirm the cache table actually contains a row.
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM embedding_cache")
        .fetch_one(store.reader())
        .await
        .unwrap();
    assert_eq!(
        count.0, 1,
        "expected exactly one cache row after two identical embeds"
    );
}

#[tokio::test]
async fn cached_embeddings_canonicalises_inputs_so_whitespace_variants_share_cache() {
    let store = Store::in_memory().await.unwrap();
    let provider = FakeEmbeddingProvider::new("test", "test-model", 16);
    let cached = CachedEmbeddings::new(provider, &store);

    cached.embed("hello   world").await.unwrap();
    cached.embed("  hello world  ").await.unwrap();

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM embedding_cache")
        .fetch_one(store.reader())
        .await
        .unwrap();
    assert_eq!(
        count.0, 1,
        "whitespace-equivalent inputs must collapse to one cache entry"
    );
}

#[tokio::test]
async fn cached_embeddings_separates_providers_in_cache() {
    let store = Store::in_memory().await.unwrap();

    let local = CachedEmbeddings::new(
        FakeEmbeddingProvider::new("local", "bge-small-en-v1.5", 16),
        &store,
    );
    let openai = CachedEmbeddings::new(
        FakeEmbeddingProvider::new("openai", "text-embedding-3-small", 16),
        &store,
    );

    local.embed("hello").await.unwrap();
    openai.embed("hello").await.unwrap();

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM embedding_cache")
        .fetch_one(store.reader())
        .await
        .unwrap();
    assert_eq!(
        count.0, 2,
        "same text under different (provider, model) keys must produce two cache entries"
    );
}
