//! Anthropic provider for memory extraction.
//!
//! Uses the Messages API. Anthropic does not support OpenAI-style
//! `response_format: json_object`, so we coerce JSON output via a strict
//! system prompt and parse the message content. Models that drift into
//! prose surface as `LlmError::Provider`.

use crate::{ExtractedMemory, ExtractionRequest, LlmError, LlmProvider};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Anthropic Messages API provider.
///
/// Default base URL is `https://api.anthropic.com/v1`. API version header
/// is hardcoded to a stable version known to support the Messages API.
pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            model: model.into(),
            base_url: "https://api.anthropic.com/v1".to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn extract(&self, req: ExtractionRequest<'_>) -> Result<Vec<ExtractedMemory>, LlmError> {
        let key = req.api_key_override.as_deref().unwrap_or(&self.api_key);
        if key.is_empty() {
            return Err(LlmError::MissingApiKey {
                provider: "anthropic".to_string(),
            });
        }

        let body = AnthropicMessagesRequest {
            model: &self.model,
            max_tokens: 1024,
            system: SYSTEM_PROMPT,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: format_transcript(req.transcript),
            }],
        };

        let url = format!("{}/messages", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Provider(format!("anthropic request: {e}")))?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(LlmError::RateLimited {
                retry_after_seconds: 60,
            });
        }
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(LlmError::Provider(format!("anthropic {status}: {text}")));
        }

        let parsed: AnthropicMessagesResponse = response
            .json()
            .await
            .map_err(|e| LlmError::Provider(format!("anthropic parse: {e}")))?;

        // Anthropic v1 has one ContentBlock variant (Text); pull the first.
        // When tool-use/image blocks ship, this gains arms.
        let text = parsed
            .content
            .into_iter()
            .next()
            .map(|AnthropicContentBlock::Text { text }| text)
            .ok_or_else(|| LlmError::Provider("anthropic returned no text block".to_string()))?;

        parse_extraction_payload(&text)
    }
}

#[derive(Serialize)]
struct AnthropicMessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<AnthropicMessage>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicMessagesResponse {
    content: Vec<AnthropicContentBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text { text: String },
}

const SYSTEM_PROMPT: &str = "You extract durable memories from conversations. \
Given the transcript that follows, output a single JSON object with key \"memories\" \
whose value is an array of objects, each with keys \"content\" (string), \
\"category\" (one of: episodic, semantic, procedural, core), and \"memory_type\" \
(one of: fact, preference, plan, transient_state, other). \
Return ONLY the JSON object, no prose, no markdown fences.";

fn format_transcript(transcript: &[crate::Turn]) -> String {
    transcript
        .iter()
        .map(|t| format!("{}: {}", t.role, t.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_extraction_payload(content: &str) -> Result<Vec<ExtractedMemory>, LlmError> {
    #[derive(Deserialize)]
    struct Wrapper {
        memories: Vec<ExtractedMemory>,
    }
    // Anthropic occasionally wraps in markdown fences despite the prompt;
    // strip a leading ```json / trailing ``` if present.
    let trimmed = content.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_end_matches("```").trim())
        .unwrap_or(trimmed);

    let parsed: Wrapper = serde_json::from_str(stripped)
        .map_err(|e| LlmError::Provider(format!("parse extraction JSON: {e}; raw={content}")))?;
    Ok(parsed.memories)
}
