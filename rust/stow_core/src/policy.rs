//! Auto-offload candidate scanning. Walks the configured roots and selects
//! files that are large enough and stale enough per the policy, skipping
//! anything risky (hidden files, app/package bundles, excluded paths, files
//! already offloaded). Read-only — selection only; the engine does the moving.
//!
//! Usage signal: the primary "last used" is Spotlight's `kMDItemLastUsedDate`
//! (when a file was last opened via LaunchServices) — far more reliable than
//! filesystem atime, which macOS often doesn't update (relatime/noatime). We
//! fall back to `max(atime, mtime)` only when Spotlight has no value (e.g.
//! Spotlight disabled on the volume, or never-opened files).

use crate::config::Config;
use crate::engine;
use crate::error::StowResult;
use serde::Serialize;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;
use walkdir::{DirEntry, WalkDir};

#[derive(Serialize, Clone)]
pub struct Candidate {
    pub path: String,
    pub size: i64,
    pub days_unused: i64,
    /// "spotlight" if last-used came from kMDItemLastUsedDate, else "filesystem".
    pub signal: String,
    /// Spotlight open count, when available (informational).
    pub use_count: Option<i64>,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Filesystem fallback: the more recent of access-time and modified-time. Using
/// the max is conservative — if either says "recent", we treat it as recent.
fn fs_last_used(md: &std::fs::Metadata) -> i64 {
    md.atime().max(md.mtime())
}

fn name(entry: &DirEntry) -> &str {
    entry.file_name().to_str().unwrap_or("")
}

/// Prune directory subtrees we never descend into (hidden, bundles, excluded).
/// With `include_hidden`, dotted dirs like `~/.cache` ARE scanned — except the
/// sensitive/credential ones, which are never touched.
fn should_descend(entry: &DirEntry, excludes: &[String], include_hidden: bool) -> bool {
    if !entry.file_type().is_dir() {
        return true;
    }
    let n = name(entry);
    if is_sensitive_dir(n) {
        return false; // never, even with include_hidden
    }
    if n.starts_with('.') && !include_hidden {
        return false;
    }
    if is_package_dir(n) {
        return false;
    }
    let path = entry.path().to_string_lossy();
    !excludes.iter().any(|e| path.contains(e.as_str()))
}

/// Dirs we refuse to offload from even when `include_hidden` is on: credentials,
/// keys, and VCS/trash where stubbing a file would be dangerous or confusing.
fn is_sensitive_dir(n: &str) -> bool {
    const SENSITIVE: &[&str] = &[
        ".ssh",
        ".aws",
        ".gnupg",
        ".password-store",
        ".kube",
        ".docker",
        ".git",
        ".Trash",
        ".Trashes",
    ];
    SENSITIVE.contains(&n)
}

/// True for directory names that are really opaque packages, not folders.
fn is_package_dir(n: &str) -> bool {
    const PKG_EXT: &[&str] = &[
        ".app",
        ".photoslibrary",
        ".imovielibrary",
        ".tvlibrary",
        ".musiclibrary",
        ".bundle",
        ".framework",
        ".aplibrary",
    ];
    PKG_EXT.iter().any(|ext| n.ends_with(ext))
}

fn path_has_hidden_component(p: &Path) -> bool {
    p.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| s.starts_with('.'))
            .unwrap_or(false)
    })
}

/// Parse a Spotlight raw date like "2026-05-30 18:42:11 +0000" into epoch secs.
/// Returns None for "(null)" / unparseable.
fn parse_mdls_date(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() || s == "(null)" {
        return None;
    }
    // "YYYY-MM-DD HH:MM:SS +ZZZZ"
    let mut parts = s.split_whitespace();
    let date = parts.next()?;
    let time = parts.next()?;
    let tz = parts.next().unwrap_or("+0000");

    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;

    let mut t = time.split(':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mm: i64 = t.next()?.parse().ok()?;
    let ss: i64 = t.next().unwrap_or("0").parse().ok()?;

    // Timezone offset like +0000 / -0700.
    let tz_secs = parse_tz(tz);

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss - tz_secs)
}

fn parse_tz(tz: &str) -> i64 {
    let b = tz.as_bytes();
    if b.len() < 5 {
        return 0;
    }
    let sign = if b[0] == b'-' { -1 } else { 1 };
    let h: i64 = tz[1..3].parse().unwrap_or(0);
    let m: i64 = tz[3..5].parse().unwrap_or(0);
    sign * (h * 3600 + m * 60)
}

/// Days since Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Query Spotlight for last-used date + use count in one call.
/// Returns (last_used_epoch, use_count) where each may be None.
fn spotlight(path: &str) -> (Option<i64>, Option<i64>) {
    let out = Command::new("mdls")
        .args([
            "-raw",
            "-name",
            "kMDItemLastUsedDate",
            "-name",
            "kMDItemUseCount",
            path,
        ])
        .output();
    let Ok(out) = out else {
        return (None, None);
    };
    if !out.status.success() {
        return (None, None);
    }
    // With multiple -name and -raw, values are NUL-separated in attribute order.
    let raw = String::from_utf8_lossy(&out.stdout);
    let mut fields = raw.split('\0');
    let last_used = fields.next().and_then(parse_mdls_date);
    let use_count = fields
        .next()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "(null)")
        .and_then(|s| s.parse::<i64>().ok());
    (last_used, use_count)
}

/// Select offload candidates across all configured roots, largest first.
pub fn scan(cfg: &Config) -> StowResult<Vec<Candidate>> {
    let pol = &cfg.policy;
    let min_size = pol.min_size_bytes;
    let max_age_secs = pol.min_age_days as i64 * 86_400;
    let include_hidden = pol.include_hidden;
    let now = now_secs();

    // Phase 1 (cheap): walk + filter by size/exclusions/placeholder.
    struct Pre {
        path: String,
        size: i64,
        fs_used: i64,
    }
    let mut pre: Vec<Pre> = Vec::new();

    for root in &pol.roots {
        let root_path = Path::new(root);
        if !root_path.exists() {
            continue;
        }
        let walker = WalkDir::new(root_path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| should_descend(e, &pol.excludes, include_hidden));

        for entry in walker.flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if !include_hidden && path_has_hidden_component(path) {
                continue;
            }
            let path_str = path.to_string_lossy();
            if pol.excludes.iter().any(|e| path_str.contains(e.as_str())) {
                continue;
            }
            let md = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if md.len() < min_size {
                continue;
            }
            if engine::is_placeholder(path).unwrap_or(false) {
                continue;
            }
            pre.push(Pre {
                path: path_str.into_owned(),
                size: md.len() as i64,
                fs_used: fs_last_used(&md),
            });
        }
    }

    // Phase 2 (Spotlight): only for size-qualifying files (a small set).
    let mut out: Vec<Candidate> = Vec::new();
    for p in pre {
        let (sl_used, use_count) = spotlight(&p.path);
        let (last_used, signal) = match sl_used {
            Some(t) => (t, "spotlight"),
            None => (p.fs_used, "filesystem"),
        };
        let age = now - last_used;
        if age < max_age_secs {
            continue;
        }
        out.push(Candidate {
            path: p.path,
            size: p.size,
            days_unused: age / 86_400,
            signal: signal.to_string(),
            use_count,
        });
    }

    out.sort_by(|a, b| b.size.cmp(&a.size));
    Ok(out)
}
