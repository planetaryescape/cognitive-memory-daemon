//! Lifecycle: decay, reinforce, consolidate, expire, promote.
//!
//! Algorithms ported from `cognitive-memory-sdk/sdks/python/src/cognitive_memory/engine.py`.
//! The Python implementation is the algorithmic source of truth; this Rust
//! port produces numerically equivalent results within tolerance.
//!
//! Per `docs/developer/test-discipline.md` §9 ("Special discipline: parity
//! tests"), expected values in tests are taken from the Python formula
//! (computed in comments), not from the Rust implementation. Tolerance:
//! `1e-4` absolute on f64 retention/score values.

use serde::{Deserialize, Serialize};

/// Decay model selection. Mirrors the `decay_model` config field in the
/// Python SDK.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecayModel {
    #[default]
    Exponential,
    Power,
}

/// Lifecycle configuration: tunable parameters for decay/scoring.
///
/// Defaults match the v6 paper / Python SDK defaults. Override at the
/// daemon level (config file → DI into the lifecycle layer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleConfig {
    pub decay_model: DecayModel,
    /// Exponent on the retention factor in the score formula (paper Eq. 3).
    pub retrieval_score_exponent: f64,
    /// Power-decay shape parameter (only used when `decay_model = Power`).
    pub power_decay_gamma: f64,
    /// Direct-retrieval stability boost amount.
    pub direct_boost: f64,
    /// Associative-retrieval stability boost amount.
    pub associative_boost: f64,
    /// Spaced repetition interval in days (boost factor scales as `dt/interval`).
    pub spaced_rep_interval_days: f64,
    /// Cap on the spaced-repetition multiplier.
    pub max_spaced_rep_multiplier: f64,
    /// Core-promotion thresholds.
    pub core_access_threshold: u64,
    pub core_stability_threshold: f64,
    pub core_session_threshold: usize,
    /// Time constant for association edge decay (paper Eq 10).
    /// SDK default: 90 days. Mirrors
    /// `association_decay_constant_days` in types.py.
    pub association_decay_constant_days: f64,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            decay_model: DecayModel::Exponential,
            // α=0.3 matches the Python SDK default (types.py:96). Lower
            // alpha means decay weighs *less* in scoring — old but
            // exact-match memories still rank well.
            retrieval_score_exponent: 0.3,
            power_decay_gamma: 0.7,
            direct_boost: 0.1,
            associative_boost: 0.03,
            spaced_rep_interval_days: 7.0,
            max_spaced_rep_multiplier: 2.0,
            core_access_threshold: 10,
            core_stability_threshold: 0.85,
            core_session_threshold: 3,
            association_decay_constant_days: 90.0,
        }
    }
}

/// The mutable lifecycle state of a single memory. The daemon stores
/// these fields denormalised across `memories` columns; this struct is the
/// in-memory view the lifecycle functions operate on.
#[derive(Debug, Clone)]
pub struct MemoryState {
    /// Last-access timestamp in unix seconds. Falls back to `created_at`
    /// when the memory has never been retrieved.
    pub last_accessed_at: i64,
    /// Memory creation timestamp in unix seconds.
    pub created_at: i64,
    /// Stability factor. Bounded to `[0.0, 1.0]`; higher = decays slower.
    pub stability: f64,
    /// Importance factor, `[0.0, 1.0]`.
    pub importance: f64,
    /// Base decay rate (`beta_c`). `f64::INFINITY` means "no decay" (e.g.
    /// procedural memories).
    pub base_decay_rate: f64,
    /// Retention floor — `compute_retention` never returns below this.
    pub floor: f64,
    /// Tombstone for cold-storage stubs; stub memories return retention 0.
    pub is_stub: bool,
    /// Retrieval counter.
    pub access_count: u64,
    /// Distinct sessions in which this memory was retrieved (for core promotion).
    pub session_count: usize,
    /// Current category — used to detect "already core" in promotion check.
    pub category: Category,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Episodic,
    Semantic,
    Procedural,
    Core,
}

/// Parse a wire-form category string into the typed enum. Unknown
/// strings fall back to Semantic, matching the Python SDK default.
pub fn parse_category(s: &str) -> Category {
    match s {
        "episodic" => Category::Episodic,
        "semantic" => Category::Semantic,
        "procedural" => Category::Procedural,
        "core" => Category::Core,
        _ => Category::Semantic,
    }
}

/// Apply read-side decay to an association edge weight (paper Eq 10):
/// `w' = w * exp(-Δt_days / τ)`. SDK default `τ = 90` days
/// (`association_decay_constant_days` in cognitive_memory/types.py).
///
/// `last_co_retrieval` and `now` are unix seconds. Negative Δt is
/// clamped to 0 (defensive: clock skew shouldn't increase weight).
/// Mirrors `decay_association` in engine.py:217-229.
pub fn decay_association_weight(
    stored_weight: f64,
    last_co_retrieval: i64,
    now: i64,
    tau_days: f64,
) -> f64 {
    let dt_days = ((now - last_co_retrieval).max(0) as f64) / 86_400.0;
    stored_weight * (-dt_days / tau_days).exp()
}

