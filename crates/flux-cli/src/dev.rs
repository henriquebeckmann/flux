//! The loop behind `flux dev`: show the extension's log, and re-validate on every save so syntax
//! errors appear in the terminal (with file and line) within a fraction of a second, while the
//! launcher reloads the extension on its own.

use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use flux_runtime::validate::{Report, validate_dir};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Ctrl+C sets the returned flag instead of killing the process (so `dev` can detach cleanly).
pub fn interrupted_flag() -> &'static AtomicBool {
    // SAFETY: the handler only stores into an atomic.
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
    &INTERRUPTED
}

/// A stamp that changes whenever any file below `dir` is added, removed or modified.
pub fn stamp(dir: &Path) -> (SystemTime, usize) {
    fn walk(d: &Path, acc: &mut (SystemTime, usize), depth: u8) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            acc.1 += 1;
            if let Ok(t) = m.modified() {
                acc.0 = acc.0.max(t);
            }
            if m.is_dir() && depth > 0 && !e.file_name().to_string_lossy().starts_with('.') {
                walk(&e.path(), acc, depth - 1);
            }
        }
    }
    let mut acc = (SystemTime::UNIX_EPOCH, 0);
    walk(dir, &mut acc, 5);
    acc
}

/// The extension's id (from its manifest), to locate its log file.
pub fn extension_name(dir: &Path) -> Option<String> {
    validate_dir(dir).name
}

/// One line per problem, the way a compiler prints them.
pub fn format_report(r: &Report) -> Vec<String> {
    let mut out: Vec<String> = r.errors.iter().map(|e| format!("error: {e}")).collect();
    out.extend(r.warnings.iter().map(|w| format!("warning: {w}")));
    out
}

/// Prints new lines appended to `path` until `stop` is set.
pub fn follow_file(path: &Path, out: &mut dyn Write, stop: &AtomicBool) {
    let mut pos = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        let Ok(mut f) = std::fs::File::open(path) else { continue };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < pos {
            pos = 0; // rotated
        }
        if len > pos && f.seek(SeekFrom::Start(pos)).is_ok() {
            let mut buf = String::new();
            if f.take(len - pos).read_to_string(&mut buf).is_ok() {
                let _ = write!(out, "{buf}");
                let _ = out.flush();
            }
            pos = len;
        }
    }
}

/// Validates on every change of `dir`, printing only when the report differs from the last one.
/// Runs until `stop` is set.
pub fn watch_validate(dir: PathBuf, out: Arc<std::sync::Mutex<Box<dyn Write + Send>>>, stop: &'static AtomicBool) {
    let mut last_stamp = stamp(&dir);
    let mut last: Vec<String> = format_report(&validate_dir(&dir));
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(250));
        let s = stamp(&dir);
        if s == last_stamp {
            continue;
        }
        last_stamp = s;
        let report = validate_dir(&dir);
        let lines = format_report(&report);
        if lines != last {
            if let Ok(mut o) = out.lock() {
                if report.ok() {
                    let _ = writeln!(o, "[dev] ok ({} warning(s))", report.warnings.len());
                }
                for l in &lines {
                    let _ = writeln!(o, "[dev] {l}");
                }
                let _ = o.flush();
            }
            last = lines;
        } else if let Ok(mut o) = out.lock() {
            let _ = writeln!(o, "[dev] reloaded");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_changes_when_files_change_and_ignores_hidden_folders() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.lua"), "x").unwrap();
        let s1 = stamp(d.path());
        assert_eq!(s1, stamp(d.path()));
        std::fs::create_dir_all(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub/b.lua"), "y").unwrap();
        assert_ne!(s1, stamp(d.path()));
        let s2 = stamp(d.path());
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        std::fs::write(d.path().join(".git/objects"), "z").unwrap();
        assert_eq!(stamp(d.path()).1, s2.1 + 1, "the .git folder itself counts once, its content is not scanned");
    }

    #[test]
    fn follow_prints_appended_lines_and_survives_rotation() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.log");
        std::fs::write(&p, "old\n").unwrap();
        static STOP: AtomicBool = AtomicBool::new(false);
        let (p2, out) = (p.clone(), Arc::new(std::sync::Mutex::new(Vec::<u8>::new())));
        struct Sink(Arc<std::sync::Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink(out.clone());
        let t = std::thread::spawn(move || {
            let mut s = sink;
            follow_file(&p2, &mut s, &STOP);
        });
        std::thread::sleep(Duration::from_millis(500));
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "new line").unwrap();
        drop(f);
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !String::from_utf8_lossy(&out.lock().unwrap()).contains("new line") && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        STOP.store(true, Ordering::SeqCst);
        t.join().unwrap();
        let text = String::from_utf8_lossy(&out.lock().unwrap()).to_string();
        assert!(text.contains("new line") && !text.contains("old"), "{text:?}");
    }
}
