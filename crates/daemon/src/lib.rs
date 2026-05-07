//! The cognitive-memory daemon library.
//!
//! Exposes the `Daemon` type that owns a `Store`, an `EmbeddingProvider`,
//! and a Unix-socket accept loop. The `cm-daemon` binary in `main.rs` is a
//! thin wrapper that constructs `Daemon` with production defaults; tests
//! construct it with `FakeEmbeddingProvider` for speed.
//!
//! Adapted from `mxr/crates/daemon/src/server.rs` (accept loop, semaphore,
//! socket inspection). See `docs/developer/code-reuse.md` Phase 4.

mod doctor;
mod handlers;
mod server;
mod trace;

pub use doctor::{run_doctor, CheckLevel, CheckResult, DoctorReport};
pub use server::{Daemon, DaemonError};
pub use trace::{Trace, TraceRing};
