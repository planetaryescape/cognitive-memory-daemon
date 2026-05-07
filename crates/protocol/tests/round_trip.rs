//! Round-trip tests for IPC protocol types.
//!
//! Each test asserts that a value of a protocol type encodes to JSON and
//! decodes back to an equal value. These are contract tests — they pin the
//! wire format we depend on, not implementation details. See
//! `docs/developer/test-discipline.md` §10 ("Special discipline: vendored
//! code") for the discipline that applies to protocol round-trips.

#![allow(clippy::panic, clippy::unwrap_used)]

use bytes::BytesMut;
use cognitive_memory_protocol::{DiagnosticsRequest, IpcCodec, IpcMessage, IpcPayload, Request};
use pretty_assertions::assert_eq;
use tokio_util::codec::{Decoder, Encoder};

/// Behaviour 1 (per build plan §"Phase 0 — workspace + protocol crate"):
/// `IpcMessage` round-trips through JSON: encode then decode equals the
/// original.
///
/// Spec: `PROTOCOL.md` §3 (envelope shape) and §5.3.1 (`Diagnostics::Status`
/// example).
#[test]
fn ipc_message_round_trips_through_json() {
    let original = IpcMessage {
        id: 42,
        payload: IpcPayload::Request(Request::Diagnostics(DiagnosticsRequest::Status)),
    };

    let json = serde_json::to_string(&original).expect("encode IpcMessage to JSON");
    let decoded: IpcMessage = serde_json::from_str(&json).expect("decode IpcMessage from JSON");

    assert_eq!(decoded, original);
}

/// Behaviour 2: `IpcMessage` round-trips through the length-delimited codec
/// on an in-memory buffer.
///
/// Spec: `PROTOCOL.md` §1 (4-byte big-endian length prefix + JSON payload,
/// max frame 16 MiB).
#[test]
fn ipc_message_round_trips_through_length_delimited_codec() {
    let original = IpcMessage {
        id: 7,
        payload: IpcPayload::Request(Request::Diagnostics(DiagnosticsRequest::Status)),
    };

    let mut codec = IpcCodec::new();
    let mut buf = BytesMut::new();

    codec
        .encode(original.clone(), &mut buf)
        .expect("encode IpcMessage into buffer");

    let decoded = codec
        .decode(&mut buf)
        .expect("decode without error")
        .expect("a complete frame in the buffer");

    assert_eq!(decoded, original);
}

/// Behaviour 3: the codec rejects frames larger than 16 MiB.
///
/// Spec: `PROTOCOL.md` §1 ("Maximum frame: 16,777,216 bytes (16 MiB)").
///
/// We do not allocate 16 MiB of real payload to test this — we craft a length
/// prefix that claims an oversized frame and confirm decode refuses it.
#[test]
fn codec_rejects_frames_larger_than_16_mib() {
    let mut codec = IpcCodec::new();
    let mut buf = BytesMut::new();

    let oversized_len: u32 = 16 * 1024 * 1024 + 1;
    buf.extend_from_slice(&oversized_len.to_be_bytes());

    let err = codec
        .decode(&mut buf)
        .expect_err("oversized frame should be rejected");

    // tokio-util surfaces frame-too-large as io::ErrorKind::InvalidData.
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

/// Behaviour 9: an unknown bucket name in the wire JSON decodes to a
/// catch-all `Request::UnknownBucket` variant rather than erroring.
///
/// This is forward-compat: a v2 daemon responding to a v1 client (or vice
/// versa) may include bucket names the receiver doesn't know. Per
/// `PROTOCOL.md` §8, additive changes (new buckets, new ops) are not
/// version-bumping; old code must tolerate them. The daemon's handler
/// returns `Response::Err { kind: InvalidPayload }` for unknown buckets,
/// but the *decoder* must not panic or fail at the wire-format layer.
#[test]
fn unknown_bucket_decodes_to_catch_all_variant() {
    let raw = r#"{
        "id": 99,
        "payload": {
            "kind": "Request",
            "body": {
                "bucket": "FutureFeature",
                "op": "DoSomething",
                "extra_field": 42
            }
        }
    }"#;

    let decoded: IpcMessage = serde_json::from_str(raw)
        .expect("unknown bucket must decode without error (forward-compat)");

    match decoded.payload {
        IpcPayload::Request(Request::UnknownBucket) => {}
        other => panic!("expected Request::UnknownBucket, got {other:?}"),
    }
}

/// Behaviour 5: protocol-version negotiation accepts the matching version
/// and rejects mismatched versions with `ProtocolMismatch`.
///
/// Spec: `PROTOCOL.md` §2 (Connection setup; `Hello { protocol_version }` is
/// rejected on mismatch with `Error { kind: "ProtocolMismatch", ... }`).
#[test]
fn validate_protocol_version_accepts_matching_and_rejects_mismatched() {
    use cognitive_memory_protocol::{
        validate_protocol_version, ProtocolMismatch, IPC_PROTOCOL_VERSION,
    };

    assert!(validate_protocol_version(IPC_PROTOCOL_VERSION).is_ok());

    let too_low = validate_protocol_version(0).expect_err("0 must be rejected");
    assert_eq!(
        too_low,
        ProtocolMismatch {
            client_version: 0,
            daemon_version: IPC_PROTOCOL_VERSION,
        }
    );

    let too_high = validate_protocol_version(2).expect_err("2 must be rejected");
    assert_eq!(
        too_high,
        ProtocolMismatch {
            client_version: 2,
            daemon_version: IPC_PROTOCOL_VERSION,
        }
    );
}

/// Behaviour 4: `IPC_PROTOCOL_VERSION` is `1`.
///
/// This is a literal-value-pin test, not a behaviour-coverage test. Its job
/// is to fail if anyone accidentally bumps the constant: a version bump must
/// be a deliberate change to a public contract surface, not a side effect of
/// some other refactor. Bumping the constant requires updating this test in
/// the same commit, which forces the conversation.
///
/// Spec: `PROTOCOL.md` §1 ("Version: `IPC_PROTOCOL_VERSION = 1`").
#[test]
fn ipc_protocol_version_is_one() {
    assert_eq!(cognitive_memory_protocol::IPC_PROTOCOL_VERSION, 1);
}
