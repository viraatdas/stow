//! Reclaim space from regenerable tool/package-manager caches.
//!
//! These are NOT offloaded — offloading a cache file is actively harmful (the
//! tool reads a placeholder instead of getting a clean cache *miss*, so it breaks
//! instead of re-fetching). They're meant to be deleted: every entry here is
//! something a tool re-downloads or rebuilds on demand. We never touch source,
//! credentials (`.ssh`/`.aws`), installed toolchains (`.rustup`/`.ghcup`),
//! `.cargo/bin`, or config — only well-known caches.

use crate::error::{StowError, StowResult};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

/// (display name, path relative to home, how it comes back). Curated — only
/// directories that are pure regenerable caches.
const KNOWN: &[(&str, &str, &str)] = &[
    ("npm", ".npm/_cacache", "re-fetched by npm"),
    ("bun", ".bun/install/cache", "re-fetched by bun"),
    ("uv", ".cache/uv", "re-fetched by uv"),
    ("uv", "Library/Caches/uv", "re-fetched by uv"),
    ("pip", ".cache/pip", "re-fetched by pip"),
    ("pip", "Library/Caches/pip", "re-fetched by pip"),
    ("yarn", ".cache/yarn", "re-fetched by yarn"),
    ("yarn", "Library/Caches/Yarn", "re-fetched by yarn"),
    ("pnpm", ".pnpm-store", "re-fetched by pnpm"),
    ("huggingface", ".cache/huggingface", "models re-download on use"),
    ("gradle", ".gradle/caches", "re-fetched by gradle"),
    ("cargo (registry cache)", ".cargo/registry/cache", "re-fetched by cargo"),
    ("cargo (registry src)", ".cargo/registry/src", "re-extracted by cargo"),
    ("go (build)", "Library/Caches/go-build", "rebuilt by go"),
    ("go (mod download)", "go/pkg/mod/cache/download", "re-fetched by go"),
    ("deno", "Library/Caches/deno", "re-fetched by deno"),
    ("pre-commit", ".cache/pre-commit", "re-created by pre-commit"),
    ("zig", ".cache/zig", "rebuilt by zig"),
    ("playwright", "Library/Caches/ms-playwright", "re-downloaded"),
    ("puppeteer", ".cache/puppeteer", "re-downloaded"),
    ("electron", ".cache/electron", "re-downloaded"),
    ("cocoapods", "Library/Caches/CocoaPods", "re-fetched by pod"),
    ("Xcode DerivedData", "Library/Developer/Xcode/DerivedData", "rebuilt by Xcode"),
];

#[derive(Serialize, Clone)]
pub struct CacheEntry {
    pub name: String,
    pub path: String,
    pub size_bytes: i64,
    pub idle_days: i64,
    /// How the data comes back after deletion (shown to reassure it's safe).
    pub regenerates: String,
    pub removed: bool,
}

#[derive(Serialize)]
pub struct CleanReport {
    pub min_idle_days: u64,
    pub applied: bool,
    pub reclaimable_bytes: i64,
    pub freed_bytes: i64,
    pub entries: Vec<CacheEntry>,
}

fn now() -> SystemTime {
    SystemTime::now()
}

/// Total size (bytes) and most-recent mtime under `dir`, in one walk.
fn measure(dir: &Path) -> (i64, SystemTime) {
    let mut size: i64 = 0;
    let mut newest = SystemTime::UNIX_EPOCH;
    for entry in WalkDir::new(dir).follow_links(false).into_iter().flatten() {
        if let Ok(md) = entry.metadata() {
            if md.is_file() {
                size += md.len() as i64;
            }
            if let Ok(m) = md.modified() {
                if m > newest {
                    newest = m;
                }
            }
        }
    }
    (size, newest)
}

/// Scan known caches. Includes an entry when it exists, is non-empty, and has
/// been idle for at least `min_idle_days` (0 = include regardless of age).
pub fn scan(min_idle_days: u64) -> StowResult<Vec<CacheEntry>> {
    let home = dirs::home_dir()
        .ok_or_else(|| StowError::Io("cannot resolve home directory".into()))?;
    let now = now();
    let mut out = Vec::new();
    for (name, rel, regen) in KNOWN {
        let path: PathBuf = home.join(rel);
        if !path.exists() {
            continue;
        }
        let (size, newest) = measure(&path);
        if size == 0 {
            continue;
        }
        let idle_secs = now.duration_since(newest).map(|d| d.as_secs()).unwrap_or(0);
        let idle_days = (idle_secs / 86_400) as i64;
        if (idle_days as u64) < min_idle_days {
            continue;
        }
        out.push(CacheEntry {
            name: (*name).to_string(),
            path: path.to_string_lossy().into_owned(),
            size_bytes: size,
            idle_days,
            regenerates: (*regen).to_string(),
            removed: false,
        });
    }
    out.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    Ok(out)
}

/// Scan, and (when `apply`) delete each qualifying cache. Returns a report with
/// per-entry sizes and the total freed.
pub fn clean(min_idle_days: u64, apply: bool) -> StowResult<CleanReport> {
    let mut entries = scan(min_idle_days)?;
    let reclaimable_bytes: i64 = entries.iter().map(|e| e.size_bytes).sum();
    let mut freed: i64 = 0;
    if apply {
        for e in entries.iter_mut() {
            match std::fs::remove_dir_all(&e.path) {
                Ok(_) => {
                    e.removed = true;
                    freed += e.size_bytes;
                }
                Err(err) => {
                    // Leave removed=false; surface nothing fatal — a busy cache
                    // (e.g. open by a running tool) just stays.
                    let _ = err;
                }
            }
        }
    }
    Ok(CleanReport {
        min_idle_days,
        applied: apply,
        reclaimable_bytes,
        freed_bytes: freed,
        entries,
    })
}
