//! Rust client for cognitive-memory-daemon.
//!
//! Thin wrapper over a Unix socket using `IpcCodec`. Performs the
//! Hello/Welcome handshake on connect; afterwards exposes a `request` method
//! that allocates a fresh `id` per call, sends a `Request`, and awaits the
//! matching `Response`.
//!
//! Per `AGENTS.md` §3, this crate may depend only on `core` and `protocol` —
//! never on `daemon` or any backend crate.

use bytes::BytesMut;
use cognitive_memory_protocol::{
    validate_protocol_version, IpcCodec, IpcMessage, IpcPayload, ProtocolMismatch, Request,
    Response, IPC_PROTOCOL_VERSION,
};
use futures::{SinkExt, StreamExt};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio_util::codec::{Decoder, Encoder, Framed, LengthDelimitedCodec};

/// Errors surfaced by the client.
#[allow(clippy::large_enum_variant)] // UnexpectedPayload carries IpcPayload by design.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("hello/welcome serialise/deserialise: {0}")]
    Handshake(#[from] serde_json::Error),
    #[error(
        "daemon protocol version {} != client {IPC_PROTOCOL_VERSION}",
        .0.daemon_version
    )]
    ProtocolMismatch(ProtocolMismatch),
    #[error("daemon closed connection mid-handshake")]
    HandshakeShortRead,
    #[error("daemon closed connection while awaiting response")]
    UnexpectedClose,
    #[error("response id {got} did not match request id {expected}")]
    IdMismatch { expected: u64, got: u64 },
    #[error("expected Response payload, got something else")]
    UnexpectedPayload(IpcPayload),
}

/// Client connected to a cognitive-memory daemon over a Unix socket.
pub struct Client {
    framed: Framed<UnixStream, IpcCodec>,
    next_id: u64,
}

impl Client {
    /// Connect to a daemon at `socket_path` and complete the handshake.
    pub async fn connect(
        socket_path: &Path,
        client_label: &str,
        user_id: &str,
    ) -> Result<Self, ClientError> {
        let mut stream = UnixStream::connect(socket_path).await?;

        // Pre-Framed handshake: Hello/Welcome are length-prefixed JSON
        // (same framing as IpcMessage) but carry a different schema. We
        // do them on the raw stream before wrapping in IpcCodec.
        let hello = serde_json::json!({
            "kind": "Hello",
            "client": client_label,
            "protocol_version": IPC_PROTOCOL_VERSION,
            "user_id": user_id,
        });
        write_length_prefixed_json(&mut stream, &hello).await?;

        let welcome = read_length_prefixed_json(&mut stream).await?;
        let proto = welcome
            .get("protocol_version")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(0);
        if let Err(mismatch) = validate_protocol_version(proto) {
            return Err(ClientError::ProtocolMismatch(mismatch));
        }

        let framed = Framed::new(stream, IpcCodec::new());
        Ok(Self { framed, next_id: 1 })
    }

    /// Send a request and await the matching response.
    pub async fn request(&mut self, request: Request) -> Result<Response, ClientError> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).expect("u64 id overflow");

        let msg = IpcMessage {
            id,
            payload: IpcPayload::Request(request),
        };
        self.framed.send(msg).await?;

        let reply = self
            .framed
            .next()
            .await
            .ok_or(ClientError::UnexpectedClose)??;

        if reply.id != id {
            return Err(ClientError::IdMismatch {
                expected: id,
                got: reply.id,
            });
        }
        match reply.payload {
            IpcPayload::Response(resp) => Ok(resp),
            other => Err(ClientError::UnexpectedPayload(other)),
        }
    }
}

async fn write_length_prefixed_json(
    stream: &mut UnixStream,
    value: &serde_json::Value,
) -> Result<(), ClientError> {
    let bytes = serde_json::to_vec(value)?;
    let mut codec = LengthDelimitedCodec::builder()
        .length_field_length(4)
        .max_frame_length(16 * 1024 * 1024)
        .new_codec();
    let mut buf = BytesMut::new();
    codec.encode(bytes::Bytes::from(bytes), &mut buf)?;
    stream.write_all(&buf).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_length_prefixed_json(
    stream: &mut UnixStream,
) -> Result<serde_json::Value, ClientError> {
    let mut buf = BytesMut::with_capacity(8 * 1024);
    let mut codec = LengthDelimitedCodec::builder()
        .length_field_length(4)
        .max_frame_length(16 * 1024 * 1024)
        .new_codec();

    loop {
        if let Some(frame) = codec.decode(&mut buf)? {
            return Ok(serde_json::from_slice(&frame)?);
        }
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            return Err(ClientError::HandshakeShortRead);
        }
    }
}
