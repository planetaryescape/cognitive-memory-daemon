//! Shared types and traits for cognitive-memory-daemon.
//!
//! Leaf crate. Depends on no other internal crate. Holds the type vocabulary
//! that everything else speaks: `MemoryId`, `UserId`, `Category`, `MemoryType`,
//! error types. No I/O.
//!
//! See `ARCHITECTURE.md` §5 for the dependency graph this crate sits at the
//! bottom of.
