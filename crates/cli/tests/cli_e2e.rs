//! End-to-end CLI tests: spin up a real daemon, invoke `cm` against it.
//!
//! These tests confirm the CLI surface (subcommands, --json, exit codes)
//! against a live daemon — the same wiring the user will see at install.

#![allow(clippy::panic, clippy::unwrap_used)]

use assert_cmd::Command;
use cognitive_memory_daemon::Daemon;
use cognitive_memory_embeddings::FakeEmbeddingProvider;
use cognitive_memory_store::Store;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

async fn boot_daemon() -> (
    tokio::task::JoinHandle<()>,
    PathBuf,
    tokio::sync::broadcast::Sender<()>,
    TempDir,
) {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("cm.sock");
    let db_path = tmp.path().join("data.db");

    let store = Store::open(&db_path).await.unwrap();
    let embeddings = Arc::new(FakeEmbeddingProvider::new("local", "fake-16", 16));
    let daemon = Daemon::new(store, embeddings, socket_path.clone());
    let shutdown = daemon.shutdown_handle();

    let handle = tokio::spawn(async move {
        daemon.serve().await.expect("daemon serve");
    });

    for _ in 0..100 {
        if socket_path.exists() {
            return (handle, socket_path, shutdown, tmp);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon never bound the socket");
}

#[tokio::test]
async fn cm_status_against_running_daemon() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;

    let socket_str = socket.to_str().unwrap().to_string();
    let assert = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("cm")
            .unwrap()
            .arg("--socket")
            .arg(&socket_str)
            .arg("status")
            .assert()
            .success()
            .stdout(predicates::str::contains("memories: 0"))
    })
    .await
    .unwrap();
    drop(assert);

    let _ = shutdown.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn cm_store_then_search_round_trip() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let socket_str = socket.to_str().unwrap().to_string();

    let socket_for_store = socket_str.clone();
    let stored_stdout = tokio::task::spawn_blocking(move || {
        let output = Command::cargo_bin("cm")
            .unwrap()
            .arg("--socket")
            .arg(&socket_for_store)
            .arg("store")
            .arg("Tea over coffee.")
            .output()
            .unwrap();
        assert!(output.status.success(), "store failed: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    })
    .await
    .unwrap();

    assert!(
        stored_stdout.starts_with("stored: mem_"),
        "unexpected store output: {stored_stdout:?}"
    );

    let socket_for_search = socket_str;
    let search_stdout = tokio::task::spawn_blocking(move || {
        let output = Command::cargo_bin("cm")
            .unwrap()
            .arg("--socket")
            .arg(&socket_for_search)
            .arg("search")
            .arg("Tea over coffee.")
            .output()
            .unwrap();
        assert!(output.status.success(), "search failed: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    })
    .await
    .unwrap();

    assert!(
        search_stdout.contains("Tea over coffee."),
        "search did not return the stored memory: {search_stdout:?}"
    );

    let _ = shutdown.send(());
    let _ = handle.await;
}

/// `--no-spawn` disables auto-spawn: with no daemon at the socket, `cm`
/// fails fast instead of trying to fork `cm-daemon`. This is what tests
/// and CI rely on to avoid accidentally launching a real daemon.
#[tokio::test]
async fn cm_no_spawn_fails_when_socket_missing() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("never_exists.sock");
    let socket_str = socket.to_str().unwrap().to_string();

    let exit = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("cm")
            .unwrap()
            .arg("--socket")
            .arg(&socket_str)
            .arg("--no-spawn")
            .arg("status")
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(
        !exit.status.success(),
        "--no-spawn must fail when no daemon"
    );
    let stderr = String::from_utf8_lossy(&exit.stderr);
    assert!(
        stderr.contains("connect to daemon"),
        "expected connect error in stderr, got: {stderr}"
    );
}

/// Auto-spawn: with no daemon running, `cm` forks `cm-daemon` and the
/// command succeeds. The test pins fake embeddings via env so CI never
/// downloads the local model.
#[tokio::test]
async fn cm_auto_spawns_daemon_when_socket_missing() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("cm.sock");
    let daemon_bin = assert_cmd::cargo::cargo_bin("cm-daemon");
    let tmp_path = tmp.path().to_path_buf();

    let socket_str = socket.to_str().unwrap().to_string();
    let daemon_bin_str = daemon_bin.to_str().unwrap().to_string();
    let tmp_path_for_start = tmp_path.clone();
    let stdout = tokio::task::spawn_blocking(move || {
        let output = Command::cargo_bin("cm")
            .unwrap()
            .arg("--socket")
            .arg(&socket_str)
            .env("COGNITIVE_MEMORY_DAEMON_BIN", &daemon_bin_str)
            .env("COGNITIVE_MEMORY_INSTANCE", "cm-cli-autospawn-test")
            .env("COGNITIVE_MEMORY_EMBEDDINGS", "fake")
            .env(
                "COGNITIVE_MEMORY_CONFIG_DIR",
                tmp_path_for_start.join("config"),
            )
            .env("COGNITIVE_MEMORY_DATA_DIR", tmp_path_for_start.join("data"))
            .env(
                "COGNITIVE_MEMORY_RUNTIME_DIR",
                tmp_path_for_start.join("runtime"),
            )
            .env(
                "COGNITIVE_MEMORY_CACHE_DIR",
                tmp_path_for_start.join("cache"),
            )
            .env("COGNITIVE_MEMORY_LOG_DIR", tmp_path_for_start.join("logs"))
            .arg("status")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "cm status (auto-spawn) failed: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    })
    .await
    .unwrap();

    assert!(stdout.contains("memories: 0"));

    let socket_for_stop = socket.to_str().unwrap().to_string();
    let daemon_bin_stop = daemon_bin.to_str().unwrap().to_string();
    tokio::task::spawn_blocking(move || {
        let output = Command::cargo_bin("cm")
            .unwrap()
            .arg("--socket")
            .arg(&socket_for_stop)
            .env("COGNITIVE_MEMORY_DAEMON_BIN", &daemon_bin_stop)
            .env("COGNITIVE_MEMORY_INSTANCE", "cm-cli-autospawn-test")
            .env("COGNITIVE_MEMORY_EMBEDDINGS", "fake")
            .env("COGNITIVE_MEMORY_CONFIG_DIR", tmp_path.join("config"))
            .env("COGNITIVE_MEMORY_DATA_DIR", tmp_path.join("data"))
            .env("COGNITIVE_MEMORY_RUNTIME_DIR", tmp_path.join("runtime"))
            .env("COGNITIVE_MEMORY_CACHE_DIR", tmp_path.join("cache"))
            .env("COGNITIVE_MEMORY_LOG_DIR", tmp_path.join("logs"))
            .arg("daemon")
            .arg("stop")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stop failed: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cm_search_with_json_emits_json() {
    let (handle, socket, shutdown, _tmp) = boot_daemon().await;
    let socket_str = socket.to_str().unwrap().to_string();

    let stdout = tokio::task::spawn_blocking(move || {
        let output = Command::cargo_bin("cm")
            .unwrap()
            .arg("--socket")
            .arg(&socket_str)
            .arg("--json")
            .arg("search")
            .arg("anything")
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    })
    .await
    .unwrap();

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("JSON output");
    assert_eq!(parsed["kind"], "MemorySearchResults");
    assert!(parsed["results"].is_array());

    let _ = shutdown.send(());
    let _ = handle.await;
}
