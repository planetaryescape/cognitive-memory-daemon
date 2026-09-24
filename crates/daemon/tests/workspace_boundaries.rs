//! Workspace boundary checks for the daemon architecture.
//!
//! Adapted from the mxr/spotuify pattern: the crate graph is part of
//! the design. Client surfaces may depend on `core`, `protocol`, and
//! `client` at runtime, but not on backend crates.

#![allow(clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("daemon crate should live under crates/daemon")
        .to_path_buf()
}

fn read_manifest(rel_path: &str) -> toml::Value {
    let raw = fs::read_to_string(repo_root().join(rel_path)).unwrap_or_else(|err| {
        panic!("failed to read {rel_path}: {err}");
    });
    toml::from_str(&raw).unwrap_or_else(|err| {
        panic!("malformed TOML in {rel_path}: {err}");
    })
}

fn runtime_internal_deps(manifest: &toml::Value) -> BTreeSet<String> {
    manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|deps| deps.keys())
        .filter(|name| name.starts_with("cognitive-memory-"))
        .cloned()
        .collect()
}

#[test]
fn workspace_uses_resolver_2() {
    let manifest = read_manifest("Cargo.toml");
    let resolver = manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("resolver"))
        .and_then(toml::Value::as_str);
    assert_eq!(
        resolver,
        Some("2"),
        "workspace.resolver must stay at 2 so feature unification follows 2021-edition rules"
    );
}

#[test]
fn client_surfaces_do_not_depend_on_backend_crates_at_runtime() {
    let allowed: BTreeSet<String> = [
        "cognitive-memory-core",
        "cognitive-memory-protocol",
        "cognitive-memory-client",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    for manifest_path in [
        "crates/client/Cargo.toml",
        "crates/cli/Cargo.toml",
        "crates/http-bridge/Cargo.toml",
    ] {
        let actual = runtime_internal_deps(&read_manifest(manifest_path));
        let extras: BTreeSet<_> = actual.difference(&allowed).collect();
        assert!(
            extras.is_empty(),
            "{manifest_path} has runtime backend deps: {extras:?}. Client surfaces must go through protocol/client."
        );
    }
}

#[test]
fn cm_daemon_does_not_bind_tcp() {
    let daemon_src = repo_root().join("crates/daemon/src");
    let mut offenders = Vec::new();
    for entry in walk_rs_files(&daemon_src) {
        let body = fs::read_to_string(&entry).unwrap();
        if body.contains("TcpListener") {
            offenders.push(entry);
        }
    }
    assert!(
        offenders.is_empty(),
        "cm-daemon must stay Unix-socket-only; found TcpListener in {offenders:?}"
    );
}

fn walk_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let entries = fs::read_dir(&path).unwrap_or_else(|err| {
            panic!("failed to read {}: {err}", path.display());
        });
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files
}
