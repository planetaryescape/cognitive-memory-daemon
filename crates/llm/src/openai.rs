//! OpenAI provider for memory extraction.
//!
//! Uses the Chat Completions API with structured (JSON-mode) output. The
//! prompt asks the model to produce a JSON array of `{content, category,
//! memory_type}` objects given a transcript.

use crate::{ExtractedMemory, ExtractionRequest, LlmError, LlmProvider};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// OpenAI Chat Completions provider.
///
/// Default base URL is `https://api.openai.com/v1`. Override (for tests
/// against a mock server) with the `base_url` builder field.
pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            model: model.into(),
            base_url: "https://api.openai.com/v1".to_string(),
        }
    }

    /// Override the base URL — for testing against a mock OpenAI-compatible
    /// server. Leave the default in production.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn extract(&self, req: ExtractionRequest<'_>) -> Result<Vec<ExtractedMemory>, LlmError> {
        let key = req.api_key_override.as_deref().unwrap_or(&self.api_key);
        if key.is_empty() {
            return Err(LlmError::MissingApiKey {
                provider: "openai".to_string(),
            });
        }

        let messages = build_messages(req.transcript);
        let body = OpenAiChatRequest {
            model: &self.model,
            messages,
            response_format: ResponseFormat::JsonObject,
            temperature: 0.0,
        };

        let url = format!("{}/chat/completions", self.base_url);
        let response = self
            .client
            .post(&url)
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Provider(format!("openai request: {e}")))?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(LlmError::RateLimited {
                retry_after_seconds: 60,
            });
        }
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(LlmError::Provider(format!("openai {status}: {text}")));
        }

        let parsed: OpenAiChatResponse = response
            .json()
            .await
            .map_err(|e| LlmError::Provider(format!("openai parse: {e}")))?;

        let content = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::Provider("openai returned no choices".to_string()))?
            .message
            .content;

        parse_extraction_payload(&content)
    }
}

#[derive(Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    response_format: ResponseFormat,
    temperature: f32,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseFormat {
    JsonObject,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: ChatMessage,
}

const SYSTEM_PROMPT: &str = "You extract durable memories from conversations. \
Given the transcript that follows, output a single JSON object with key \"memories\" \
whose value is an array of objects, each with keys \"content\" (string, the memory \
in third-person paraphrase), \"category\" (one of: episodic, semantic, procedural, core), \
and \"memory_type\" (one of: fact, preference, plan, transient_state, other). \
Return ONLY the JSON object — no prose, no markdown.";

fn build_messages(transcript: &[crate::Turn]) -> Vec<ChatMessage> {
    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: SYSTEM_PROMPT.to_string(),
    }];
    let formatted_transcript = transcript
        .iter()
        .map(|t| format!("{}: {}", t.role, t.content))
        .collect::<Vec<_>>()
        .join("\n");
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: formatted_transcript,
    });
    messages
}

fn parse_extraction_payload(content: &str) -> Result<Vec<ExtractedMemory>, LlmError> {
    #[derive(Deserialize)]
    struct Wrapper {
        memories: Vec<ExtractedMemory>,
    }
    let parsed: Wrapper = serde_json::from_str(content.trim())
        .map_err(|e| LlmError::Provider(format!("parse extraction JSON: {e}; raw={content}")))?;
    Ok(parsed.memories)
}
