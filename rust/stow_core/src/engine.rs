//! The Stow engine: init (auto-provision S3), offload (upload + replace with a
//! tiny placeholder to free space), restore (download + rewrite byte-identical),
//! and status. CLI-mode core — no File Provider required.
//!
//! Public functions are synchronous; each builds a short-lived Tokio runtime
//! (the CLI is one-shot: one command = one runtime), keeping the FFI simple.

use crate::config::{self, Config};
use crate::error::{StowError, StowResult};
use crate::index::{Index, Record};
use crate::s3;
use serde::Serialize;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Placeholder magic. A file beginning with this is an offloaded stub.
const MAGIC: &[u8] = b"STOW1\n";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn runtime() -> StowResult<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| StowError::Unknown(format!("tokio runtime: {e}")))
}

// ---- results returned to the CLI as JSON -------------------------------------

#[derive(Serialize)]
pub struct InitResult {
    pub bucket: String,
    pub region: String,
    pub account: String,
    pub created: bool,
}

#[derive(Serialize)]
pub struct OffloadResult {
    pub path: String,
    pub hash: String,
    pub bytes_freed: i64,
    pub s3_key: String,
    pub deduped: bool,
}

#[derive(Serialize)]
pub struct RestoreResult {
    pub path: String,
    pub bytes_restored: i64,
}

#[derive(Serialize)]
pub struct StatusItem {
    pub path: String,
    pub size: i64,
    pub hash: String,
    pub present_as_placeholder: bool,
}

#[derive(Serialize)]
pub struct StatusResult {
    pub bucket: String,
    pub region: String,
    pub count: usize,
    pub bytes_offloaded: i64,
    pub items: Vec<StatusItem>,
}

// ---- init --------------------------------------------------------------------

/// Detect AWS creds, derive a unique bucket, create it, persist config.
pub fn init(region_arg: Option<String>) -> StowResult<InitResult> {
    let region = region_arg.unwrap_or_else(config::default_region);
    let rt = runtime()?;
    rt.block_on(async {
        let account = s3::account_id(&region).await?;
        let bucket = config::derive_bucket_name(&format!("{account}-{region}"));
        let client = s3::client(&region).await?;
        let existed = client.head_bucket().bucket(&bucket).send().await.is_ok();
        s3::ensure_bucket(&client, &bucket, &region).await?;
        let cfg = Config {
            bucket: bucket.clone(),
            region: region.clone(),
            prefix: "objects/".to_string(),
            policy: config::Policy::default(),
        };
        cfg.save()?;
        // Touch the index so it's ready.
        let _ = Index::open()?;
        Ok(InitResult {
            bucket,
            region,
            account,
            created: !existed,
        })
    })
}

fn load_cfg() -> StowResult<Config> {
    Config::load()?.ok_or_else(|| {
        StowError::InvalidConfig("Stow is not initialized — run `stow init` first".into())
    })
}

// ---- offload -----------------------------------------------------------------

/// Upload a file to S3, then replace it with a placeholder to free disk space.
pub fn offload(path: &str) -> StowResult<OffloadResult> {
    let cfg = load_cfg()?;
    let p = Path::new(path);
    config::ensure_regular_file(p)?;

    // If it's already a placeholder, nothing to do.
    if is_placeholder(p)? {
        return Err(StowError::InvalidArg(format!("{path} is already offloaded")));
    }

    let data = std::fs::read(p).map_err(|e| StowError::Io(e.to_string()))?;
    let md = std::fs::metadata(p).map_err(|e| StowError::Io(e.to_string()))?;
    let size = md.len() as i64;
    let mode = md.permissions().mode();
    let mtime = std::os::unix::fs::MetadataExt::mtime(&md);

    let hash = blake3::hash(&data).to_hex().to_string();
    let s3_key = cfg.object_key(&hash);

    let rt = runtime()?;
    let deduped = rt.block_on(async {
        let client = s3::client(&cfg.region).await?;
        let existed = client
            .head_object()
            .bucket(&cfg.bucket)
            .key(&s3_key)
            .send()
            .await
            .is_ok();
        s3::put_object(&client, &cfg.bucket, &s3_key, data).await?;
        Ok::<bool, StowError>(existed)
    })?;

    // Record before mutating the file, so we never lose track of it.
    let rec = Record {
        path: path.to_string(),
        hash: hash.clone(),
        size,
        mode,
        mtime,
        s3_key: s3_key.clone(),
        offloaded_at: now(),
    };
    Index::open()?.upsert(&rec)?;

    // Replace the file contents with a placeholder, preserving the path.
    write_placeholder(p, &cfg, &rec)?;

    Ok(OffloadResult {
        path: path.to_string(),
        hash,
        bytes_freed: size,
        s3_key,
        deduped,
    })
}

