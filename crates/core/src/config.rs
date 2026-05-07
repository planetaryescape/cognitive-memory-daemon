//! Daemon configuration. TOML at `~/.config/cognitive-memory/config.toml`.
//!
//! Two configurable surfaces:
//! - `[llm]` selects the conflict-judge / consolidation provider
//!   (Phase 4). Absent ⇒ heuristic fallback. Edit via `cm config set-llm`.
//! - `[lifecycle]` overrides paper-faithful tunables (β per category,
//!   etc.) so benchmarks/tuning trials can flip params without rebuilds
//!   (Phase 0a-daemon). Absent ⇒ paper §3.2 Table 2 defaults.
//!
//! Schema:
//! ```toml
//! [llm]
//! provider = "local" | "openai" | "anthropic" | "none"
//! model_path = "/path/to/qwen3-4b.gguf"        # provider = local
//! api_key_env = "OPENAI_API_KEY"               # provider = openai|anthropic
//! model = "gpt-4o-mini"                        # provider = openai|anthropic
//!
//! [lifecycle.base_decay_rates]
//! semantic = 60.0                               # override one or more β_c
//! episodic = 30.0
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default)]
    pub llm: Option<LlmConfig>,
    /// Optional lifecycle overrides. Absent ⇒ daemon uses
    /// paper-faithful defaults from `LifecycleConfig::default()` plus
    /// `DecayModel::Power` (per `paper_faithful_lifecycle_config`).
    #[serde(default)]
    pub lifecycle: Option<LifecycleOverrides>,
}

/// Subset of `LifecycleConfig` exposed to operators via config.toml.
/// Only fields useful for runtime tuning are exposed; structural fields
/// (`decay_model`, etc.) stay built-in to avoid accidental misconfig.
///
/// Absent fields ⇒ keep paper default. Present fields override per
/// category (for `base_decay_rates`) — partial overrides preserve
/// untouched categories' defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleOverrides {
    /// Per-category β_c (days). Wire-form keys: "episodic", "semantic",
    /// "core", "procedural". Mirrors paper §3.2 Table 2 / SDK
    /// `BASE_DECAY_RATES`. Use `inf` for procedural-style no-decay.
    #[serde(default)]
    pub base_decay_rates: Option<HashMap<String, f64>>,
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

#[cfg(test)]
mod tests {
    use super::*;

    // =====================================================================
    // [lifecycle] TOML surface — Phase 0a-daemon. Confirms the operator-
    // facing schema parses, supports partial overrides (one category at
    // a time), and survives an absent section. This is the ONLY surface
    // a benchmark trial uses to flip β_c on the daemon.
    // =====================================================================

    #[test]
    fn config_with_no_lifecycle_section_parses_to_none() {
        // Operators who have only [llm] configured shouldn't see lifecycle
        // overrides leak in. Default is None.
        let toml_text = r#"
            [llm]
            provider = "none"
        "#;
        let cfg: DaemonConfig = toml::from_str(toml_text).unwrap();
        assert!(cfg.lifecycle.is_none());
    }

    #[test]
    fn config_with_partial_lifecycle_override_parses() {
        // The whole point of the surface: change one category without
        // having to enumerate the others. Wire-form keys ("semantic"
        // etc.) so the same key set works in TOML and JSON harness configs.
        let toml_text = r#"
            [lifecycle.base_decay_rates]
            semantic = 60.0
        "#;
        let cfg: DaemonConfig = toml::from_str(toml_text).unwrap();
        let overrides = cfg.lifecycle.expect("lifecycle parsed");
        let rates = overrides.base_decay_rates.expect("rates parsed");
        assert_eq!(rates.get("semantic").copied(), Some(60.0));
        // No mention of episodic in the TOML ⇒ not present in the
        // override map (so it inherits paper default at merge time).
        assert!(rates.get("episodic").is_none());
    }

    #[test]
    fn config_with_multiple_lifecycle_overrides_parses() {
        let toml_text = r#"
            [lifecycle.base_decay_rates]
            episodic = 30.0
            semantic = 60.0
        "#;
        let cfg: DaemonConfig = toml::from_str(toml_text).unwrap();
        let rates = cfg
            .lifecycle
            .expect("lifecycle parsed")
            .base_decay_rates
            .expect("rates parsed");
        assert_eq!(rates.get("episodic").copied(), Some(30.0));
        assert_eq!(rates.get("semantic").copied(), Some(60.0));
    }

    #[test]
    fn config_roundtrips_lifecycle_through_save_load() {
        // Operator hand-edits config.toml, daemon reloads on restart;
        // CLI's `set-llm` also rewrites the file. The roundtrip must
        // preserve [lifecycle] so set-llm doesn't clobber tuning state.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        let mut original = DaemonConfig::default();
        let mut rates = HashMap::new();
        rates.insert("semantic".to_string(), 60.0);
        original.lifecycle = Some(LifecycleOverrides {
            base_decay_rates: Some(rates),
        });

        original.save_to(&path).unwrap();
        let reloaded = DaemonConfig::load_from(&path).unwrap();
        let reloaded_rates = reloaded
            .lifecycle
            .expect("lifecycle survived roundtrip")
            .base_decay_rates
            .expect("rates survived roundtrip");
        assert_eq!(reloaded_rates.get("semantic").copied(), Some(60.0));
    }
}
