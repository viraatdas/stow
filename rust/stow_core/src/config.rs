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
    /// Auto-offload policy (smart, conservative defaults).
    #[serde(default)]
    pub policy: Policy,
    /// AWS credentials captured at `stow init` time. Stored here because the
    /// sandboxed File Provider extension cannot read `~/.aws`; the unsandboxed
    /// CLI resolves them once and persists them in the shared group container.
    #[serde(default)]
    pub access_key_id: Option<String>,
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub session_token: Option<String>,
}

fn default_prefix() -> String {
    "objects/".to_string()
}

/// Smart-default policy for what `stow auto` will offload. Deliberately
/// conservative: only large, genuinely-stale files in a few safe locations.
/// Tuned so nothing surprising gets moved (remember: until the File Provider
/// layer lands, an offloaded file is a placeholder until `stow restore`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    /// Minimum file size to consider. Small files aren't worth offloading.
    #[serde(default = "default_min_size")]
    pub min_size_bytes: u64,
    /// A file must be untouched (neither read nor modified) for at least this
    /// many days before it's an offload candidate.
    #[serde(default = "default_min_age_days")]
    pub min_age_days: u64,
    /// Directories scanned for candidates (absolute paths).
    #[serde(default = "default_roots")]
    pub roots: Vec<String>,
    /// Path substrings that exclude a file from consideration.
    #[serde(default = "default_excludes")]
    pub excludes: Vec<String>,
    /// Scan inside hidden dirs (e.g. `~/.cache`). Off by default: in-place offload
    /// there doesn't auto-restore on open (only the Stow folder does), so stubbing
    /// a cache file an app reads directly can break it. Credential dirs (`.ssh`,
    /// `.aws`, …) and `.git`/`.Trash` stay excluded even when this is on.
    #[serde(default = "default_include_hidden")]
    pub include_hidden: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            min_size_bytes: default_min_size(),
            min_age_days: default_min_age_days(),
            roots: default_roots(),
            excludes: default_excludes(),
            include_hidden: default_include_hidden(),
        }
    }
}

fn default_include_hidden() -> bool {
    false
}

fn default_min_size() -> u64 {
    10 * 1024 * 1024 // 10 MiB — below this, offloading isn't worth it
}

fn default_min_age_days() -> u64 {
    90 // conservative: a full quarter untouched before we'd move it
}

fn default_roots() -> Vec<String> {
    let home = dirs::home_dir().unwrap_or_default();
    ["Downloads", "Desktop", "Movies"]
        .iter()
        .map(|d| home.join(d).to_string_lossy().into_owned())
        .collect()
}

fn default_excludes() -> Vec<String> {
    // Substrings; anything matching is skipped. Keep system/app/cache areas safe.
    vec![
        "/Library/".into(),
        "/.Trash/".into(),
        "node_modules".into(),
        "/.git/".into(),
        "/DerivedData/".into(),
        "/.venv/".into(),
    ]
}

impl Config {
    /// Directory holding config + index.
    ///
    /// Sandboxed callers (the File Provider extension, the agent) can't compute
    /// the shared location themselves — `dirs::*` resolve to the per-process
    /// sandbox container, not the real home. So Swift resolves the App Group
    /// container via `containerURL(forSecurityApplicationGroupIdentifier:)` and
    /// passes it in `STOW_GROUP_DIR`. When set, that's the single shared store
    /// for config + both SQLite DBs, reachable by the CLI and the sandboxed
    /// extension alike. Falls back to `~/Library/Application Support/ai.exla.stow`
    /// for the bare CLI when no group dir is provided.
    pub fn support_dir() -> StowResult<PathBuf> {
        if let Ok(dir) = std::env::var("STOW_GROUP_DIR") {
            if !dir.trim().is_empty() {
                return Ok(PathBuf::from(dir));
            }
        }
        let base = dirs::data_dir()
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

    /// Explicit credentials captured at init, if present. The sandboxed
    /// extension relies on these; the CLI falls back to the default chain when
    /// they're absent (returns None).
    pub fn creds(&self) -> Option<crate::s3::Creds> {
        match (&self.access_key_id, &self.secret_access_key) {
            (Some(ak), Some(sk)) if !ak.is_empty() && !sk.is_empty() => {
                Some(crate::s3::Creds {
                    access_key_id: ak.clone(),
                    secret_access_key: sk.clone(),
                    session_token: self.session_token.clone(),
                })
            }
            _ => None,
        }
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
