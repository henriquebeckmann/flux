//! Per-extension logs and errors: an in-memory ring for the launcher UI plus append-only files
//! (written by a background thread, rotated at 1 MB) for `flux logs` and for debugging after a restart.

use std::{
    collections::{HashMap, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
    time::{SystemTime, UNIX_EPOCH},
};

use flux_api::LogLevel;

const RING: usize = 1000;
const MAX_FILE: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    pub ts_ms: i64,
    pub level: LogLevel,
    pub message: String,
}

impl LogLine {
    pub fn render(&self) -> String {
        format!("{} {:<5} {}", fmt_ts(self.ts_ms), format!("{:?}", self.level).to_uppercase(), self.message)
    }
}

/// `2026-11-08T12:34:56Z` without a date-time dependency (civil-from-days algorithm).
pub fn fmt_ts(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[derive(Default)]
struct Ring {
    lines: VecDeque<LogLine>,
    errors: VecDeque<LogLine>,
}

pub struct ExtLogs {
    rings: Mutex<HashMap<String, Ring>>,
    files: Option<mpsc::Sender<(String, String)>>,
}

pub fn log_path(dir: &Path, ext: &str) -> PathBuf {
    dir.join(format!("{ext}.log"))
}

impl ExtLogs {
    /// Keeps lines in memory only (tests, tools).
    pub fn in_memory() -> Self {
        Self { rings: Mutex::new(HashMap::new()), files: None }
    }

    /// Keeps lines in memory and appends them to `<dir>/<extension>.log`.
    pub fn with_files(dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<(String, String)>();
        std::thread::Builder::new()
            .name("flux-logs".into())
            .spawn(move || {
                let _ = std::fs::create_dir_all(&dir);
                while let Ok((ext, line)) = rx.recv() {
                    let path = log_path(&dir, &ext);
                    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_FILE) {
                        let _ = std::fs::rename(&path, path.with_extension("log.1"));
                    }
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                        let _ = writeln!(f, "{line}");
                    }
                }
            })
            .ok();
        Self { rings: Mutex::new(HashMap::new()), files: Some(tx) }
    }

    pub fn push(&self, ext: &str, level: LogLevel, message: &str) {
        self.push_at(ext, level, message, now_ms());
    }

    pub fn push_at(&self, ext: &str, level: LogLevel, message: &str, ts_ms: i64) {
        let line = LogLine { ts_ms, level, message: message.replace('\n', "\\n") };
        if let Some(tx) = &self.files {
            let _ = tx.send((ext.to_string(), line.render()));
        }
        let mut rings = self.rings.lock().unwrap();
        let ring = rings.entry(ext.to_string()).or_default();
        if level == LogLevel::Error {
            ring.errors.push_back(line.clone());
            if ring.errors.len() > 100 {
                ring.errors.pop_front();
            }
        }
        ring.lines.push_back(line);
        if ring.lines.len() > RING {
            ring.lines.pop_front();
        }
    }

    /// Last `n` lines, oldest first.
    pub fn lines(&self, ext: &str, n: usize) -> Vec<LogLine> {
        let rings = self.rings.lock().unwrap();
        rings.get(ext).map(|r| r.lines.iter().rev().take(n).rev().cloned().collect()).unwrap_or_default()
    }

    pub fn errors(&self, ext: &str) -> Vec<LogLine> {
        self.rings.lock().unwrap().get(ext).map(|r| r.errors.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn last_error(&self, ext: &str) -> Option<LogLine> {
        self.rings.lock().unwrap().get(ext).and_then(|r| r.errors.back().cloned())
    }

    pub fn clear(&self, ext: &str) {
        self.rings.lock().unwrap().remove(ext);
    }
}

/// Last `n` lines of an extension's log file (what `flux logs` prints).
pub fn tail_file(dir: &Path, ext: &str, n: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(log_path(dir, ext)) else { return vec![] };
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_format_as_utc_iso() {
        assert_eq!(fmt_ts(0), "1970-01-01T00:00:00Z");
        assert_eq!(fmt_ts(1_000_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(fmt_ts(1_709_210_096_000), "2024-02-29T12:34:56Z"); // leap day
    }

    #[test]
    fn ring_keeps_the_latest_lines_and_separates_errors() {
        let l = ExtLogs::in_memory();
        for i in 0..1200 {
            l.push("a", LogLevel::Info, &format!("line {i}"));
        }
        l.push("a", LogLevel::Error, "boom\nsecond line");
        l.push("b", LogLevel::Warn, "other");
        let all = l.lines("a", 5000);
        assert_eq!(all.len(), RING);
        assert_eq!(all.last().unwrap().message, "boom\\nsecond line");
        assert_eq!(l.lines("a", 2).len(), 2);
        assert_eq!(l.errors("a").len(), 1);
        assert_eq!(l.last_error("a").unwrap().message, "boom\\nsecond line");
        assert!(l.errors("b").is_empty() && l.lines("b", 10).len() == 1);
        l.clear("a");
        assert!(l.lines("a", 10).is_empty());
    }

    #[test]
    fn files_are_appended_by_the_background_writer_and_rotated() {
        let d = tempfile::tempdir().unwrap();
        let l = ExtLogs::with_files(d.path().to_path_buf());
        l.push_at("x", LogLevel::Info, "hello", 0);
        l.push_at("x", LogLevel::Error, "bad", 1000);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while tail_file(d.path(), "x", 10).len() < 2 && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let t = tail_file(d.path(), "x", 10);
        assert_eq!(t[0], "1970-01-01T00:00:00Z INFO  hello");
        assert_eq!(t[1], "1970-01-01T00:00:01Z ERROR bad");
        assert_eq!(tail_file(d.path(), "x", 1), vec![t[1].clone()]);
        // rotation
        std::fs::write(log_path(d.path(), "x"), "y".repeat(MAX_FILE as usize + 10)).unwrap();
        l.push_at("x", LogLevel::Info, "after", 2000);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !d.path().join("x.log.1").exists() && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(d.path().join("x.log.1").exists());
        assert!(tail_file(d.path(), "x", 5).iter().any(|s| s.contains("after")));
    }
}