/// Initial stability for a freshly-created memory: `0.1 + 0.3 * importance`.
/// Range: importance ∈ [0, 1] → stability ∈ [0.1, 0.4]. Mirrors the SDK
/// constructor formula in `cognitive_memory/core.py:126` and
/// `cognitive_memory/extraction.py:216`.
pub fn stability_from_importance(importance: f64) -> f64 {
    0.1 + 0.3 * importance
}

/// Strip the `_perspective_<...>` suffix from a session id so multiple
/// per-perspective retrievals of the same conversation count as one
/// session toward core promotion. Mirrors `_session_roots` in
/// `cognitive_memory/core.py:44-47` (`re.sub(r"_perspective_.*$", "", sid)`).
pub fn session_root(sid: &str) -> &str {
    match sid.find("_perspective_") {
        Some(idx) => &sid[..idx],
        None => sid,
    }
}

/// Base decay rate in days for a given category — Table 2 in the paper.
/// Mirrors `BASE_DECAY_RATES` in `sdks/python/src/cognitive_memory/types.py`.
/// `f64::INFINITY` means "never decays".
pub fn base_decay_rate_for_category(category: &str) -> f64 {
    match category {
        "episodic" => 45.0,
        "semantic" => 120.0,
        "core" => 120.0,
        "procedural" => f64::INFINITY,
        _ => 120.0, // unknown → semantic default
    }
}

/// `R(m) = max(floor, exp(-dt / (S * B * beta_c)))` (Equation 1 in the paper).
///
/// Power-decay variant: `R(m) = max(floor, (1 + dt / (S*B*beta_c))^(-gamma))`.
///
/// `now` is unix seconds. `dt_days = max(0, (now - last_accessed_at) / 86400)`.
pub fn compute_retention(memory: &MemoryState, now: i64, config: &LifecycleConfig) -> f64 {
    if memory.is_stub {
        return 0.0;
    }
    if memory.base_decay_rate.is_infinite() {
        return 1.0;
    }

    let last = memory.last_accessed_at;
    let dt_days = ((now - last).max(0) as f64) / 86400.0;

    let s = memory.stability.max(0.01);
    let b = (1.0 + memory.importance * 2.0).min(3.0);
    let effective_rate = s * b * memory.base_decay_rate;

    let raw = match config.decay_model {
        DecayModel::Exponential => (-dt_days / effective_rate).exp(),
        DecayModel::Power => (1.0 + dt_days / effective_rate).powf(-config.power_decay_gamma),
    };

    raw.max(memory.floor)
}

/// `score(m, q) = relevance(m, q) * R(m)^alpha` (Equation 3 in the paper).
pub fn score_memory(
    memory: &MemoryState,
    relevance: f64,
    now: i64,
    config: &LifecycleConfig,
) -> f64 {
    let retention = compute_retention(memory, now, config);
    relevance * retention.powf(config.retrieval_score_exponent)
}

/// Spaced-repetition multiplier: `min(max_mult, dt_days / interval_days)`.
fn spaced_rep_factor(memory: &MemoryState, now: i64, config: &LifecycleConfig) -> f64 {
    let dt_days = ((now - memory.last_accessed_at).max(0) as f64) / 86400.0;
    config
        .max_spaced_rep_multiplier
        .min(dt_days / config.spaced_rep_interval_days)
}

/// Direct retrieval boost (Equations 4-5). Mutates the memory in place.
pub fn apply_direct_boost(memory: &mut MemoryState, now: i64, config: &LifecycleConfig) {
    let factor = spaced_rep_factor(memory, now, config);
    memory.stability = (memory.stability + config.direct_boost * factor).min(1.0);
    memory.access_count += 1;
    memory.last_accessed_at = now;
}

/// Associative retrieval boost (Equations 6-7). Mutates in place.
pub fn apply_associative_boost(memory: &mut MemoryState, now: i64, config: &LifecycleConfig) {
    let factor = spaced_rep_factor(memory, now, config);
    memory.stability = (memory.stability + config.associative_boost * factor).min(1.0);
    memory.access_count += 1;
    memory.last_accessed_at = now;
}

/// Promote a memory to `Core` if it meets all three thresholds. Returns
/// `true` if a promotion occurred.
pub fn check_core_promotion(memory: &mut MemoryState, config: &LifecycleConfig) -> bool {
    if memory.category == Category::Core {
        return false;
    }
    if memory.access_count >= config.core_access_threshold
        && memory.stability >= config.core_stability_threshold
        && memory.session_count >= config.core_session_threshold
    {
        memory.category = Category::Core;
        return true;
    }
    false
}
