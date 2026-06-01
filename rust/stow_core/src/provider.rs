//! File Provider domain backing store + operations.
//!
//! This is the data model behind the "Stow" folder that appears in Finder. The
//! extension calls these functions (over the C FFI) to enumerate, fetch, create,
//! and delete items. State lives in a SQLite DB inside the **App Group container**
//! so the sandboxed extension and the agent/CLI all share it.
//!
//! Content is content-addressed in S3 (key = "fp/<blake3>"), so identical files
//! dedup automatically. A file is "dataless" when its bytes live only in S3;
//! `fetch` downloads them on demand (this is the rehydrate-on-open path).

use crate::config::Config;
use crate::error::{StowError, StowResult};
use crate::s3;
use rusqlite::{params, Connection};
use serde::Serialize;

/// Root container identifier, matching NSFileProviderRootContainerItemIdentifier.
pub const ROOT: &str = "NSFileProviderRootContainerItemIdentifier";

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub item_id: String,
    pub parent_id: String,
    pub filename: String,
    pub is_folder: bool,
    pub size: i64,
    pub content_type: String,
    pub hash: Option<String>,
    pub s3_key: Option<String>,
    /// Monotonic version bumped on every content change (drives NSFileProvider).
    pub version: i64,
    pub modified_at: i64,
    /// Last time the file was created, modified, or read (via `fetch`). This is
    /// the "last touched" signal the auto-evictor uses for staleness — the
    /// sandboxed agent can't stat the user-visible CloudStorage path, so the DB
    /// carries the access time instead.
    pub last_access: i64,
    /// True when the bytes are in S3 and not materialized locally.
    pub dataless: bool,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Append a trace line to the shared App Group container so we can see how far
/// the (sandboxed) extension gets without privileged logs.
fn rtrace(s: &str) {
    if let Ok(dir) = std::env::var("STOW_GROUP_DIR") {
        let path = std::path::Path::new(&dir).join("ext-trace.log");
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "[rust] {s}");
        }
    }
}

