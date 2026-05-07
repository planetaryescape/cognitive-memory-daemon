//! Parity tests against the Python SDK's `engine.compute_retention` and
//! `engine.score_memory` formulas. Expected values are computed in Python
//! (`engine.py:78`, `engine.py:114`) and pinned here as constants.
//!
//! Tolerance: `1e-4` absolute on retention/score floats (per build plan).
//! Categorical decisions (e.g. `check_core_promotion`) require exact match.

#![allow(clippy::panic, clippy::unwrap_used)]

use cognitive_memory_lifecycle::{
    apply_direct_boost, check_core_promotion, compute_retention, score_memory, session_root,
    stability_from_importance, Category, DecayModel, LifecycleConfig, MemoryState,
};
use pretty_assertions::assert_eq;

const TOL: f64 = 1e-4;

fn approx_eq(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() < tol
}

fn baseline_memory(now: i64, dt_days_ago: f64) -> MemoryState {
    let last = now - (dt_days_ago * 86400.0) as i64;
    MemoryState {
        last_accessed_at: last,
        created_at: last,
        stability: 0.5,
        importance: 0.0,
        base_decay_rate: 30.0, // 30-day baseline beta_c
        floor: 0.0,
        is_stub: false,
        access_count: 0,
        session_count: 0,
        category: Category::Semantic,
    }
}

#[test]
fn retention_at_zero_dt_is_one() {
    // dt = 0, exp(0) = 1.0
    let now: i64 = 1_700_000_000;
    let mem = baseline_memory(now, 0.0);
    let r = compute_retention(&mem, now, &LifecycleConfig::default());
    assert!(approx_eq(r, 1.0, TOL), "expected 1.0, got {r}");
}

#[test]
fn retention_exponential_decay_at_known_dt() {
    // From Python: dt=15 days, S=0.5, B=1.0 (importance=0), beta_c=30
    // effective = 0.5 * 1.0 * 30 = 15
    // exp(-15/15) = exp(-1) ≈ 0.36787944117
    let now: i64 = 1_700_000_000;
    let mem = baseline_memory(now, 15.0);
    let r = compute_retention(&mem, now, &LifecycleConfig::default());
    let expected = (-1.0_f64).exp();
    assert!(
        approx_eq(r, expected, TOL),
        "exponential decay: expected {expected}, got {r}"
    );
}

#[test]
fn retention_power_decay_at_known_dt() {
    // From Python: dt=15 days, effective=15, gamma=0.7
    // (1 + 15/15)^(-0.7) = 2^(-0.7) ≈ 0.61557220...
    let now: i64 = 1_700_000_000;
    let mem = baseline_memory(now, 15.0);
    let cfg = LifecycleConfig {
        decay_model: DecayModel::Power,
        ..LifecycleConfig::default()
    };
    let r = compute_retention(&mem, now, &cfg);
    let expected = 2.0_f64.powf(-0.7);
    assert!(
        approx_eq(r, expected, TOL),
        "power decay: expected {expected}, got {r}"
    );
}

#[test]
fn retention_floor_clamps_minimum() {
    let now: i64 = 1_700_000_000;
    let mut mem = baseline_memory(now, 365.0); // a year — exp decay << floor
    mem.floor = 0.6;
    let r = compute_retention(&mem, now, &LifecycleConfig::default());
    assert!(
        approx_eq(r, 0.6, TOL),
        "floor must clamp retention; got {r}"
    );
}

#[test]
fn retention_stub_is_zero() {
    let now: i64 = 1_700_000_000;
    let mut mem = baseline_memory(now, 0.0);
    mem.is_stub = true;
    let r = compute_retention(&mem, now, &LifecycleConfig::default());
    assert_eq!(r, 0.0, "stub memories must return 0.0");
}

#[test]
fn retention_infinite_decay_rate_means_no_decay() {
    let now: i64 = 1_700_000_000;
    let mut mem = baseline_memory(now, 365.0);
    mem.base_decay_rate = f64::INFINITY;
    let r = compute_retention(&mem, now, &LifecycleConfig::default());
    assert_eq!(
        r, 1.0,
        "procedural-style memories (infinite beta_c) must not decay"
    );
}

#[test]
fn score_combines_relevance_and_retention_with_alpha() {
    let now: i64 = 1_700_000_000;
    let mem = baseline_memory(now, 0.0); // R = 1.0
    let cfg = LifecycleConfig::default();
    let alpha = cfg.retrieval_score_exponent;
    let s = score_memory(&mem, 0.7, now, &cfg);
    // R^α = 1.0 regardless of α; score = relevance.
    assert!(approx_eq(s, 0.7, TOL), "got {s}");

    // dt=15 days → R = exp(-1) ≈ 0.367879. Score uses the lifecycle
    // config's α (0.3 per SDK parity).
    let mem2 = baseline_memory(now, 15.0);
    let s2 = score_memory(&mem2, 0.7, now, &cfg);
    let expected = 0.7_f64 * (-1.0_f64).exp().powf(alpha);
    assert!(
        approx_eq(s2, expected, TOL),
        "expected {expected}, got {s2}"
    );
}

