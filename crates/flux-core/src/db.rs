use std::{path::Path, sync::Mutex, time::{SystemTime, UNIX_EPOCH}};

use flux_api::Storage;
use rusqlite::{Connection, OptionalExtension, params};

/// Flux's SQLite database: command history and extension storage
/// (always isolated by extension name).
pub struct Db {
    conn: Mutex<Connection>,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Microseconds, so two searches recorded within the same second still order correctly.
fn now_micros() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0)
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS history (
                 command_id TEXT PRIMARY KEY, count INTEGER NOT NULL, last_used INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS kv (
                 ext TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY (ext, key));
             CREATE TABLE IF NOT EXISTS prefs (
                 ext TEXT NOT NULL, name TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY (ext, name));
             CREATE TABLE IF NOT EXISTS search_history (
                 query TEXT PRIMARY KEY, count INTEGER NOT NULL, last_used INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS extension_state (
                 name TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 1, version TEXT,
                 installed_at INTEGER, source TEXT);
             CREATE TABLE IF NOT EXISTS cache (
                 ext TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, expires_at INTEGER NOT NULL,
                 PRIMARY KEY (ext, key));",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Option<T> {
        let conn = self.conn.lock().ok()?;
        f(&conn).map_err(|e| log::error!("sqlite: {e}")).ok()
    }

    // ----------------------------------------------------- extension state

    pub fn set_extension_enabled(&self, name: &str, enabled: bool) {
        self.with(|c| {
            c.execute(
                "INSERT INTO extension_state (name, enabled) VALUES (?1, ?2)
                 ON CONFLICT(name) DO UPDATE SET enabled = ?2",
                params![name, enabled as i64],
            )
        });
    }

    pub fn extension_enabled(&self, name: &str) -> bool {
        self.with(|c| c.query_row("SELECT enabled FROM extension_state WHERE name = ?1", params![name], |r| r.get::<_, i64>(0)).optional())
            .flatten()
            .map(|v| v != 0)
            .unwrap_or(true)
    }

    pub fn disabled_extensions(&self) -> std::collections::HashSet<String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT name FROM extension_state WHERE enabled = 0")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<_>>()
        })
        .unwrap_or_default()
    }

    pub fn record_install(&self, name: &str, version: &str, source: &str) {
        self.with(|c| {
            c.execute(
                "INSERT INTO extension_state (name, enabled, version, installed_at, source) VALUES (?1, 1, ?2, ?3, ?4)
                 ON CONFLICT(name) DO UPDATE SET version = ?2, installed_at = ?3, source = ?4",
                params![name, version, now(), source],
            )
        });
    }

    /// Forgets an extension's management row and, with `purge`, everything it stored.
    pub fn forget_extension(&self, name: &str, purge: bool) {
        self.with(|c| {
            c.execute("DELETE FROM extension_state WHERE name = ?1", params![name])?;
            if purge {
                for table in ["kv", "prefs", "cache"] {
                    c.execute(&format!("DELETE FROM {table} WHERE ext = ?1"), params![name])?;
                }
            }
            Ok(())
        });
    }

    pub fn record_use(&self, command_id: &str) {
        self.with(|c| {
            c.execute(
                "INSERT INTO history (command_id, count, last_used) VALUES (?1, 1, ?2)
                 ON CONFLICT(command_id) DO UPDATE SET count = count + 1, last_used = ?2",
                params![command_id, now()],
            )
        });
    }

    pub fn record_search(&self, query: &str) {
        let q = query.trim();
        if q.is_empty() {
            return;
        }
        self.with(|c| {
            c.execute(
                "INSERT INTO search_history (query, count, last_used) VALUES (?1, 1, ?2)
                 ON CONFLICT(query) DO UPDATE SET count = count + 1, last_used = ?2",
                params![q, now_micros()],
            )
        });
    }

    /// Most recent searches first.
    pub fn recent_searches(&self, limit: usize) -> Vec<String> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT query FROM search_history ORDER BY last_used DESC, rowid DESC LIMIT ?1")?;
            stmt.query_map(params![limit as i64], |r| r.get(0))?.collect()
        })
        .unwrap_or_default()
    }

    /// `(command_id, count, last_used)`
    pub fn history(&self) -> Vec<(String, u32, i64)> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT command_id, count, last_used FROM history")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect()
        })
        .unwrap_or_default()
    }
}

