//! Persisted Stow configuration and on-disk paths.
//!
//! Config lives at `~/Library/Application Support/ai.exla.stow/config.json`,
//! the SQLite index alongside it as `index.db`. Credentials are NOT stored here
//! — the AWS SDK reads them from the standard chain (`~/.aws`, env, SSO).

use crate::error::{StowError, StowResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// S3 bucket holding offloaded objects (auto-created by `stow init`).
    pub bucket: String,
    /// AWS region for the bucket.
    pub region: String,
    /// Key prefix within the bucket.
    #[serde(default = "default_prefix")]
    pub prefix: String,
}

fn default_prefix() -> String {
    "objects/".to_string()
}

impl Config {
    /// Directory holding config + index: `~/Library/Application Support/ai.exla.stow`.
    pub fn support_dir() -> StowResult<PathBuf> {
        let base = dirs::data_dir() // ~/Library/Application Support on macOS
            .ok_or_else(|| StowError::Io("cannot resolve Application Support dir".into()))?;
        Ok(base.join("ai.exla.stow"))
    }

    pub fn config_path() -> StowResult<PathBuf> {
        Ok(Self::support_dir()?.join("config.json"))
    }

    pub fn index_path() -> StowResult<PathBuf> {
        Ok(Self::support_dir()?.join("index.db"))
    }

    /// Load persisted config, or `None` if Stow hasn't been initialized.
    pub fn load() -> StowResult<Option<Config>> {
        let p = Self::config_path()?;
        if !p.exists() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(&p).map_err(|e| StowError::Io(e.to_string()))?;
        let cfg: Config =
            serde_json::from_str(&s).map_err(|e| StowError::InvalidConfig(e.to_string()))?;
        Ok(Some(cfg))
    }

    /// Persist config (creates the support dir if needed).
    pub fn save(&self) -> StowResult<()> {
        let dir = Self::support_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| StowError::Io(e.to_string()))?;
        let s = serde_json::to_string_pretty(self)
            .map_err(|e| StowError::Unknown(e.to_string()))?;
        std::fs::write(Self::config_path()?, s).map_err(|e| StowError::Io(e.to_string()))?;
        Ok(())
    }

    /// The S3 object key for a given content hash.
    pub fn object_key(&self, hash: &str) -> String {
        format!("{}{}", self.prefix, hash)
    }
}

/// Best-effort default region, used when `~/.aws` doesn't specify one.
pub fn default_region() -> String {
    std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_string())
}

/// Derive a stable, unique-ish bucket name for this machine/account.
/// Caller passes a short account/host token; we keep it DNS-safe.
pub fn derive_bucket_name(token: &str) -> String {
    let cleaned: String = token
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    format!("stow-{}", trimmed)
}

/// Helper to check a path exists and is a file.
pub fn ensure_regular_file(p: &Path) -> StowResult<()> {
    let md = std::fs::metadata(p).map_err(|e| StowError::Io(format!("{}: {e}", p.display())))?;
    if !md.is_file() {
        return Err(StowError::InvalidArg(format!("{} is not a regular file", p.display())));
    }
    Ok(())
}