#[test]
fn direct_boost_increases_stability_and_access_count() {
    let now: i64 = 1_700_000_000;
    let mut mem = baseline_memory(now, 14.0); // 14 days = 2-week gap
    let cfg = LifecycleConfig::default();

    let before = mem.stability;
    let access_before = mem.access_count;
    apply_direct_boost(&mut mem, now, &cfg);

    // factor = min(2.0, 14/7) = 2.0; boost = 0.1 * 2.0 = 0.2
    // 0.5 + 0.2 = 0.7
    assert!(
        approx_eq(mem.stability, before + 0.2, TOL),
        "stability after boost: expected {}, got {}",
        before + 0.2,
        mem.stability
    );
    assert_eq!(mem.access_count, access_before + 1);
    assert_eq!(mem.last_accessed_at, now);
}

#[test]
fn direct_boost_caps_stability_at_one() {
    let now: i64 = 1_700_000_000;
    let mut mem = baseline_memory(now, 30.0);
    mem.stability = 0.95;

    apply_direct_boost(&mut mem, now, &LifecycleConfig::default());
    assert!(
        mem.stability <= 1.0,
        "stability must cap at 1.0; got {}",
        mem.stability
    );
}

#[test]
fn core_promotion_requires_all_three_thresholds() {
    let now: i64 = 1_700_000_000;
    let mut mem = baseline_memory(now, 0.0);
    mem.access_count = 9; // just below threshold (10)
    mem.stability = 0.9;
    mem.session_count = 5;
    let cfg = LifecycleConfig::default();

    assert!(
        !check_core_promotion(&mut mem, &cfg),
        "below access threshold should not promote"
    );
    assert_eq!(mem.category, Category::Semantic);

    mem.access_count = 10;
    assert!(
        check_core_promotion(&mut mem, &cfg),
        "all thresholds met should promote"
    );
    assert_eq!(mem.category, Category::Core);

    // Already core: no further promotion.
    assert!(!check_core_promotion(&mut mem, &cfg));
}

// ===========================================================================
// session_root — strips `_perspective_*` suffix so multi-perspective
// retrievals of the same conversation count as one session toward core
// promotion. Mirrors `_session_roots` in cognitive_memory/core.py:44-47.
// ===========================================================================

#[test]
fn session_root_strips_perspective_suffix() {
    // SDK: core.py:44-47 — `re.sub(r"_perspective_.*$", "", sid)`.
    // The full sid `s_01ABC_perspective_user_view` reduces to its root
    // `s_01ABC`. The substring after `_perspective_` is dropped.
    assert_eq!(session_root("s_01ABC_perspective_user_view"), "s_01ABC");
}

#[test]
fn session_root_passes_through_when_no_perspective_suffix() {
    // SDK: core.py:44-47 — when no `_perspective_` substring exists,
    // the regex sub is a no-op and the input is returned unchanged.
    assert_eq!(session_root("s_01ABC"), "s_01ABC");
}

#[test]
fn session_root_handles_empty_string() {
    // SDK: core.py:44-47 — `re.sub(...)` on `""` returns `""`. Daemon
    // must mirror so an empty session id (defensive caller) doesn't
    // crash the dedup path.
    assert_eq!(session_root(""), "");
}

#[test]
fn session_root_strips_at_first_perspective_marker() {
    // SDK regex `_perspective_.*$` is greedy on the suffix, anchored
    // at end. Matches the FIRST occurrence and strips through end.
    // `s_X_perspective_a_perspective_b` → `s_X` (everything from the
    // first `_perspective_` onward is dropped).
    assert_eq!(session_root("s_X_perspective_a_perspective_b"), "s_X");
}

// ===========================================================================
// stability_from_importance — fresh memories start with stability per the
// SDK formula `0.1 + 0.3 * importance`, NOT the placeholder 0.5 the
// daemon currently hardcodes. Mirrors core.py:126 / extraction.py:216.
// ===========================================================================

#[test]
fn stability_baseline_at_zero_importance() {
    // SDK: core.py:126 — `stability=0.1 + (importance * 0.3)`.
    // importance=0.0 → 0.1 + 0.0 = 0.1.
    assert!(approx_eq(stability_from_importance(0.0), 0.1, TOL));
}

#[test]
fn stability_baseline_at_mid_importance() {
    // SDK: core.py:126 — importance=0.7 → 0.1 + 0.7*0.3 = 0.31.
    // Non-trivial value catches a drop-the-bias mutation (`0.3*imp`)
    // or a swap of constants (`0.3 + 0.1*imp`).
    assert!(approx_eq(stability_from_importance(0.7), 0.31, TOL));
}

#[test]
fn stability_baseline_at_max_importance() {
    // SDK: core.py:126 — importance=1.0 → 0.1 + 1.0*0.3 = 0.4.
    // Boundary at importance ceiling.
    assert!(approx_eq(stability_from_importance(1.0), 0.4, TOL));
}