/// Path to the File Provider DB in the App Group container. Falls back to the
/// support dir when the group container can't be resolved (e.g. CLI on a host
/// without the entitlement) so tests and tooling still work.
fn db_path() -> StowResult<std::path::PathBuf> {
    // Same shared store as config (App Group container via STOW_GROUP_DIR when
    // sandboxed). Keeps the extension and CLI pointed at one DB.
    let dir = Config::support_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| StowError::Io(e.to_string()))?;
    Ok(dir.join("provider.db"))
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open() -> StowResult<Store> {
        let p = db_path()?;
        let conn = Connection::open(&p).map_err(|e| StowError::Io(e.to_string()))?;
        // NOT WAL: a File Provider extension's sandbox forbids the mmap-backed
        // `-shm` shared-memory file WAL requires, so WAL → SQLITE_CANTOPEN inside
        // the extension. TRUNCATE uses a plain rollback journal (no shared
        // memory); cross-process safe here given busy_timeout + our tiny write
        // rate (a few inserts on createItem, reads on enumerate).
        conn.execute_batch(
            "PRAGMA journal_mode=TRUNCATE;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS fp_items (
                item_id      TEXT PRIMARY KEY,
                parent_id    TEXT NOT NULL,
                filename     TEXT NOT NULL,
                is_folder    INTEGER NOT NULL,
                size         INTEGER NOT NULL,
                content_type TEXT NOT NULL,
                hash         TEXT,
                s3_key       TEXT,
                version      INTEGER NOT NULL,
                modified_at  INTEGER NOT NULL,
                last_access  INTEGER NOT NULL DEFAULT 0,
                dataless     INTEGER NOT NULL DEFAULT 1
             );
             CREATE INDEX IF NOT EXISTS idx_fp_parent ON fp_items(parent_id);",
        )
        .map_err(|e| StowError::Io(e.to_string()))?;
        // Migrate older DBs that predate last_access (ignore "duplicate column").
        let _ = conn.execute_batch(
            "ALTER TABLE fp_items ADD COLUMN last_access INTEGER NOT NULL DEFAULT 0;");
        Ok(Store { conn })
    }

    fn row_to_item(row: &rusqlite::Row) -> rusqlite::Result<Item> {
        Ok(Item {
            item_id: row.get(0)?,
            parent_id: row.get(1)?,
            filename: row.get(2)?,
            is_folder: row.get::<_, i64>(3)? != 0,
            size: row.get(4)?,
            content_type: row.get(5)?,
            hash: row.get(6)?,
            s3_key: row.get(7)?,
            version: row.get(8)?,
            modified_at: row.get(9)?,
            last_access: row.get(10)?,
            dataless: row.get::<_, i64>(11)? != 0,
        })
    }

    pub fn children(&self, parent_id: &str) -> StowResult<Vec<Item>> {
        let mut stmt = self
            .conn
            .prepare("SELECT item_id,parent_id,filename,is_folder,size,content_type,hash,s3_key,version,modified_at,last_access,dataless FROM fp_items WHERE parent_id=?1 ORDER BY filename")
            .map_err(|e| StowError::Io(e.to_string()))?;
        let rows = stmt
            .query_map(params![parent_id], Self::row_to_item)
            .map_err(|e| StowError::Io(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StowError::Io(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn get(&self, item_id: &str) -> StowResult<Option<Item>> {
        let mut stmt = self
            .conn
            .prepare("SELECT item_id,parent_id,filename,is_folder,size,content_type,hash,s3_key,version,modified_at,last_access,dataless FROM fp_items WHERE item_id=?1")
            .map_err(|e| StowError::Io(e.to_string()))?;
        let mut rows = stmt
            .query(params![item_id])
            .map_err(|e| StowError::Io(e.to_string()))?;
        match rows.next().map_err(|e| StowError::Io(e.to_string()))? {
            Some(row) => Ok(Some(Self::row_to_item(row).map_err(|e| StowError::Io(e.to_string()))?)),
            None => Ok(None),
        }
    }

    pub fn upsert(&self, it: &Item) -> StowResult<()> {
        self.conn
            .execute(
                "INSERT INTO fp_items (item_id,parent_id,filename,is_folder,size,content_type,hash,s3_key,version,modified_at,last_access,dataless)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
                 ON CONFLICT(item_id) DO UPDATE SET
                   parent_id=excluded.parent_id, filename=excluded.filename,
                   is_folder=excluded.is_folder, size=excluded.size,
                   content_type=excluded.content_type, hash=excluded.hash,
                   s3_key=excluded.s3_key, version=excluded.version,
                   modified_at=excluded.modified_at, last_access=excluded.last_access,
                   dataless=excluded.dataless",
                params![it.item_id, it.parent_id, it.filename, it.is_folder as i64,
                        it.size, it.content_type, it.hash, it.s3_key, it.version,
                        it.modified_at, it.last_access, it.dataless as i64],
            )
            .map_err(|e| StowError::Io(e.to_string()))?;
        Ok(())
    }

    pub fn delete(&self, item_id: &str) -> StowResult<()> {
        self.conn
            .execute("DELETE FROM fp_items WHERE item_id=?1", params![item_id])
            .map_err(|e| StowError::Io(e.to_string()))?;
        Ok(())
    }

    pub fn set_dataless(&self, item_id: &str, dataless: bool) -> StowResult<()> {
        self.conn
            .execute("UPDATE fp_items SET dataless=?2 WHERE item_id=?1", params![item_id, dataless as i64])
            .map_err(|e| StowError::Io(e.to_string()))?;
        Ok(())
    }

    /// Record that an item was just read — resets the "untouched" clock the
    /// auto-evictor uses, so actively-used files are never offloaded.
    pub fn touch_access(&self, item_id: &str, ts: i64) -> StowResult<()> {
        self.conn
            .execute("UPDATE fp_items SET last_access=?2 WHERE item_id=?1", params![item_id, ts])
            .map_err(|e| StowError::Io(e.to_string()))?;
        Ok(())
    }
}

// ---- high-level operations (used by the FFI) --------------------------------

fn new_id() -> String {
    // Unique-ish id from time + a counter-ish nonce (no rand in this crate).
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("itm-{}-{}", now(), n)
}

/// Create a file item: read the source bytes, upload to S3 (content-addressed),
/// and record it. Returns the new item. `temp_path` empty => a folder.
pub fn create(parent_id: &str, filename: &str, is_folder: bool, temp_path: &str) -> StowResult<Item> {
    rtrace("create: loading config");
    let cfg = Config::load()?
        .ok_or_else(|| StowError::InvalidConfig("not initialized — run `stow init`".into()))?;
    rtrace("create: config loaded; opening store");
    let store = Store::open()?;
    rtrace("create: store opened");
    let id = new_id();

    if is_folder {
        let it = Item {
            item_id: id, parent_id: parent_id.to_string(), filename: filename.to_string(),
            is_folder: true, size: 0, content_type: "public.folder".into(),
            hash: None, s3_key: None, version: 1, modified_at: now(),
            last_access: now(), dataless: false,
        };
        store.upsert(&it)?;
        return Ok(it);
    }

    rtrace(&format!("create: reading temp {temp_path}"));
    let data = std::fs::read(temp_path).map_err(|e| StowError::Io(format!("{temp_path}: {e}")))?;
    let size = data.len() as i64;
    rtrace(&format!("create: read {size} bytes; hashing"));
    let hash = blake3::hash(&data).to_hex().to_string();
    let s3_key = format!("fp/{hash}");
    rtrace(&format!("create: have creds={}; building runtime", cfg.creds().is_some()));

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()
        .map_err(|e| StowError::Unknown(e.to_string()))?;
    rtrace("create: runtime built; entering block_on");
    rt.block_on(async {
        rtrace("create: building S3 client");
        let c = s3::client(&cfg.region, cfg.creds()).await?;
        rtrace("create: S3 client ready; put_object");
        let r = s3::put_object(&c, &cfg.bucket, &s3_key, data).await;
        rtrace(&format!("create: put_object returned ok={}", r.is_ok()));
        r
    })?;

    let it = Item {
        item_id: id, parent_id: parent_id.to_string(), filename: filename.to_string(),
        is_folder: false, size, content_type: guess_type(filename),
        hash: Some(hash), s3_key: Some(s3_key), version: 1, modified_at: now(),
        last_access: now(), dataless: false, // freshly created => materialized
    };
    store.upsert(&it)?;
    Ok(it)
}

/// Replace an item's contents (modifyItem with new bytes).
pub fn update_contents(item_id: &str, temp_path: &str) -> StowResult<Item> {
    let cfg = Config::load()?
        .ok_or_else(|| StowError::InvalidConfig("not initialized".into()))?;
    let store = Store::open()?;
    let mut it = store.get(item_id)?
        .ok_or_else(|| StowError::NotFound(item_id.to_string()))?;
    let data = std::fs::read(temp_path).map_err(|e| StowError::Io(format!("{temp_path}: {e}")))?;
    let size = data.len() as i64;
    let hash = blake3::hash(&data).to_hex().to_string();
    let s3_key = format!("fp/{hash}");
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()
        .map_err(|e| StowError::Unknown(e.to_string()))?;
    rt.block_on(async {
        let c = s3::client(&cfg.region, cfg.creds()).await?;
        s3::put_object(&c, &cfg.bucket, &s3_key, data).await
    })?;
    it.size = size;
    it.hash = Some(hash);
    it.s3_key = Some(s3_key);
    it.version += 1;
    it.modified_at = now();
    it.last_access = now();
    it.dataless = false;
    store.upsert(&it)?;
    Ok(it)
}

/// Download an item's bytes from S3 to `out_path` (rehydrate-on-open). Verifies
/// the content hash, then marks the item materialized.
pub fn fetch(item_id: &str, out_path: &str) -> StowResult<Item> {
    let cfg = Config::load()?
        .ok_or_else(|| StowError::InvalidConfig("not initialized".into()))?;
    let store = Store::open()?;
    let it = store.get(item_id)?
        .ok_or_else(|| StowError::NotFound(item_id.to_string()))?;
    let key = it.s3_key.clone()
        .ok_or_else(|| StowError::NotFound(format!("{item_id} has no s3 object")))?;

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()
        .map_err(|e| StowError::Unknown(e.to_string()))?;
    let data = rt.block_on(async {
        let c = s3::client(&cfg.region, cfg.creds()).await?;
        s3::get_object(&c, &cfg.bucket, &key).await
    })?;

    if let Some(h) = &it.hash {
        let got = blake3::hash(&data).to_hex().to_string();
        if &got != h {
            return Err(StowError::Integrity(format!("{item_id}: hash mismatch")));
        }
    }
    std::fs::write(out_path, &data).map_err(|e| StowError::Io(e.to_string()))?;
    store.set_dataless(item_id, false)?;
    // Reading is an access event — reset the staleness clock so files in active
    // use are never auto-evicted.
    store.touch_access(item_id, now())?;
    Ok(it)
}

fn guess_type(filename: &str) -> String {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "txt" => "public.plain-text",
        "pdf" => "com.adobe.pdf",
        "png" => "public.png",
        "jpg" | "jpeg" => "public.jpeg",
        "mov" => "com.apple.quicktime-movie",
        "mp4" => "public.mpeg-4",
        "zip" => "public.zip-archive",
        _ => "public.data",
    }
    .to_string()
}
