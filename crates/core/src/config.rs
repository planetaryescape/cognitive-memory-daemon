//! Daemon configuration. TOML at `~/.config/cognitive-memory/config.toml`.
//!
//! Currently the only configurable surface is the LLM provider. The
//! file is optional — absent file ⇒ no LLM ⇒ heuristic fallback for
//! conflict resolution, no consolidation. Edit via `cm config set-llm`.
//!
//! Schema:
//! ```toml
//! [llm]
//! provider = "local" | "openai" | "anthropic" | "none"
//! model_path = "/path/to/qwen3-4b.gguf"        # provider = local
//! api_key_env = "OPENAI_API_KEY"               # provider = openai|anthropic
//! model = "gpt-4o-mini"                        # provider = openai|anthropic
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default)]
    pub llm: Option<LlmConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum LlmConfig {
    None,
    Local {
        model_path: PathBuf,
    },
    Openai {
        api_key_env: String,
        #[serde(default = "default_openai_model")]
        model: String,
    },
    Anthropic {
        api_key_env: String,
        #[serde(default = "default_anthropic_model")]
        model: String,
    },
}

fn default_openai_model() -> String {
    "gpt-4o-mini".to_string()
}

fn default_anthropic_model() -> String {
    "claude-haiku-4-5-20251001".to_string()
}

/// Path to the user's daemon config file. Honours XDG via `dirs::config_dir`.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .expect("config dir resolvable")
        .join("cognitive-memory")
        .join("config.toml")
}

impl DaemonConfig {
    /// Load from the canonical path. Missing file → empty default.
    /// Malformed file → error so the operator can fix it.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(ConfigError::Parse),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Io(e)),
        }
    }

    /// Persist to the canonical path. Creates parent dirs as needed.
    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(&config_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ConfigError::Io)?;
        }
        let text = toml::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        std::fs::write(path, text).map_err(ConfigError::Io)?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(std::io::Error),
    #[error("parse: {0}")]
    Parse(toml::de::Error),
    #[error("serialize: {0}")]
    Serialize(toml::ser::Error),
}
