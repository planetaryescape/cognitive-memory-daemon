//! OpenAI + Anthropic provider tests using a wiremock HTTP server.
//!
//! No live API calls in CI. Each test stands up a mock that mimics the
//! provider's response shape, points the provider at the mock's base URL,
//! and verifies the request shape (auth header, JSON body) and the
//! response parsing.

#![allow(clippy::panic, clippy::unwrap_used)]

use cognitive_memory_llm::{
    AnthropicProvider, ExtractedMemory, ExtractionRequest, LlmError, LlmProvider, OpenAiProvider,
    Turn,
};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn transcript() -> Vec<Turn> {
    vec![Turn {
        role: "user".to_string(),
        content: "I prefer tea over coffee.".to_string(),
    }]
}

#[tokio::test]
async fn openai_provider_parses_extraction_response() {
    let server = MockServer::start().await;

    let response_body = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "{\"memories\": [{\"content\": \"User prefers tea over coffee.\", \"category\": \"semantic\", \"memory_type\": \"preference\"}]}"
            }
        }]
    });

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new("test-key", "gpt-4o-mini").with_base_url(server.uri());
    let req = ExtractionRequest {
        transcript: &transcript(),
        api_key_override: None,
    };
    let result = provider.extract(req).await.unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0],
        ExtractedMemory {
            content: "User prefers tea over coffee.".to_string(),
            category: "semantic".to_string(),
            memory_type: "preference".to_string(),
        }
    );
}

#[tokio::test]
async fn openai_provider_uses_per_request_key_override() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer override-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "{\"memories\": []}"
                }
            }]
        })))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new("default-key", "gpt-4o-mini").with_base_url(server.uri());
    let req = ExtractionRequest {
        transcript: &transcript(),
        api_key_override: Some("override-key".to_string()),
    };
    // The mock matches the *override* auth header — if the override isn't
    // honoured the mock returns 404 and the test fails.
    let result = provider.extract(req).await.unwrap();
    assert!(result.is_empty());
}

#[tokio::test]
async fn openai_provider_surfaces_429_as_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new("k", "gpt-4o").with_base_url(server.uri());
    let result = provider
        .extract(ExtractionRequest {
            transcript: &transcript(),
            api_key_override: None,
        })
        .await;
    assert!(matches!(result, Err(LlmError::RateLimited { .. })));
}

#[tokio::test]
async fn openai_provider_returns_missing_api_key_when_empty() {
    let provider = OpenAiProvider::new("", "gpt-4o");
    let result = provider
        .extract(ExtractionRequest {
            transcript: &transcript(),
            api_key_override: None,
        })
        .await;
    assert!(matches!(result, Err(LlmError::MissingApiKey { .. })));
}

#[tokio::test]
async fn anthropic_provider_parses_extraction_response() {
    let server = MockServer::start().await;

    let response_body = json!({
        "content": [{
            "type": "text",
            "text": "{\"memories\": [{\"content\": \"User likes Rust.\", \"category\": \"semantic\", \"memory_type\": \"preference\"}]}"
        }]
    });

    Mock::given(method("POST"))
        .and(path("/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(&server)
        .await;

    let provider =
        AnthropicProvider::new("test-key", "claude-haiku-4-5-20251001").with_base_url(server.uri());
    let req = ExtractionRequest {
        transcript: &transcript(),
        api_key_override: None,
    };
    let result = provider.extract(req).await.unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].content, "User likes Rust.");
}

#[tokio::test]
async fn anthropic_provider_strips_markdown_fences() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{
                "type": "text",
                "text": "```json\n{\"memories\": []}\n```"
            }]
        })))
        .mount(&server)
        .await;

    let provider =
        AnthropicProvider::new("k", "claude-haiku-4-5-20251001").with_base_url(server.uri());
    let result = provider
        .extract(ExtractionRequest {
            transcript: &transcript(),
            api_key_override: None,
        })
        .await
        .unwrap();
    assert!(result.is_empty(), "fenced JSON must parse cleanly");
}