impl Storage for Db {
    fn local_get(&self, ext: &str, key: &str) -> Option<String> {
        self.with(|c| c.query_row("SELECT value FROM kv WHERE ext=?1 AND key=?2", params![ext, key], |r| r.get(0)).optional()).flatten()
    }
    fn local_set(&self, ext: &str, key: &str, json: &str) {
        self.with(|c| c.execute("INSERT OR REPLACE INTO kv (ext, key, value) VALUES (?1, ?2, ?3)", params![ext, key, json]));
    }
    fn local_remove(&self, ext: &str, key: &str) {
        self.with(|c| c.execute("DELETE FROM kv WHERE ext=?1 AND key=?2", params![ext, key]));
    }
    fn local_clear(&self, ext: &str) {
        self.with(|c| c.execute("DELETE FROM kv WHERE ext=?1", params![ext]));
    }

    fn pref_get(&self, ext: &str, name: &str) -> Option<String> {
        self.with(|c| c.query_row("SELECT value FROM prefs WHERE ext=?1 AND name=?2", params![ext, name], |r| r.get(0)).optional()).flatten()
    }
    fn pref_set(&self, ext: &str, name: &str, json: &str) {
        self.with(|c| c.execute("INSERT OR REPLACE INTO prefs (ext, name, value) VALUES (?1, ?2, ?3)", params![ext, name, json]));
    }

    fn cache_get(&self, ext: &str, key: &str) -> Option<String> {
        self.with(|c| {
            c.execute("DELETE FROM cache WHERE expires_at <= ?1", params![now()])?;
            c.query_row("SELECT value FROM cache WHERE ext=?1 AND key=?2", params![ext, key], |r| r.get(0)).optional()
        })
        .flatten()
    }
    fn cache_set(&self, ext: &str, key: &str, json: &str, ttl_secs: u64) {
        self.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO cache (ext, key, value, expires_at) VALUES (?1, ?2, ?3, ?4)",
                params![ext, key, json, now() + ttl_secs as i64],
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_is_isolated_per_extension() {
        let db = Db::open_in_memory().unwrap();
        db.local_set("a", "k", "1");
        db.local_set("b", "k", "2");
        assert_eq!(db.local_get("a", "k").as_deref(), Some("1"));
        db.local_clear("a");
        assert_eq!(db.local_get("a", "k"), None);
        assert_eq!(db.local_get("b", "k").as_deref(), Some("2"));
    }

    #[test]
    fn cache_expires() {
        let db = Db::open_in_memory().unwrap();
        db.cache_set("a", "k", "1", 100);
        assert_eq!(db.cache_get("a", "k").as_deref(), Some("1"));
        db.cache_set("a", "k", "1", 0);
        assert_eq!(db.cache_get("a", "k"), None);
    }

    #[test]
    fn preferences_persist_across_reopen_and_are_scoped_per_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flux.db");
        {
            let db = Db::open(&path).unwrap();
            db.pref_set("a", "theme", "\"dark\"");
            db.pref_set("b", "theme", "\"light\"");
            db.pref_set("a", "theme", "\"solar\""); // overwrite
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.pref_get("a", "theme").as_deref(), Some("\"solar\""));
        assert_eq!(db.pref_get("b", "theme").as_deref(), Some("\"light\""));
        assert_eq!(db.pref_get("c", "theme"), None);
    }

    #[test]
    fn search_history_is_recent_first_deduplicated_and_ignores_blanks() {
        let db = Db::open_in_memory().unwrap();
        for q in ["foo", "bar", "  ", "foo", "baz"] {
            db.record_search(q);
        }
        assert_eq!(db.recent_searches(10), ["baz", "foo", "bar"]);
        assert_eq!(db.recent_searches(2), ["baz", "foo"]);
    }

    #[test]
    fn history_counts_uses() {
        let db = Db::open_in_memory().unwrap();
        db.record_use("x:y");
        db.record_use("x:y");
        assert_eq!(db.history()[0].1, 2);
    }
}
