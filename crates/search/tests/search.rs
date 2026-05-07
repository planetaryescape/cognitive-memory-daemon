//! Search-layer integration tests against a real in-memory store.

#![allow(clippy::panic, clippy::unwrap_used)]

use cognitive_memory_embeddings::{EmbeddingProvider, FakeEmbeddingProvider};
use cognitive_memory_search::{SearchOptions, Searcher};
use cognitive_memory_store::{MemoryRepo, MemoryRow, Store};
use pretty_assertions::assert_eq;

const PROVIDER: &str = "test";
const MODEL: &str = "test-model";
const DIM: usize = 16;

async fn store_memory_with_embedding(
    repo: &MemoryRepo<'_>,
    provider: &FakeEmbeddingProvider,
    user_id: &str,
    id: &str,
    content: &str,
    valid_until: Option<i64>,
) {
    let vector = provider.embed(content).await.unwrap();
    let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
    let mut row = MemoryRow::new_minimal(id, user_id, content, "semantic", "fact", 100);
    row.embedding = Some(bytes);
    row.embedding_provider = Some(PROVIDER.to_string());
    row.embedding_model = Some(MODEL.to_string());
    row.valid_until = valid_until;
    repo.insert(&row).await.unwrap();
}

#[tokio::test]
async fn search_returns_most_similar_memory_first() {
    let store = Store::in_memory().await.unwrap();
    let provider = FakeEmbeddingProvider::new(PROVIDER, MODEL, DIM);
    let repo = MemoryRepo::new(&store);

    store_memory_with_embedding(&repo, &provider, "alice", "m1", "apple", None).await;
    store_memory_with_embedding(&repo, &provider, "alice", "m2", "orange", None).await;
    store_memory_with_embedding(&repo, &provider, "alice", "m3", "banana", None).await;

    let searcher = Searcher::new(&store);
    let query_vec = provider.embed("apple").await.unwrap();
    let opts = SearchOptions::new(PROVIDER, MODEL, 1000);

    let results = searcher.search("alice", &query_vec, &opts).await.unwrap();

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].memory_id, "m1", "exact-match must rank first");
    assert!(
        results[0].score >= results[1].score && results[1].score >= results[2].score,
        "results must be sorted by score descending"
    );
}

#[tokio::test]
async fn search_isolates_results_by_user_id() {
    let store = Store::in_memory().await.unwrap();
    let provider = FakeEmbeddingProvider::new(PROVIDER, MODEL, DIM);
    let repo = MemoryRepo::new(&store);

    store_memory_with_embedding(&repo, &provider, "alice", "m_alice", "secret", None).await;
    store_memory_with_embedding(&repo, &provider, "bob", "m_bob", "secret", None).await;

    let searcher = Searcher::new(&store);
    let query_vec = provider.embed("secret").await.unwrap();
    let opts = SearchOptions::new(PROVIDER, MODEL, 1000);

    let alice_hits = searcher.search("alice", &query_vec, &opts).await.unwrap();
    assert_eq!(alice_hits.len(), 1);
    assert_eq!(alice_hits[0].memory_id, "m_alice");

    let bob_hits = searcher.search("bob", &query_vec, &opts).await.unwrap();
    assert_eq!(bob_hits.len(), 1);
    assert_eq!(bob_hits[0].memory_id, "m_bob");
}

