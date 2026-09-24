//! Shared types and traits for cognitive-memory-daemon.
//!
//! Leaf crate. Depends on no other internal crate. Holds the type vocabulary
//! that everything else speaks: `MemoryId`, `UserId`, `Category`, `MemoryType`,
//! error types. Plus the daemon's user-facing TOML configuration so the CLI
//! can read/write it without depending on the daemon crate.
//!
//! See `ARCHITECTURE.md` §5 for the dependency graph this crate sits at the
//! bottom of.

mod config;
mod paths;
pub use config::{config_path, ConfigError, DaemonConfig, LifecycleOverrides, LlmConfig};
pub use paths::{
    app_instance_name, default_instance_name, ensure_private_dir, secure_private_file_if_exists,
    secure_private_socket, RuntimePaths,
};
