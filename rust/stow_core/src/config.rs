//! Configuration passed from Swift into the Rust core as a JSON string.
//!
//! The host app writes this (bucket/region/prefix + tuning) into the App Group
//! container; both the app and the extension construct a `StowCore` from it.
//! Credentials are NOT carried here — in M1 they come from the Keychain via the
//! AWS credential provider chain.

use crate::error::{StowError, StowResult};
use serde::Deserialize;

// Several fields are consumed only once S3/index wiring lands in M1.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// S3 bucket holding offloaded objects.
    pub bucket: String,
    /// AWS region, e.g. "us-west-1".
    pub region: String,
    /// Key prefix within the bucket (e.g. "stow/"). Defaults to empty.
    #[serde(default)]
    pub prefix: String,
    /// Absolute path to the shared SQLite index in the App Group container.
    /// Optional in M0; required once the index is wired up in M1.
    #[serde(default)]
    pub index_path: Option<String>,

    /// Download tuning. Sensible defaults so the host can omit them.
    #[serde(default = "default_chunk_size")]
    pub chunk_size_bytes: u64,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: u32,
}

fn default_chunk_size() -> u64 {
    8 * 1024 * 1024 // 8 MiB
}

fn default_concurrency() -> u32 {
    12
}

impl Config {
    pub fn from_json(s: &str) -> StowResult<Config> {
        let cfg: Config = serde_json::from_str(s)
            .map_err(|e| StowError::InvalidConfig(format!("parse error: {e}")))?;
        if cfg.bucket.trim().is_empty() {
            return Err(StowError::InvalidConfig("bucket must not be empty".into()));
        }
        if cfg.region.trim().is_empty() {
            return Err(StowError::InvalidConfig("region must not be empty".into()));
        }
        Ok(cfg)
    }
}