#[tokio::test]
async fn search_respects_limit() {
    let store = Store::in_memory().await.unwrap();
    let provider = FakeEmbeddingProvider::new(PROVIDER, MODEL, DIM);
    let repo = MemoryRepo::new(&store);

    for i in 0..5 {
        store_memory_with_embedding(
            &repo,
            &provider,
            "alice",
            &format!("m{i}"),
            &format!("memory {i}"),
            None,
        )
        .await;
    }

    let searcher = Searcher::new(&store);
    let query_vec = provider.embed("memory 0").await.unwrap();
    let opts = SearchOptions {
        limit: 2,
        ..SearchOptions::new(PROVIDER, MODEL, 1000)
    };

    let results = searcher.search("alice", &query_vec, &opts).await.unwrap();
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn search_filters_expired_memories_by_default() {
    let store = Store::in_memory().await.unwrap();
    let provider = FakeEmbeddingProvider::new(PROVIDER, MODEL, DIM);
    let repo = MemoryRepo::new(&store);

    store_memory_with_embedding(&repo, &provider, "alice", "live", "live", None).await;
    store_memory_with_embedding(&repo, &provider, "alice", "dead", "dead", Some(500)).await;

    let searcher = Searcher::new(&store);
    let query_vec = provider.embed("anything").await.unwrap();
    // now=1000 > 500 (the dead memory's valid_until)
    let opts = SearchOptions::new(PROVIDER, MODEL, 1000);

    let results = searcher.search("alice", &query_vec, &opts).await.unwrap();
    let ids: Vec<&str> = results.iter().map(|r| r.memory_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["live"],
        "expired memory must be filtered out by default"
    );
}

#[tokio::test]
async fn search_with_deep_recall_includes_expired() {
    let store = Store::in_memory().await.unwrap();
    let provider = FakeEmbeddingProvider::new(PROVIDER, MODEL, DIM);
    let repo = MemoryRepo::new(&store);

    store_memory_with_embedding(&repo, &provider, "alice", "live", "live", None).await;
    store_memory_with_embedding(&repo, &provider, "alice", "dead", "dead", Some(500)).await;

    let searcher = Searcher::new(&store);
    let query_vec = provider.embed("anything").await.unwrap();
    let opts = SearchOptions {
        deep_recall: true,
        ..SearchOptions::new(PROVIDER, MODEL, 1000)
    };

    let results = searcher.search("alice", &query_vec, &opts).await.unwrap();
    assert_eq!(
        results.len(),
        2,
        "deep_recall must surface expired memories"
    );
}

#[tokio::test]
async fn search_skips_memories_under_different_provider_or_model() {
    let store = Store::in_memory().await.unwrap();
    let provider_a = FakeEmbeddingProvider::new("local", "bge-small", DIM);
    let provider_b = FakeEmbeddingProvider::new("openai", "text-embedding-3-small", DIM);
    let repo = MemoryRepo::new(&store);

    // Memory under provider_a:
    let vec_a = provider_a.embed("apple").await.unwrap();
    let bytes_a: Vec<u8> = vec_a.iter().flat_map(|f| f.to_le_bytes()).collect();
    let mut row_a = MemoryRow::new_minimal("ma", "alice", "apple", "semantic", "fact", 100);
    row_a.embedding = Some(bytes_a);
    row_a.embedding_provider = Some("local".to_string());
    row_a.embedding_model = Some("bge-small".to_string());
    repo.insert(&row_a).await.unwrap();

    // Search with provider_b → no results (no memory under that provider).
    let searcher = Searcher::new(&store);
    let query_vec = provider_b.embed("apple").await.unwrap();
    let opts = SearchOptions::new("openai", "text-embedding-3-small", 1000);

    let results = searcher.search("alice", &query_vec, &opts).await.unwrap();
    assert!(
        results.is_empty(),
        "search must not return memories embedded under a different (provider, model)"
    );
}

#[tokio::test]
async fn hybrid_search_combines_dense_and_bm25_via_rrf() {
    let store = Store::in_memory().await.unwrap();
    let provider = FakeEmbeddingProvider::new(PROVIDER, MODEL, DIM);
    let repo = MemoryRepo::new(&store);

    // Insert memories with distinct content.
    store_memory_with_embedding(&repo, &provider, "alice", "m1", "rust async tokio", None).await;
    store_memory_with_embedding(
        &repo,
        &provider,
        "alice",
        "m2",
        "python pandas dataframe",
        None,
    )
    .await;
    store_memory_with_embedding(
        &repo,
        &provider,
        "alice",
        "m3",
        "javascript typescript node",
        None,
    )
    .await;

    let searcher = Searcher::new(&store);
    let query_vec = provider.embed("rust async tokio").await.unwrap();
    let opts = SearchOptions {
        hybrid: true,
        query_text: Some("rust async tokio".to_string()),
        ..SearchOptions::new(PROVIDER, MODEL, 1000)
    };

    let results = searcher.search("alice", &query_vec, &opts).await.unwrap();
    assert!(!results.is_empty(), "hybrid search should return results");
    assert_eq!(
        results[0].memory_id, "m1",
        "exact match should top fused ranking"
    );
}

#[tokio::test]
async fn hybrid_search_errors_without_query_text() {
    let store = Store::in_memory().await.unwrap();
    let provider = FakeEmbeddingProvider::new(PROVIDER, MODEL, DIM);
    let repo = MemoryRepo::new(&store);
    store_memory_with_embedding(&repo, &provider, "alice", "m1", "anything", None).await;

    let opts = SearchOptions {
        hybrid: true,
        query_text: None,
        ..SearchOptions::new(PROVIDER, MODEL, 1000)
    };
    let query_vec = provider.embed("anything").await.unwrap();
    let result = Searcher::new(&store)
        .search("alice", &query_vec, &opts)
        .await;
    assert!(result.is_err(), "hybrid without query_text must fail");
}

#[tokio::test]
async fn search_returns_dimension_mismatch_when_query_and_memory_disagree() {
    use cognitive_memory_search::SearchError;

    let store = Store::in_memory().await.unwrap();
    let provider = FakeEmbeddingProvider::new(PROVIDER, MODEL, 16);
    let repo = MemoryRepo::new(&store);

    // Insert a memory with 16-dim embedding.
    store_memory_with_embedding(&repo, &provider, "alice", "m1", "apple", None).await;

    // Query with a 32-dim vector → mismatch.
    let bad_query = vec![0.5_f32; 32];
    let opts = SearchOptions::new(PROVIDER, MODEL, 1000);

    let result = Searcher::new(&store)
        .search("alice", &bad_query, &opts)
        .await;
    match result {
        Err(SearchError::DimensionMismatch { query, memory }) => {
            assert_eq!(query, 32);
            assert_eq!(memory, 16);
        }
        other => panic!("expected DimensionMismatch, got {other:?}"),
    }
}
