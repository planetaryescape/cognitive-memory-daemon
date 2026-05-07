//! Local LLM provider via `llama-cpp-2` (FFI to llama.cpp).
//!
//! Loads a GGUF model file from disk, runs greedy single-token decoding
//! in a blocking thread, returns decoded text. Used by the daemon's
//! conflict judge (4-label classification) and consolidation
//! summariser. Apple Silicon Metal acceleration is enabled via the
//! `llama-cpp-2/metal` feature.
//!
//! Behind the `local-llm` cargo feature so the default build stays
//! slim. Default model: Qwen3-4B-Instruct-2507 Q4_K_M (~2.5 GB,
//! Apache 2.0). Use `cm download-model` to fetch.

use crate::{ExtractedMemory, ExtractionRequest, LlmError, LlmProvider};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::OnceLock;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use std::num::NonZeroU32;

/// Process-wide llama.cpp backend. `LlamaBackend::init()` may only be
/// called once per process; this OnceLock enforces that.
static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();

fn backend() -> Result<&'static LlamaBackend, LlmError> {
    BACKEND.get_or_init(|| {
        // `init` can fail for missing CUDA/Metal libs; we don't
        // want to swallow that, so panic on first init failure.
        // The only realistic path to None is OOM at startup.
        LlamaBackend::init().expect("llama backend init")
    });
    Ok(BACKEND.get().expect("backend was just initialized"))
}

pub struct LocalLlmProvider {
    model_path: PathBuf,
    model_name: String,
    /// Lazily-loaded model. Loading takes a few seconds for a 2.5GB
    /// GGUF; we do it once on the first `complete()` call so daemon
    /// startup stays snappy.
    model: tokio::sync::OnceCell<LlamaModel>,
}

impl LocalLlmProvider {
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        let model_path = model_path.into();
        let model_name = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("local-gguf")
            .to_string();
        Self {
            model_path,
            model_name,
            model: tokio::sync::OnceCell::new(),
        }
    }

    async fn ensure_model(&self) -> Result<&LlamaModel, LlmError> {
        self.model
            .get_or_try_init(|| async {
                let path = self.model_path.clone();
                let model = tokio::task::spawn_blocking(move || {
                    let backend = backend()?;
                    let model_params = LlamaModelParams::default();
                    LlamaModel::load_from_file(backend, &path, &model_params)
                        .map_err(|e| LlmError::Provider(format!("load_from_file: {e}")))
                })
                .await
                .map_err(|e| LlmError::Provider(format!("join load: {e}")))??;
                Ok::<_, LlmError>(model)
            })
            .await
    }
}

#[async_trait]
impl LlmProvider for LocalLlmProvider {
    fn name(&self) -> &str {
        "local"
    }

    fn model(&self) -> &str {
        &self.model_name
    }

    async fn extract(&self, _req: ExtractionRequest<'_>) -> Result<Vec<ExtractedMemory>, LlmError> {
        // Memory extraction from a transcript needs a structured
        // output schema; not wired for the local provider yet. The
        // daemon's ingest path doesn't currently call extract — only
        // the SDK does, against hosted providers.
        Err(LlmError::Provider(
            "extract() not implemented for LocalLlmProvider; use a hosted provider for extraction"
                .to_string(),
        ))
    }

    async fn complete(&self, prompt: &str, max_tokens: usize) -> Result<String, LlmError> {
        // Ensure model is loaded (first-call cost).
        let model = self.ensure_model().await?;
        // `block_in_place` keeps the &LlamaModel borrow alive for
        // the duration of the synchronous decode, without needing
        // to widen the lifetime via unsafe transmute. Trade-off: the
        // calling task is parked on its current worker thread for
        // the duration of the decode (~hundreds of ms for short
        // prompts on Apple Silicon Metal). Acceptable since LLM
        // judging is off the user-facing search hot path — it runs
        // at tick time, sequentially, with a small batch.
        tokio::task::block_in_place(|| decode_greedy(model, prompt, max_tokens))
    }
}

/// Greedy decoding loop. Tokenize prompt, decode max_tokens tokens
/// (or stop on EOS), detokenize the new tokens to a string.
fn decode_greedy(model: &LlamaModel, prompt: &str, max_tokens: usize) -> Result<String, LlmError> {
    let backend = backend()?;
    let ctx_size = (prompt.len() / 2 + max_tokens + 256).max(2048) as u32;
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(ctx_size))
        .with_n_batch(ctx_size.min(512));
    let mut ctx = model
        .new_context(backend, ctx_params)
        .map_err(|e| LlmError::Provider(format!("new_context: {e}")))?;

    // Tokenize the prompt. AddBos::Always is safe for short-form
    // generation; chat templates would wrap the prompt themselves.
    let tokens = model
        .str_to_token(prompt, AddBos::Always)
        .map_err(|e| LlmError::Provider(format!("tokenize: {e}")))?;

    let n_prompt = tokens.len();
    if n_prompt as i32 >= ctx.n_ctx() as i32 {
        return Err(LlmError::Provider(format!(
            "prompt {n_prompt} tokens exceeds ctx {}",
            ctx.n_ctx()
        )));
    }

    let mut batch = LlamaBatch::new(512, 1);
    for (i, tok) in tokens.iter().enumerate() {
        let is_last = i == tokens.len() - 1;
        batch
            .add(*tok, i as i32, &[0], is_last)
            .map_err(|e| LlmError::Provider(format!("batch add: {e}")))?;
    }
    ctx.decode(&mut batch)
        .map_err(|e| LlmError::Provider(format!("decode prompt: {e}")))?;

    // Greedy sampler — deterministic single-best token for
    // classification-shape outputs.
    let mut sampler = LlamaSampler::greedy();

    let mut n_cur = batch.n_tokens();
    let mut output = String::new();
    let eos = model.token_eos();

    for _ in 0..max_tokens {
        let token: LlamaToken = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);
        if token == eos {
            break;
        }
        // Signature: token_to_piece_bytes(token, max_size, special, lstrip).
        // max_size 64 is plenty for one BPE piece; special=true keeps
        // model-control tokens decoded literally; lstrip None is the
        // safe default for streaming concat.
        let bytes = model
            .token_to_piece_bytes(token, 64, true, None)
            .map_err(|e| LlmError::Provider(format!("token_to_piece_bytes: {e}")))?;
        output.push_str(&String::from_utf8_lossy(&bytes));
        // Prepare next batch with the just-sampled token.
        batch.clear();
        batch
            .add(token, n_cur, &[0], true)
            .map_err(|e| LlmError::Provider(format!("batch add gen: {e}")))?;
        ctx.decode(&mut batch)
            .map_err(|e| LlmError::Provider(format!("decode gen: {e}")))?;
        n_cur += 1;
    }

    Ok(output)
}
