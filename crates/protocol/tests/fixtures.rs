//! Golden-fixture tests for the IPC protocol.
//!
//! Each subdirectory under `tests/fixtures/` corresponds to a payload bucket
//! (`requests/`, `responses/`, `events/`). Each `.json` file is one fixture
//! describing a complete `IpcMessage` on the wire.
//!
//! The tests below walk each subdirectory and assert that every fixture:
//!   1. Decodes successfully into the typed `IpcMessage`.
//!   2. Re-encodes to JSON.
//!   3. The re-encoded JSON, parsed as a `serde_json::Value`, equals the
//!      original parsed value (semantic equality — ignores whitespace and
//!      key order, but catches any field rename, drop, or type change).
//!
//! Adding a new request/response/event variant in the protocol crate
//! requires adding at least one fixture in the corresponding subdirectory in
//! the same PR (per `AGENTS.md` §9). The tests below pick it up automatically.
//!
//! See `docs/developer/test-discipline.md` §10 for the discipline behind
//! contract tests.

#![allow(clippy::panic, clippy::unwrap_used)]

use cognitive_memory_protocol::IpcMessage;
use std::fs;
use std::path::{Path, PathBuf};

/// Behaviour 6, 7, 8: every fixture in the corresponding directory
/// round-trips between JSON and `IpcMessage` without semantic drift.
#[test]
fn every_request_fixture_round_trips() {
    walk_and_assert_round_trip(&fixture_dir("requests"));
}

#[test]
fn every_response_fixture_round_trips() {
    walk_and_assert_round_trip(&fixture_dir("responses"));
}

#[test]
fn every_event_fixture_round_trips() {
    walk_and_assert_round_trip(&fixture_dir("events"));
}

fn fixture_dir(bucket: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(bucket)
}

fn walk_and_assert_round_trip(dir: &Path) {
    let entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read fixture dir {}: {e}", dir.display()))
        .filter_map(|res| res.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();

    assert!(
        !entries.is_empty(),
        "no fixtures found in {} — at least one fixture per bucket is required to keep this test from being vacuous",
        dir.display()
    );

    for path in entries {
        assert_round_trip(&path);
    }
}

fn assert_round_trip(fixture_path: &Path) {
    let raw = fs::read_to_string(fixture_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", fixture_path.display()));

    let original_value: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("parse JSON in {}: {e}", fixture_path.display()));

    let typed: IpcMessage = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("decode IpcMessage from {}: {e}", fixture_path.display()));

    let re_encoded = serde_json::to_string(&typed)
        .unwrap_or_else(|e| panic!("re-encode IpcMessage from {}: {e}", fixture_path.display()));

    let re_encoded_value: serde_json::Value = serde_json::from_str(&re_encoded)
        .unwrap_or_else(|e| panic!("parse re-encoded JSON from {}: {e}", fixture_path.display()));

    pretty_assertions::assert_eq!(
        re_encoded_value,
        original_value,
        "round-trip drift in fixture {}",
        fixture_path.display()
    );
}
