//! LLM-layer tests: provider trait, cache, rate limiter.

#![allow(clippy::panic, clippy::unwrap_used)]

use cognitive_memory_llm::{
    extraction_cache_key, CachedExtractor, ExtractionRequest, FakeLlmProvider,
    InMemoryExtractionCache, LlmProvider, RateLimiter, TokenBucket, Turn,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn sample_transcript() -> Vec<Turn> {
    vec![
        Turn {
            role: "user".to_string(),
            content: "I prefer tea over coffee.".to_string(),
        },
        Turn {
            role: "assistant".to_string(),
            content: "Got it.".to_string(),
        },
    ]
}

#[tokio::test]
async fn fake_extractor_extracts_user_turns() {
    let provider = FakeLlmProvider::new("fake", "v1");
    let transcript = sample_transcript();

    let extracted = provider
        .extract(ExtractionRequest {
            transcript: &transcript,
            api_key_override: None,
        })
        .await
        .unwrap();

    assert_eq!(extracted.len(), 1);
    assert_eq!(extracted[0].category, "semantic");
    assert!(extracted[0].content.contains("tea over coffee"));
}

#[tokio::test]
async fn cached_extractor_skips_provider_on_cache_hit() {
    let provider = FakeLlmProvider::new("fake", "v1");
    let calls = provider.calls.clone();
    let cache = InMemoryExtractionCache::new();
    let extractor = CachedExtractor::new(provider, cache);
    let transcript = sample_transcript();

    let _ = extractor
        .extract(ExtractionRequest {
            transcript: &transcript,
            api_key_override: None,
        })
        .await
        .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    let _ = extractor
        .extract(ExtractionRequest {
            transcript: &transcript,
            api_key_override: None,
        })
        .await
        .unwrap();
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "second call must be served from cache"
    );
}

#[test]
fn cache_key_changes_when_provider_or_model_changes() {
    let transcript = sample_transcript();
    let k1 = extraction_cache_key("openai", "gpt-4o-mini", &transcript);
    let k2 = extraction_cache_key("openai", "gpt-4o", &transcript);
    let k3 = extraction_cache_key("anthropic", "gpt-4o-mini", &transcript);
    assert_ne!(k1, k2, "model change must change key");
    assert_ne!(k1, k3, "provider change must change key");
}

#[test]
fn cache_key_changes_when_transcript_changes() {
    let t1 = sample_transcript();
    let mut t2 = t1.clone();
    t2[0].content = "I prefer coffee.".to_string();
    let k1 = extraction_cache_key("openai", "gpt-4o", &t1);
    let k2 = extraction_cache_key("openai", "gpt-4o", &t2);
    assert_ne!(k1, k2);
}

#[tokio::test]
async fn token_bucket_allows_initial_burst_at_capacity() {
    let bucket = Arc::new(TokenBucket::new(3.0, 1.0));
    let start = Instant::now();
    for _ in 0..3 {
        bucket.acquire().await;
    }
    assert!(
        start.elapsed() < Duration::from_millis(50),
        "first 3 calls within capacity must be ~instant"
    );
}

#[tokio::test]
async fn token_bucket_blocks_when_empty_until_refill() {
    let bucket = Arc::new(TokenBucket::new(1.0, 10.0)); // refill 10/s = 100ms per token
    bucket.acquire().await; // consume the one token

    let start = Instant::now();
    bucket.acquire().await; // must wait ~100ms for refill
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(80) && elapsed < Duration::from_millis(300),
        "expected ~100ms wait, got {elapsed:?}"
    );
}