// ---- scan / auto-offload -----------------------------------------------------

#[derive(Serialize)]
pub struct ScanResult {
    pub candidate_count: usize,
    pub reclaimable_bytes: i64,
    pub min_size_bytes: u64,
    pub min_age_days: u64,
    pub roots: Vec<String>,
    pub candidates: Vec<crate::policy::Candidate>,
}

/// Dry run: list what `stow auto` would offload, largest first. No changes.
pub fn scan() -> StowResult<ScanResult> {
    let cfg = load_cfg()?;
    let candidates = crate::policy::scan(&cfg)?;
    let reclaimable_bytes = candidates.iter().map(|c| c.size).sum();
    Ok(ScanResult {
        candidate_count: candidates.len(),
        reclaimable_bytes,
        min_size_bytes: cfg.policy.min_size_bytes,
        min_age_days: cfg.policy.min_age_days,
        roots: cfg.policy.roots.clone(),
        candidates,
    })
}

#[derive(Serialize)]
pub struct AutoFailure {
    pub path: String,
    pub error: String,
}

#[derive(Serialize)]
pub struct AutoResult {
    pub offloaded_count: usize,
    pub bytes_freed: i64,
    pub offloaded: Vec<String>,
    pub failures: Vec<AutoFailure>,
}

/// Apply the policy: offload every candidate. This is what the scheduled job
/// (and `stow auto`) runs.
pub fn auto_offload() -> StowResult<AutoResult> {
    let cfg = load_cfg()?;
    let candidates = crate::policy::scan(&cfg)?;
    let mut res = AutoResult {
        offloaded_count: 0,
        bytes_freed: 0,
        offloaded: Vec::new(),
        failures: Vec::new(),
    };
    for c in candidates {
        match offload(&c.path) {
            Ok(o) => {
                res.offloaded_count += 1;
                res.bytes_freed += o.bytes_freed;
                res.offloaded.push(c.path);
            }
            Err(e) => res.failures.push(AutoFailure {
                path: c.path,
                error: e.to_string(),
            }),
        }
    }
    Ok(res)
}

// ---- config accessors --------------------------------------------------------

/// Return the full persisted config (bucket/region/policy) as-is.
pub fn get_config() -> StowResult<Config> {
    load_cfg()
}

/// Replace the policy block with the provided JSON and persist.
pub fn set_policy(policy_json: &str) -> StowResult<Config> {
    let pol: config::Policy = serde_json::from_str(policy_json)
        .map_err(|e| StowError::InvalidConfig(format!("policy: {e}")))?;
    let mut cfg = load_cfg()?;
    cfg.policy = pol;
    cfg.save()?;
    Ok(cfg)
}

// ---- restore -----------------------------------------------------------------

