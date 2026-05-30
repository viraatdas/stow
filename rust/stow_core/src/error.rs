//! Error codes returned across the C FFI boundary.
//!
//! Every fallible `extern "C"` entry point returns one of these as an `int32_t`.
//! `STOW_OK` (0) means success; anything nonzero is a failure and, where the
//! function takes an `err_out: *mut *mut c_char`, a human-readable message is
//! written there (caller frees with `stow_string_free`).

use std::ffi::c_int;

/// Result/error codes shared with Swift. Kept `#[repr(i32)]` so cbindgen emits a
/// plain C enum and the values are ABI-stable.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StowStatus {
    /// Operation succeeded.
    Ok = 0,
    /// Catch-all internal error.
    Unknown = 1,
    /// A Rust panic was caught at the FFI boundary (should never reach Swift in
    /// normal operation; indicates a bug).
    Panic = 2,
    /// A required pointer argument was null or otherwise invalid.
    InvalidArg = 3,
    /// The config JSON passed to `stow_core_new` was missing/malformed.
    InvalidConfig = 4,
    /// Functionality not implemented yet (M0 stubs return this).
    Unimplemented = 5,
    /// Requested item/object not found.
    NotFound = 6,
    /// Local filesystem I/O error.
    Io = 7,
    /// Network / S3 transport error.
    Network = 8,
    /// Operation was cancelled via a cancel token.
    Cancelled = 9,
    /// Content failed integrity verification (hash/AEAD tag mismatch).
    Integrity = 10,
}

impl StowStatus {
    /// The `i32` value sent across FFI.
    pub fn code(self) -> c_int {
        self as c_int
    }
}

/// Internal error type used inside the crate; maps to a `StowStatus` at the
/// boundary. Heavy runtime variants (network, io) get fleshed out in M1/M3.
#[allow(dead_code)] // some variants are only constructed once M1/M3 land
#[derive(Debug, thiserror::Error)]
pub enum StowError {
    #[error("invalid argument: {0}")]
    InvalidArg(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("not implemented yet: {0}")]
    Unimplemented(&'static str),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("cancelled")]
    Cancelled,
    #[error("integrity check failed: {0}")]
    Integrity(String),
    #[error("{0}")]
    Unknown(String),
}

impl StowError {
    pub fn status(&self) -> StowStatus {
        match self {
            StowError::InvalidArg(_) => StowStatus::InvalidArg,
            StowError::InvalidConfig(_) => StowStatus::InvalidConfig,
            StowError::Unimplemented(_) => StowStatus::Unimplemented,
            StowError::NotFound(_) => StowStatus::NotFound,
            StowError::Io(_) => StowStatus::Io,
            StowError::Network(_) => StowStatus::Network,
            StowError::Cancelled => StowStatus::Cancelled,
            StowError::Integrity(_) => StowStatus::Integrity,
            StowError::Unknown(_) => StowStatus::Unknown,
        }
    }
}

pub type StowResult<T> = Result<T, StowError>;
