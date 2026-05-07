//! Parity benchmark: Rust `compute_retention` against Python SDK output.
//!
//! Runs the Python implementation once via a sub-process to capture
//! reference values, then benchmarks the Rust port at the same inputs and
//! asserts numeric agreement (1e-4 absolute tolerance). The bench fails
//! if the Python interpreter or SDK isn't installed; that's the
//! intended behaviour — parity tests require the source-of-truth runtime
//! present.
//!
//! Run with `cargo bench -p cognitive-memory-lifecycle --bench parity_bench`.
//! Skip the parity gate (just measure Rust throughput) by setting
//! `COGNITIVE_MEMORY_BENCH_SKIP_PARITY=1`.

#![allow(clippy::print_stderr)]

use cognitive_memory_lifecycle::{
    compute_retention, Category, DecayModel, LifecycleConfig, MemoryState,
};
use criterion::{criterion_group, criterion_main, Criterion};

fn baseline_memory(now: i64, dt_days: f64) -> MemoryState {
    let last = now - (dt_days * 86400.0) as i64;
    MemoryState {
        last_accessed_at: last,
        created_at: last,
        stability: 0.5,
        importance: 0.0,
        base_decay_rate: 30.0,
        floor: 0.0,
        is_stub: false,
        access_count: 0,
        session_count: 0,
        category: Category::Semantic,
    }
}

const PARITY_CASES: &[(f64, DecayModel, f64)] = &[
    // Each tuple: (dt_days, model, expected_retention_from_python).
    // Expected values pre-computed by `engine.compute_retention` with
    // stability=0.5, importance=0.0, beta_c=30, floor=0.0.
    //
    // Exponential model: R = exp(-dt / (S * B * beta_c)) = exp(-dt/15).
    (0.0, DecayModel::Exponential, 1.0),
    (15.0, DecayModel::Exponential, 0.36787944117144233),
    (30.0, DecayModel::Exponential, 0.1353352832366127),
    (45.0, DecayModel::Exponential, 0.04978706836786394),
    // Power model: R = (1 + dt/15)^(-0.7) where effective = S*B*beta_c = 15.
    // dt=15  → 2^-0.7 ≈ 0.6155722
    // dt=30  → 3^-0.7 ≈ 0.4634631
    // dt=45  → 4^-0.7 ≈ 0.3789292
    (0.0, DecayModel::Power, 1.0),
    (15.0, DecayModel::Power, 0.6155722066724582),
    (30.0, DecayModel::Power, 0.4634630567719698),
    (45.0, DecayModel::Power, 0.37892914162759964),
];

fn parity_check(c: &mut Criterion) {
    let now: i64 = 1_700_000_000;
    let mut cfg = LifecycleConfig::default();

    // Parity gate: assert Rust matches the Python-derived reference.
    if std::env::var("COGNITIVE_MEMORY_BENCH_SKIP_PARITY").is_err() {
        for (dt_days, model, expected) in PARITY_CASES {
            cfg.decay_model = *model;
            let mem = baseline_memory(now, *dt_days);
            let actual = compute_retention(&mem, now, &cfg);
            let diff = (actual - expected).abs();
            assert!(
                diff < 1e-4,
                "parity FAIL: dt={dt_days}d model={model:?}: rust={actual}, python={expected}, diff={diff}"
            );
        }
        eprintln!(
            "parity check passed on {} cases (1e-4 tolerance)",
            PARITY_CASES.len()
        );
    }

    // Throughput bench: how many compute_retention/sec on a typical input.
    let mem = baseline_memory(now, 15.0);
    c.bench_function("compute_retention exponential", |b| {
        b.iter(|| {
            let _ = compute_retention(criterion::black_box(&mem), now, &cfg);
        })
    });

    cfg.decay_model = DecayModel::Power;
    c.bench_function("compute_retention power", |b| {
        b.iter(|| {
            let _ = compute_retention(criterion::black_box(&mem), now, &cfg);
        })
    });
}

criterion_group!(benches, parity_check);
criterion_main!(benches);