/// Download an offloaded file from S3 and rewrite it byte-identically.
pub fn restore(path: &str) -> StowResult<RestoreResult> {
    let cfg = load_cfg()?;
    let p = Path::new(path);
    let rec = Index::open()?
        .get(path)?
        .ok_or_else(|| StowError::NotFound(format!("{path} is not tracked by Stow")))?;

    let rt = runtime()?;
    let data = rt.block_on(async {
        let client = s3::client(&cfg.region).await?;
        s3::get_object(&client, &cfg.bucket, &rec.s3_key).await
    })?;

    // Integrity check before overwriting.
    let got = blake3::hash(&data).to_hex().to_string();
    if got != rec.hash {
        return Err(StowError::Integrity(format!(
            "{path}: hash mismatch (expected {}, got {})",
            rec.hash, got
        )));
    }

    std::fs::write(p, &data).map_err(|e| StowError::Io(e.to_string()))?;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(rec.mode))
        .map_err(|e| StowError::Io(e.to_string()))?;
    // Preserve the original modification time, but set access time to NOW: the
    // file was just restored/accessed, so it must NOT immediately re-qualify for
    // auto-offload (the policy uses max(atime, mtime) as "last used").
    set_times(p, now(), rec.mtime)?;

    Ok(RestoreResult {
        path: path.to_string(),
        bytes_restored: data.len() as i64,
    })
}

// ---- status ------------------------------------------------------------------

pub fn status() -> StowResult<StatusResult> {
    let cfg = load_cfg()?;
    let items = Index::open()?.all()?;
    let mut out = Vec::with_capacity(items.len());
    let mut total = 0i64;
    for r in &items {
        total += r.size;
        let present = Path::new(&r.path)
            .exists()
            .then(|| is_placeholder(Path::new(&r.path)).unwrap_or(false))
            .unwrap_or(false);
        out.push(StatusItem {
            path: r.path.clone(),
            size: r.size,
            hash: r.hash.clone(),
            present_as_placeholder: present,
        });
    }
    Ok(StatusResult {
        bucket: cfg.bucket,
        region: cfg.region,
        count: items.len(),
        bytes_offloaded: total,
        items: out,
    })
}

// ---- placeholder helpers -----------------------------------------------------

pub(crate) fn is_placeholder(p: &Path) -> StowResult<bool> {
    let md = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };
    if !md.is_file() || md.len() < MAGIC.len() as u64 {
        return Ok(false);
    }
    let mut buf = vec![0u8; MAGIC.len()];
    use std::io::Read;
    let mut f = std::fs::File::open(p).map_err(|e| StowError::Io(e.to_string()))?;
    f.read_exact(&mut buf).map_err(|e| StowError::Io(e.to_string()))?;
    Ok(buf == MAGIC)
}

fn write_placeholder(p: &Path, cfg: &Config, rec: &Record) -> StowResult<()> {
    #[derive(Serialize)]
    struct Stub<'a> {
        path: &'a str,
        hash: &'a str,
        size: i64,
        s3_key: &'a str,
        bucket: &'a str,
        region: &'a str,
    }
    let stub = Stub {
        path: &rec.path,
        hash: &rec.hash,
        size: rec.size,
        s3_key: &rec.s3_key,
        bucket: &cfg.bucket,
        region: &cfg.region,
    };
    let json = serde_json::to_string_pretty(&stub).map_err(|e| StowError::Unknown(e.to_string()))?;
    let mut body = Vec::with_capacity(MAGIC.len() + json.len() + 1);
    body.extend_from_slice(MAGIC);
    body.extend_from_slice(json.as_bytes());
    body.push(b'\n');
    std::fs::write(p, &body).map_err(|e| StowError::Io(e.to_string()))?;
    Ok(())
}

/// Set a file's access and modification times via libc.
fn set_times(p: &Path, atime: i64, mtime: i64) -> StowResult<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(p.as_os_str().as_bytes()).map_err(|e| StowError::Io(e.to_string()))?;
    let times = [
        libc::timeval { tv_sec: atime as libc::time_t, tv_usec: 0 },
        libc::timeval { tv_sec: mtime as libc::time_t, tv_usec: 0 },
    ]; // [atime, mtime]
    let rc = unsafe { libc::utimes(c.as_ptr(), times.as_ptr()) };
    if rc != 0 {
        return Err(StowError::Io(format!(
            "utimes failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}
