//! `stow_core` — the Rust core for Stow, linked into the macOS app and the File
//! Provider extension as a static library and exposed over a small C ABI.
//!
//! Responsibilities (built out across milestones):
//! * S3 I/O — parallel ranged downloads (rehydration) and uploads (M1/M3)
//! * SQLite index of managed items (M1)
//! * blake3 content hashing + dedup (M1/M3)
//! * client-side AES-256-GCM framed encryption (M3)
//!
//! The public surface is everything re-exported from [`ffi`]. cbindgen reads
//! this crate to generate `stow_core.h`.

mod config;
mod engine;
mod error;
mod ffi;
mod index;
mod s3;

// Re-export the FFI surface so cbindgen and linkers see it from the crate root.
pub use error::StowStatus;
pub use ffi::*;
