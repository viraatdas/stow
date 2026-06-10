//! SQLite index of offloaded files. Tracks every file Stow has offloaded so
//! `status`/`restore` work and so we never lose track of what's in S3.

use crate::config::Config;
use crate::error::{StowError, StowResult};
use rusqlite::{params, Connection};

pub struct Index {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct Record {
    pub path: String,
    pub hash: String,
    pub size: i64,
    pub mode: u32,
    pub mtime: i64,
    pub s3_key: String,
    pub offloaded_at: i64,
    /// True once the file has been brought back on disk (`stow restore`). A
    /// restored file still lives in S3 but isn't saving local space.
    pub restored: bool,
}

/// One published share link (a copy of the content under `shares/<token>/…`,
/// publicly readable). Tracked so links can be listed and revoked.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ShareRow {
    pub token: String,
    pub source: String,
    pub s3_key: String,
    pub url: String,
    pub size: i64,
    pub is_folder: bool,
    pub created_at: i64,
}

impl Index {
    /// Open (creating if needed) the index at the standard support path.
    pub fn open() -> StowResult<Index> {
        let p = Config::index_path()?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| StowError::Io(e.to_string()))?;
        }
        let conn = Connection::open(&p).map_err(|e| StowError::Io(e.to_string()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS items (
                path         TEXT PRIMARY KEY,
                hash         TEXT NOT NULL,
                size         INTEGER NOT NULL,
                mode         INTEGER NOT NULL,
                mtime        INTEGER NOT NULL,
                s3_key       TEXT NOT NULL,
                offloaded_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_items_hash ON items(hash);",
        )
        .map_err(|e| StowError::Io(e.to_string()))?;
        // Migration: `restored` tracks whether an in-place offload has been
        // brought back (1) or is still a space-saving placeholder (0). It lets
        // `status` report space saved from the DB alone — the sandboxed agent
        // can't stat the original paths to check for placeholders. Idempotent:
        // ignore the "duplicate column" error on DBs that already have it.
        let _ = conn.execute(
            "ALTER TABLE items ADD COLUMN restored INTEGER NOT NULL DEFAULT 0",
            [],
        );
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS shares (
                token      TEXT PRIMARY KEY,
                source     TEXT NOT NULL,
                s3_key     TEXT NOT NULL,
                url        TEXT NOT NULL,
                size       INTEGER NOT NULL,
                is_folder  INTEGER NOT NULL,
                created_at INTEGER NOT NULL
             );",
        )
        .map_err(|e| StowError::Io(e.to_string()))?;
        Ok(Index { conn })
    }

    pub fn upsert(&self, r: &Record) -> StowResult<()> {
        self.conn
            .execute(
                "INSERT INTO items (path, hash, size, mode, mtime, s3_key, offloaded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(path) DO UPDATE SET
                   hash=excluded.hash, size=excluded.size, mode=excluded.mode,
                   mtime=excluded.mtime, s3_key=excluded.s3_key,
                   offloaded_at=excluded.offloaded_at,
                   restored=0",
                params![r.path, r.hash, r.size, r.mode, r.mtime, r.s3_key, r.offloaded_at],
            )
            .map_err(|e| StowError::Io(e.to_string()))?;
        Ok(())
    }

    pub fn get(&self, path: &str) -> StowResult<Option<Record>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, hash, size, mode, mtime, s3_key, offloaded_at, restored FROM items WHERE path=?1")
            .map_err(|e| StowError::Io(e.to_string()))?;
        let mut rows = stmt
            .query(params![path])
            .map_err(|e| StowError::Io(e.to_string()))?;
        if let Some(row) = rows.next().map_err(|e| StowError::Io(e.to_string()))? {
            Ok(Some(row_to_record(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn remove(&self, path: &str) -> StowResult<()> {
        self.conn
            .execute("DELETE FROM items WHERE path=?1", params![path])
            .map_err(|e| StowError::Io(e.to_string()))?;
        Ok(())
    }

    /// Flag a tracked file as restored (back on disk) or offloaded again.
    pub fn set_restored(&self, path: &str, restored: bool) -> StowResult<()> {
        self.conn
            .execute(
                "UPDATE items SET restored=?2 WHERE path=?1",
                params![path, restored as i64],
            )
            .map_err(|e| StowError::Io(e.to_string()))?;
        Ok(())
    }

    // ---- shares ---------------------------------------------------------------

    pub fn add_share(&self, s: &ShareRow) -> StowResult<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO shares (token, source, s3_key, url, size, is_folder, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![s.token, s.source, s.s3_key, s.url, s.size, s.is_folder as i64, s.created_at],
            )
            .map_err(|e| StowError::Io(e.to_string()))?;
        Ok(())
    }

    pub fn get_share(&self, token: &str) -> StowResult<Option<ShareRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT token, source, s3_key, url, size, is_folder, created_at FROM shares WHERE token=?1")
            .map_err(|e| StowError::Io(e.to_string()))?;
        let mut rows = stmt.query(params![token]).map_err(|e| StowError::Io(e.to_string()))?;
        if let Some(row) = rows.next().map_err(|e| StowError::Io(e.to_string()))? {
            Ok(Some(row_to_share(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn list_shares(&self) -> StowResult<Vec<ShareRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT token, source, s3_key, url, size, is_folder, created_at FROM shares ORDER BY created_at DESC")
            .map_err(|e| StowError::Io(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ShareRow {
                    token: row.get(0)?,
                    source: row.get(1)?,
                    s3_key: row.get(2)?,
                    url: row.get(3)?,
                    size: row.get(4)?,
                    is_folder: row.get::<_, i64>(5)? != 0,
                    created_at: row.get(6)?,
                })
            })
            .map_err(|e| StowError::Io(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StowError::Io(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn remove_share(&self, token: &str) -> StowResult<()> {
        self.conn
            .execute("DELETE FROM shares WHERE token=?1", params![token])
            .map_err(|e| StowError::Io(e.to_string()))?;
        Ok(())
    }

    /// How many other rows reference the same content hash (for safe dedup-delete).
    pub fn hash_refcount(&self, hash: &str) -> StowResult<i64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM items WHERE hash=?1", params![hash], |r| r.get(0))
            .map_err(|e| StowError::Io(e.to_string()))?;
        Ok(n)
    }

    pub fn all(&self) -> StowResult<Vec<Record>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, hash, size, mode, mtime, s3_key, offloaded_at, restored FROM items ORDER BY offloaded_at DESC")
            .map_err(|e| StowError::Io(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Record {
                    path: row.get(0)?,
                    hash: row.get(1)?,
                    size: row.get(2)?,
                    mode: row.get::<_, i64>(3)? as u32,
                    mtime: row.get(4)?,
                    s3_key: row.get(5)?,
                    offloaded_at: row.get(6)?,
                    restored: row.get::<_, i64>(7)? != 0,
                })
            })
            .map_err(|e| StowError::Io(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StowError::Io(e.to_string()))?);
        }
        Ok(out)
    }
}

fn row_to_share(row: &rusqlite::Row) -> StowResult<ShareRow> {
    Ok(ShareRow {
        token: row.get(0).map_err(|e| StowError::Io(e.to_string()))?,
        source: row.get(1).map_err(|e| StowError::Io(e.to_string()))?,
        s3_key: row.get(2).map_err(|e| StowError::Io(e.to_string()))?,
        url: row.get(3).map_err(|e| StowError::Io(e.to_string()))?,
        size: row.get(4).map_err(|e| StowError::Io(e.to_string()))?,
        is_folder: row.get::<_, i64>(5).map_err(|e| StowError::Io(e.to_string()))? != 0,
        created_at: row.get(6).map_err(|e| StowError::Io(e.to_string()))?,
    })
}

fn row_to_record(row: &rusqlite::Row) -> StowResult<Record> {
    Ok(Record {
        path: row.get(0).map_err(|e| StowError::Io(e.to_string()))?,
        hash: row.get(1).map_err(|e| StowError::Io(e.to_string()))?,
        size: row.get(2).map_err(|e| StowError::Io(e.to_string()))?,
        mode: row.get::<_, i64>(3).map_err(|e| StowError::Io(e.to_string()))? as u32,
        mtime: row.get(4).map_err(|e| StowError::Io(e.to_string()))?,
        s3_key: row.get(5).map_err(|e| StowError::Io(e.to_string()))?,
        offloaded_at: row.get(6).map_err(|e| StowError::Io(e.to_string()))?,
        restored: row.get::<_, i64>(7).map_err(|e| StowError::Io(e.to_string()))? != 0,
    })
}
