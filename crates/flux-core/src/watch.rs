use std::{collections::HashMap, path::{Path, PathBuf}, sync::{Arc, Mutex}, thread, time::{Duration, SystemTime}};

use flux_api::RuntimeRequest;
use flux_runtime::RuntimeHandle;

type Stamp = (SystemTime, usize);

fn stamp(dir: &Path, depth: u8, acc: &mut Stamp) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let Ok(meta) = e.metadata() else { continue };
        acc.1 += 1;
        if let Ok(m) = meta.modified() {
            acc.0 = acc.0.max(m)
        }
        if meta.is_dir() && depth > 0 {
            stamp(&path, depth - 1, acc)
        }
    }
}

/// Folders that are extensions themselves (attached by `flux dev`), shared with the watcher thread.
pub type Singles = Arc<Mutex<Vec<PathBuf>>>;

/// One stamp per extension folder (`root/<folder>`), so a change reloads only that extension.
/// `singles` are extension folders on their own.
pub fn snapshot(roots: &[PathBuf], singles: &[PathBuf]) -> HashMap<PathBuf, Stamp> {
    let mut out = HashMap::new();
    for p in singles {
        let mut acc = (SystemTime::UNIX_EPOCH, 0);
        stamp(p, 5, &mut acc);
        out.insert(p.clone(), acc);
    }
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && !e.file_name().to_string_lossy().starts_with('.') {
                let mut acc = (SystemTime::UNIX_EPOCH, 0);
                stamp(&p, 5, &mut acc);
                out.insert(p, acc);
            }
        }
    }
    out
}

/// What to ask the runtime for, given two snapshots: nothing, one extension, or everything
/// (folders that appeared or vanished).
pub fn changes(old: &HashMap<PathBuf, Stamp>, new: &HashMap<PathBuf, Stamp>) -> Vec<RuntimeRequest> {
    if old.keys().any(|k| !new.contains_key(k)) || new.keys().any(|k| !old.contains_key(k)) {
        return vec![RuntimeRequest::Reload];
    }
    let mut v: Vec<_> = new
        .iter()
        .filter(|(k, s)| old.get(*k) != Some(*s))
        .filter_map(|(k, _)| k.file_name().map(|n| RuntimeRequest::ReloadExtension { name: n.to_string_lossy().into_owned() }))
        .collect();
    v.sort_by_key(|r| format!("{r:?}"));
    v
}

/// Hot reload by polling mtimes (simple, no extra dependencies).
pub fn spawn(roots: Vec<PathBuf>, runtime: RuntimeHandle) -> Singles {
    let singles: Singles = Arc::default();
    let shared = singles.clone();
    thread::Builder::new()
        .name("flux-watch".into())
        .spawn(move || {
            let current = || shared.lock().map(|s| s.clone()).unwrap_or_default();
            let mut last = snapshot(&roots, &current());
            loop {
                thread::sleep(Duration::from_millis(400));
                let now = snapshot(&roots, &current());
                let requests = changes(&last, &now);
                last = now;
                for r in requests {
                    log::info!("extensions changed, reloading ({r:?})");
                    runtime.send(r);
                }
            }
        })
        .ok();
    singles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_reload_one_extension_and_new_or_removed_folders_reload_all() {
        let d = tempfile::tempdir().unwrap();
        for n in ["a", "b"] {
            std::fs::create_dir_all(d.path().join(n)).unwrap();
            std::fs::write(d.path().join(n).join("manifest.lua"), "return {}").unwrap();
        }
        let roots = vec![d.path().to_path_buf()];
        let s1 = snapshot(&roots, &[]);
        assert!(changes(&s1, &snapshot(&roots, &[])).is_empty());

        std::fs::write(d.path().join("a/commands.lua"), "x").unwrap();
        let s2 = snapshot(&roots, &[]);
        let c = changes(&s1, &s2);
        assert!(matches!(&c[..], [RuntimeRequest::ReloadExtension { name }] if name == "a"), "{c:?}");

        std::fs::create_dir_all(d.path().join("c")).unwrap();
        assert!(matches!(&changes(&s2, &snapshot(&roots, &[]))[..], [RuntimeRequest::Reload]));
        std::fs::remove_dir_all(d.path().join("c")).unwrap();
        assert!(matches!(&changes(&snapshot(&roots, &[]), &s2)[..], [RuntimeRequest::Reload] | []));
    }
}
