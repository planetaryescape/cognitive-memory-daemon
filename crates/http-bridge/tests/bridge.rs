//! HTTP bridge tests: loopback enforcement, bearer auth, scope checks,
//! and end-to-end POST /memory/store + /memory/search through to the
//! daemon over a real Unix socket.

#![allow(clippy::panic, clippy::unwrap_used)]

use cognitive_memory_daemon::Daemon;
use cognitive_memory_embeddings::FakeEmbeddingProvider;
use cognitive_memory_http_bridge::{
    enforce_loopback, router, AppState, BridgeError, Scope, TokenStore,
};
use cognitive_memory_store::Store;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn enforce_loopback_accepts_127_0_0_1() {
    let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    assert!(enforce_loopback(addr).is_ok());
}

#[test]
fn enforce_loopback_accepts_localhost_ipv6() {
    let addr: SocketAddr = "[::1]:8080".parse().unwrap();
    assert!(enforce_loopback(addr).is_ok());
}

#[test]
fn enforce_loopback_refuses_zero_zero_zero_zero() {
    let addr: SocketAddr = "0.0.0.0:8080".parse().unwrap();
    assert!(matches!(
        enforce_loopback(addr),
        Err(BridgeError::NonLoopbackBind(_))
    ));
}

#[test]
fn enforce_loopback_refuses_public_ip() {
    let addr: SocketAddr = "8.8.8.8:80".parse().unwrap();
    assert!(matches!(
        enforce_loopback(addr),
        Err(BridgeError::NonLoopbackBind(_))
    ));
}

#[test]
fn token_store_validates_minted_tokens_only() {
    let store = TokenStore::new(b"test-salt".to_vec());
    store.mint(b"valid-token", "alice", Scope::Write);

    let validated = store.validate(b"valid-token");
    assert!(validated.is_some());
    let (uid, scope) = validated.unwrap();
    assert_eq!(uid, "alice");
    assert_eq!(scope, Scope::Write);

    assert!(
        store.validate(b"unknown-token").is_none(),
        "unknown token must reject"
    );
}

async fn boot_daemon_and_bridge() -> (
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
    PathBuf,
    SocketAddr,
    String,
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

    let daemon_handle = tokio::spawn(async move {
        daemon.serve().await.expect("daemon");
    });

    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(socket_path.exists(), "daemon never bound socket");

    // Pick a free loopback port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bridge_addr = listener.local_addr().unwrap();
    drop(listener);

    let token = "test-bearer-token-12345".to_string();
    let tokens = TokenStore::new(b"test-salt".to_vec());
    tokens.mint(token.as_bytes(), "alice", Scope::Write);

    let state = AppState {
        socket_path: socket_path.clone(),
        tokens,
    };
    let app = router(state);

    let bridge_handle = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(bridge_addr).await.unwrap();
        axum::serve(listener, app).await.unwrap();
    });

    // Wait for bridge to bind.
    for _ in 0..100 {
        if std::net::TcpStream::connect(bridge_addr).is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    (
        daemon_handle,
        bridge_handle,
        socket_path,
        bridge_addr,
        token,
        shutdown,
        tmp,
    )
}

#[tokio::test]
async fn http_post_memory_store_then_search_round_trip() {
    let (daemon_h, bridge_h, _socket, addr, token, shutdown, _tmp) = boot_daemon_and_bridge().await;

    let client = reqwest::Client::new();

    // Store a memory.
    let store_resp = client
        .post(format!("http://{addr}/memory/store"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "content": "HTTP path works."
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(store_resp.status(), 200);
    let store_body: serde_json::Value = store_resp.json().await.unwrap();
    assert_eq!(store_body["ok"], true);

    // Search.
    let search_resp = client
        .post(format!("http://{addr}/memory/search"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "query": "HTTP path works.",
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(search_resp.status(), 200);
    let search_body: serde_json::Value = search_resp.json().await.unwrap();
    let results = &search_body["data"]["results"];
    assert!(
        !results.as_array().unwrap().is_empty(),
        "search returned no results: {search_body}"
    );

    let _ = shutdown.send(());
    let _ = daemon_h.await;
    bridge_h.abort();
}

#[tokio::test]
async fn http_request_without_bearer_returns_401() {
    let (daemon_h, bridge_h, _socket, addr, _token, shutdown, _tmp) =
        boot_daemon_and_bridge().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{addr}/memory/search"))
        .json(&serde_json::json!({"query":"x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let _ = shutdown.send(());
    let _ = daemon_h.await;
    bridge_h.abort();
}

#[tokio::test]
async fn http_request_with_bad_bearer_returns_401() {
    let (daemon_h, bridge_h, _socket, addr, _token, shutdown, _tmp) =
        boot_daemon_and_bridge().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{addr}/memory/search"))
        .bearer_auth("definitely-not-a-real-token")
        .json(&serde_json::json!({"query":"x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let _ = shutdown.send(());
    let _ = daemon_h.await;
    bridge_h.abort();
}
