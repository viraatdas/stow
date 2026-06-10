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
    /// True when the path is now a symlink into the File Provider mirror, so
    /// any app that opens it auto-downloads the bytes. False = legacy stub
    /// (File Provider unavailable); restore requires `stow restore`.
    pub transparent: bool,
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
pub struct FolderItem {
    pub filename: String,
    pub size: i64,
}

#[derive(Serialize)]
pub struct StatusResult {
    pub bucket: String,
    pub region: String,
    pub count: usize,
    pub bytes_offloaded: i64,
    /// Files currently saving disk space (in-place placeholders + dataless folder
    /// files) and their total bytes. DB-derived, so it's correct even from the
    /// sandboxed agent, which can't stat the original paths.
    pub saved_count: usize,
    pub saved_bytes: i64,
    pub items: Vec<StatusItem>,
    /// Offloaded files in the transparent Stow folder (dataless, only in S3).
    pub folder_count: usize,
    pub folder_bytes: i64,
    pub folder_items: Vec<FolderItem>,
}

// ---- init --------------------------------------------------------------------

/// Detect AWS creds, derive a unique bucket, create it, persist config.
pub fn init(region_arg: Option<String>) -> StowResult<InitResult> {
    // Never probe EC2 instance metadata (IMDS) — on a laptop that endpoint isn't
    // reachable and the SDK's default chain blocks on it for a long time, which
    // made `stow init` hang. We only ever use env / ~/.aws / SSO credentials.
    std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");
    let region = region_arg.unwrap_or_else(config::default_region);
    let rt = runtime()?;
    rt.block_on(async {
        let account = s3::account_id(&region).await?;
        let bucket = config::derive_bucket_name(&format!("{account}-{region}"));
        // Capture credentials now (CLI is unsandboxed) so the sandboxed extension
        // can reuse them later from the shared config.
        let creds = s3::resolve_default_creds(&region).await.ok();
        let client = s3::client(&region, creds.clone()).await?;
        let existed = client.head_bucket().bucket(&bucket).send().await.is_ok();
        s3::ensure_bucket(&client, &bucket, &region).await?;
        let cfg = Config {
            bucket: bucket.clone(),
            region: region.clone(),
            prefix: "objects/".to_string(),
            policy: config::Policy::default(),
            access_key_id: creds.as_ref().map(|c| c.access_key_id.clone()),
            secret_access_key: creds.as_ref().map(|c| c.secret_access_key.clone()),
            session_token: creds.as_ref().and_then(|c| c.session_token.clone()),
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
    // Files in the Stow folder are already managed (auto-evicted, auto-download).
    if fp_relative(path).is_some() {
        return Err(StowError::InvalidArg(format!(
            "{path} is inside the Stow folder — it's already offload-managed"
        )));
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
        let client = s3::client(&cfg.region, cfg.creds()).await?;
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
        restored: false,
    };
    Index::open()?.upsert(&rec)?;

    // Preferred: replace the file with a symlink into the File Provider mirror,
    // so opening it anywhere auto-downloads the bytes. Falls back to the legacy
    // JSON stub when the File Provider domain isn't available.
    let transparent = make_transparent(path, &rec).unwrap_or(false);
    if !transparent {
        write_placeholder(p, &cfg, &rec)?;
    }

    Ok(OffloadResult {
        path: path.to_string(),
        hash,
        bytes_freed: size,
        s3_key,
        deduped,
        transparent,
    })
}

// ---- transparent (symlink) offloads -------------------------------------------

/// The local mount of the Stow File Provider domain, e.g.
/// `~/Library/CloudStorage/StowAgent-Stow`. None when not mounted (agent not
/// running / domain not registered) or when sandboxed (container home).
fn cloudstorage_root() -> Option<std::path::PathBuf> {
    let cs = dirs::home_dir()?.join("Library/CloudStorage");
    for e in std::fs::read_dir(&cs).ok()?.flatten() {
        let name = e.file_name();
        let n = name.to_string_lossy();
        if n == "Stow" || n.ends_with("-Stow") {
            return Some(e.path());
        }
    }
    None
}

/// Mirror the freshly-offloaded object as a dataless File Provider file and
/// swap the original path for a symlink to it. Returns Ok(true) on success;
/// Ok(false) means "fall back to the stub" (File Provider missing or slow).
fn make_transparent(path: &str, rec: &Record) -> StowResult<bool> {
    use std::sync::atomic::{AtomicBool, Ordering};
    // One timeout per process is enough evidence the domain is down — don't
    // stall every file of a large `stow auto` run for 10s each.
    static FP_DOWN: AtomicBool = AtomicBool::new(false);
    if FP_DOWN.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let Some(root) = cloudstorage_root() else {
        return Ok(false);
    };

    let store = crate::provider::Store::open()?;
    crate::provider::mirror_inplace(&store, path, rec.size, &rec.hash, &rec.s3_key)?;
    // Ask the agent to signalEnumerator so fileproviderd picks up the new row.
    crate::provider::touch_signal();

    let dir = root.join(crate::provider::INPLACE_DIRNAME);
    let target = dir.join(crate::provider::mirror_filename(path));
    // Wait for fileproviderd to materialize the (dataless) entry. Listing the
    // directory forces it to enumerate.
    for _ in 0..50 {
        let _ = std::fs::read_dir(&dir);
        if std::fs::symlink_metadata(&target).is_ok() {
            std::fs::remove_file(path).map_err(|e| StowError::Io(e.to_string()))?;
            std::os::unix::fs::symlink(&target, path)
                .map_err(|e| StowError::Io(e.to_string()))?;
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    FP_DOWN.store(true, Ordering::Relaxed);
    Ok(false)
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
    let idx = Index::open()?;
    let rec = idx
        .get(path)?
        .ok_or_else(|| StowError::NotFound(format!("{path} is not tracked by Stow")))?;

    let rt = runtime()?;
    let data = rt.block_on(async {
        let client = s3::client(&cfg.region, cfg.creds()).await?;
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

    // A transparent offload is a symlink into the File Provider mirror. Remove
    // the link (NOT writing through it — that would upload into the mirror) and
    // drop the hidden mirror item before laying the real bytes back down.
    if let Ok(md) = std::fs::symlink_metadata(p) {
        if md.file_type().is_symlink() {
            std::fs::remove_file(p).map_err(|e| StowError::Io(e.to_string()))?;
            let _ = crate::provider::remove_mirror(path);
            crate::provider::touch_signal();
        }
    }
    std::fs::write(p, &data).map_err(|e| StowError::Io(e.to_string()))?;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(rec.mode))
        .map_err(|e| StowError::Io(e.to_string()))?;
    // Preserve the original modification time, but set access time to NOW: the
    // file was just restored/accessed, so it must NOT immediately re-qualify for
    // auto-offload (the policy uses max(atime, mtime) as "last used").
    set_times(p, now(), rec.mtime)?;
    // Mark restored so `status` stops counting it as saved disk space (the file
    // is back on disk now).
    idx.set_restored(path, true)?;

    Ok(RestoreResult {
        path: path.to_string(),
        bytes_restored: data.len() as i64,
    })
}

// ---- status ------------------------------------------------------------------

pub fn status() -> StowResult<StatusResult> {
    let cfg = load_cfg()?;
    let idx = Index::open()?;
    let items = idx.all()?;
    // In-place offloads still saving space. DB-only, so it's right even from
    // the sandboxed agent that can't stat the original paths: restored=0 AND
    // (no mirror, or mirror still dataless). A mirror an app has hydrated by
    // opening the symlink is on disk until the evictor re-offloads it.
    let fp_store = crate::provider::Store::open().ok();
    let mut saved_inplace_count = 0usize;
    let mut saved_inplace_bytes = 0i64;
    for r in &items {
        if r.restored {
            continue;
        }
        if let Some(store) = &fp_store {
            if let Ok(Some(m)) = store.get(&crate::provider::mirror_id(&r.path)) {
                if !m.dataless {
                    continue;
                }
            }
        }
        saved_inplace_count += 1;
        saved_inplace_bytes += r.size;
    }
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
    // Also report the transparent Stow folder's offloaded (dataless) files.
    let folder = crate::provider::list_dataless().unwrap_or_default();
    let folder_bytes: i64 = folder.iter().map(|i| i.size).sum();
    let folder_items: Vec<FolderItem> = folder
        .iter()
        .map(|i| FolderItem { filename: i.filename.clone(), size: i.size })
        .collect();

    Ok(StatusResult {
        bucket: cfg.bucket,
        region: cfg.region,
        count: items.len(),
        bytes_offloaded: total,
        // Total disk saved = in-place placeholders + all dataless folder files.
        saved_count: saved_inplace_count + folder_items.len(),
        saved_bytes: saved_inplace_bytes + folder_bytes,
        items: out,
        folder_count: folder_items.len(),
        folder_bytes,
        folder_items,
    })
}

// ---- migrate: legacy stubs -> transparent symlinks -----------------------------

#[derive(Serialize)]
pub struct MigrateResult {
    pub migrated: Vec<String>,
    pub failures: Vec<AutoFailure>,
    /// Already transparent (symlink) or restored — nothing to do.
    pub skipped: usize,
}

/// Convert every legacy STOW1 stub into a transparent symlink offload. The
/// bytes are already in S3 — this only creates the dataless mirrors and swaps
/// stubs for symlinks, so it's fast and network-free.
pub fn migrate() -> StowResult<MigrateResult> {
    let _ = load_cfg()?;
    let Some(root) = cloudstorage_root() else {
        return Err(StowError::InvalidConfig(
            "Stow folder isn't mounted — is StowAgent running?".into(),
        ));
    };
    let idx = Index::open()?;
    let store = crate::provider::Store::open()?;
    let mut res = MigrateResult { migrated: Vec::new(), failures: Vec::new(), skipped: 0 };

    // Pass 1: mirror rows for every stub, then one signal for the whole batch.
    let mut todo: Vec<Record> = Vec::new();
    for r in idx.all()? {
        let p = Path::new(&r.path);
        let is_stub = match std::fs::symlink_metadata(p) {
            Ok(md) if md.file_type().is_symlink() => {
                res.skipped += 1; // already transparent
                continue;
            }
            Ok(_) => is_placeholder(p).unwrap_or(false),
            Err(_) => false,
        };
        if !is_stub {
            res.skipped += 1; // restored / missing — leave it alone
            continue;
        }
        match crate::provider::mirror_inplace(&store, &r.path, r.size, &r.hash, &r.s3_key) {
            Ok(_) => todo.push(r),
            Err(e) => res.failures.push(AutoFailure { path: r.path, error: e.to_string() }),
        }
    }
    crate::provider::touch_signal();

    // Pass 2: wait for the dataless entries, then swap stub -> symlink.
    let dir = root.join(crate::provider::INPLACE_DIRNAME);
    for r in todo {
        let target = dir.join(crate::provider::mirror_filename(&r.path));
        let mut ok = false;
        for _ in 0..50 {
            let _ = std::fs::read_dir(&dir);
            if std::fs::symlink_metadata(&target).is_ok() {
                ok = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if !ok {
            res.failures.push(AutoFailure {
                path: r.path,
                error: "mirror never appeared in the Stow folder".into(),
            });
            continue;
        }
        let swap = std::fs::remove_file(&r.path)
            .and_then(|_| std::os::unix::fs::symlink(&target, &r.path));
        match swap {
            Ok(_) => res.migrated.push(r.path),
            Err(e) => res.failures.push(AutoFailure { path: r.path, error: e.to_string() }),
        }
    }
    Ok(res)
}

// ---- share: permanent public links ---------------------------------------------

#[derive(Serialize)]
pub struct ShareResult {
    pub url: String,
    pub token: String,
    pub path: String,
    pub size: i64,
    pub is_folder: bool,
    pub file_count: usize,
}

#[derive(Serialize)]
pub struct SharesList {
    pub shares: Vec<crate::index::ShareRow>,
}

/// 128-bit random token, hex — the unguessable part of every share URL.
fn rand_token() -> StowResult<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| StowError::Io(format!("urandom: {e}")))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Percent-encode a filename for the share URL path.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// If `path` is inside the Stow folder mount, its components relative to the
/// root (e.g. ["Photos", "trip.heic"]). Otherwise None.
fn fp_relative(path: &str) -> Option<Vec<String>> {
    let marker = "/Library/CloudStorage/";
    let i = path.find(marker)?;
    let rest = &path[i + marker.len()..];
    let mut comps = rest.split('/').filter(|c| !c.is_empty());
    let mount = comps.next()?;
    if !(mount == "Stow" || mount.ends_with("-Stow")) {
        return None;
    }
    Some(comps.map(|s| s.to_string()).collect())
}

/// Resolve a Stow-folder path to its File Provider item by walking the tree.
fn fp_item_for(store: &crate::provider::Store, comps: &[String]) -> StowResult<Option<crate::provider::Item>> {
    let mut parent = crate::provider::ROOT.to_string();
    let mut found: Option<crate::provider::Item> = None;
    for c in comps {
        let kids = store.children(&parent)?;
        match kids.into_iter().find(|k| &k.filename == c) {
            Some(k) => {
                parent = k.item_id.clone();
                found = Some(k);
            }
            None => return Ok(None),
        }
    }
    Ok(found)
}

/// Zip a directory tree into memory. Dataless Stow-folder members hydrate
/// transparently as they're read. Returns (zip bytes, file count).
fn zip_dir(dir: &Path) -> StowResult<(Vec<u8>, usize)> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut count = 0usize;
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        for entry in walkdir::WalkDir::new(dir).follow_links(false) {
            let entry = entry.map_err(|e| StowError::Io(e.to_string()))?;
            let rel = entry.path().strip_prefix(dir).unwrap_or(entry.path());
            if rel.as_os_str().is_empty() {
                continue;
            }
            let name = rel.to_string_lossy().to_string();
            if name.starts_with(crate::provider::INPLACE_DIRNAME) {
                continue; // never publish the hidden mirror area
            }
            if entry.file_type().is_dir() {
                zw.add_directory(format!("{name}/"), opts)
                    .map_err(|e| StowError::Io(e.to_string()))?;
            } else if entry.file_type().is_file() {
                let data = std::fs::read(entry.path())
                    .map_err(|e| StowError::Io(format!("{}: {e}", entry.path().display())))?;
                zw.start_file(name, opts).map_err(|e| StowError::Io(e.to_string()))?;
                zw.write_all(&data).map_err(|e| StowError::Io(e.to_string()))?;
                count += 1;
            }
        }
        zw.finish().map_err(|e| StowError::Io(e.to_string()))?;
    }
    Ok((buf.into_inner(), count))
}

/// Publish a permanent public link for a file or folder. Folders are zipped so
/// the recipient downloads everything in one go. The link is a snapshot: later
/// edits to the file don't change what the link serves. Offloaded content is
/// copied server-side in S3 — no download needed.
pub fn share(path: &str) -> StowResult<ShareResult> {
    let cfg = load_cfg()?;
    let p = Path::new(path);
    let md = std::fs::symlink_metadata(p)
        .map_err(|e| StowError::Io(format!("{path}: {e}")))?;
    let token = rand_token()?;
    let idx = Index::open()?;
    let basename = p
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();

    // Work out the cheapest source of bytes, then publish under shares/<token>/.
    enum Source {
        S3Key(String),   // server-side copy, no transfer
        Bytes(Vec<u8>),  // upload local bytes
    }
    let (source, share_name, size, is_folder, file_count) = if md.is_dir() {
        let (data, count) = zip_dir(p)?;
        let size = data.len() as i64;
        (Source::Bytes(data), format!("{basename}.zip"), size, true, count)
    } else if is_placeholder(p)? {
        // In-place offload (stub or symlink): reuse its object.
        let rec = idx
            .get(path)?
            .ok_or_else(|| StowError::NotFound(format!("{path} is offloaded but not tracked")))?;
        (Source::S3Key(rec.s3_key.clone()), basename.clone(), rec.size, false, 1)
    } else if let Some(comps) = fp_relative(path) {
        // A file in the Stow folder: share straight from its object (works even
        // when dataless — nothing is downloaded).
        let store = crate::provider::Store::open()?;
        match fp_item_for(&store, &comps)? {
            Some(it) if !it.is_folder && it.s3_key.is_some() => {
                (Source::S3Key(it.s3_key.clone().unwrap()), basename.clone(), it.size, false, 1)
            }
            _ => {
                let data = std::fs::read(p).map_err(|e| StowError::Io(e.to_string()))?;
                let size = data.len() as i64;
                (Source::Bytes(data), basename.clone(), size, false, 1)
            }
        }
    } else {
        // Any ordinary local file.
        let data = std::fs::read(p).map_err(|e| StowError::Io(e.to_string()))?;
        let size = data.len() as i64;
        (Source::Bytes(data), basename.clone(), size, false, 1)
    };

    let share_key = format!("shares/{token}/{share_name}");
    let url = format!(
        "https://{}.s3.{}.amazonaws.com/shares/{token}/{}",
        cfg.bucket,
        cfg.region,
        url_encode(&share_name)
    );

    let rt = runtime()?;
    rt.block_on(async {
        let client = s3::client(&cfg.region, cfg.creds()).await?;
        s3::ensure_public_shares(&client, &cfg.bucket).await?;
        match source {
            Source::S3Key(src) => s3::copy_object(&client, &cfg.bucket, &src, &share_key).await,
            Source::Bytes(data) => s3::put_object(&client, &cfg.bucket, &share_key, data).await,
        }
    })?;

    idx.add_share(&crate::index::ShareRow {
        token: token.clone(),
        source: path.to_string(),
        s3_key: share_key,
        url: url.clone(),
        size,
        is_folder,
        created_at: now(),
    })?;

    Ok(ShareResult { url, token, path: path.to_string(), size, is_folder, file_count })
}

/// Revoke a share: delete the public object and forget the link.
pub fn unshare(token: &str) -> StowResult<crate::index::ShareRow> {
    let cfg = load_cfg()?;
    let idx = Index::open()?;
    let row = idx
        .get_share(token)?
        .ok_or_else(|| StowError::NotFound(format!("no share with token {token}")))?;
    let rt = runtime()?;
    rt.block_on(async {
        let client = s3::client(&cfg.region, cfg.creds()).await?;
        s3::delete_object(&client, &cfg.bucket, &row.s3_key).await
    })?;
    idx.remove_share(token)?;
    Ok(row)
}

/// All active share links, newest first.
pub fn list_shares() -> StowResult<SharesList> {
    Ok(SharesList { shares: Index::open()?.list_shares()? })
}

// ---- placeholder helpers -----------------------------------------------------

pub(crate) fn is_placeholder(p: &Path) -> StowResult<bool> {
    // Transparent offloads are symlinks into the hidden File Provider mirror.
    // Check with symlink_metadata FIRST — fs::metadata would follow the link
    // and stat the dataless target, and reading it would trigger a download.
    if let Ok(md) = std::fs::symlink_metadata(p) {
        if md.file_type().is_symlink() {
            if let Ok(t) = std::fs::read_link(p) {
                let s = t.to_string_lossy();
                return Ok(s.contains("/Library/CloudStorage/")
                    && s.contains(crate::provider::INPLACE_DIRNAME));
            }
            return Ok(false);
        }
    }
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
