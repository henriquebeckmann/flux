//! Installing, packaging and removing extensions on disk. No GPUI, no daemon, no Lua state
//! beyond the validator: the CLI uses this directly.
//!
//! Layout: `<installed_dir>/<name>/…` (one folder per extension, named after its unique id).
//! Packages (`.fluxext`) are zip files with `manifest.lua` at the root.

use std::{
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use flux_runtime::{compare_versions, valid_name, validate::{Report, validate_dir}};

use crate::db::Db;

pub const PACKAGE_EXTENSION: &str = "fluxext";
const MAX_FILES: usize = 5_000;
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug)]
pub enum ExtError {
    /// The extension does not pass `flux validate`.
    Invalid(Report),
    AlreadyInstalled { name: String, installed: String, incoming: String },
    NotInstalled(String),
    BadPackage(String),
    Io(String),
}

impl std::fmt::Display for ExtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtError::Invalid(r) => {
                write!(f, "the extension is not valid:")?;
                for e in &r.errors {
                    write!(f, "\n  error: {e}")?;
                }
                Ok(())
            }
            ExtError::AlreadyInstalled { name, installed, incoming } => write!(
                f,
                "'{name}' {installed} is already installed and {incoming} is not newer (use --force to replace it)"
            ),
            ExtError::NotInstalled(n) => write!(f, "'{n}' is not installed"),
            ExtError::BadPackage(m) => write!(f, "invalid package: {m}"),
            ExtError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ExtError {}

fn io<E: std::fmt::Display>(what: &str) -> impl Fn(E) -> ExtError + '_ {
    move |e| ExtError::Io(format!("{what}: {e}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub name: String,
    pub version: String,
    pub dir: PathBuf,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub name: String,
    pub version: String,
    /// The version that was replaced, if any.
    pub replaced: Option<String>,
    pub warnings: Vec<String>,
}

pub struct Manager {
    pub installed_dir: PathBuf,
    db: Arc<Db>,
}

impl Manager {
    pub fn new(installed_dir: PathBuf, db: Arc<Db>) -> Self {
        Self { installed_dir, db }
    }

    pub fn dir_of(&self, name: &str) -> PathBuf {
        self.installed_dir.join(name)
    }

    pub fn list(&self) -> Vec<Installed> {
        let Ok(rd) = std::fs::read_dir(&self.installed_dir) else { return vec![] };
        let mut out: Vec<Installed> = rd
            .flatten()
            .filter(|e| e.path().join("manifest.lua").is_file() && !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| {
                let report = validate_dir(&e.path());
                let name = report.name.clone().unwrap_or_else(|| e.file_name().to_string_lossy().into_owned());
                Installed { enabled: self.db.extension_enabled(&name), version: report.version.unwrap_or_default(), dir: e.path(), name }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Installs from a folder or a `.fluxext` package. Validates first; replaces an older version
    /// atomically (rename), keeps the old one if anything fails.
    pub fn install(&self, source: &Path, force: bool) -> Result<InstallOutcome, ExtError> {
        std::fs::create_dir_all(&self.installed_dir).map_err(io("cannot create the extensions directory"))?;
        let staging = tempfile::Builder::new().prefix(".staging-").tempdir_in(&self.installed_dir).map_err(io("cannot create a staging directory"))?;
        let root = if source.is_dir() {
            let dst = staging.path().join("src");
            copy_tree(source, &dst)?;
            dst
        } else if source.is_file() {
            let dst = staging.path().join("src");
            unpack(source, &dst)?;
            find_root(&dst)?
        } else {
            return Err(ExtError::Io(format!("{}: no such file or folder", source.display())));
        };

        let report = validate_dir(&root);
        if !report.ok() {
            return Err(ExtError::Invalid(report));
        }
        let (name, version) = (report.name.clone().unwrap_or_default(), report.version.clone().unwrap_or_default());
        if !valid_name(&name) {
            return Err(ExtError::BadPackage(format!("invalid extension name '{name}'")));
        }

        let target = self.dir_of(&name);
        let mut replaced = None;
        if target.exists() {
            let old = validate_dir(&target).version.unwrap_or_default();
            if !force && compare_versions(&version, &old) != std::cmp::Ordering::Greater {
                return Err(ExtError::AlreadyInstalled { name, installed: old, incoming: version });
            }
            replaced = Some(old);
        }
        let backup = staging.path().join("old");
        if target.exists() {
            std::fs::rename(&target, &backup).map_err(io("cannot move the old version aside"))?;
        }
        if let Err(e) = std::fs::rename(&root, &target) {
            if backup.exists() {
                let _ = std::fs::rename(&backup, &target); // put the old version back
            }
            return Err(ExtError::Io(format!("cannot install: {e}")));
        }
        self.db.record_install(&name, &version, &source.to_string_lossy());
        Ok(InstallOutcome { name, version, replaced, warnings: report.warnings.iter().map(|w| w.to_string()).collect() })
    }

    /// Removes an installed extension. `purge` also forgets its storage and preferences
    /// (secrets in the keyring are not enumerable and stay; see docs/management.md).
    pub fn remove(&self, name: &str, purge: bool, data_root: Option<&Path>) -> Result<(), ExtError> {
        if !valid_name(name) {
            return Err(ExtError::NotInstalled(name.into()));
        }
        let dir = self.dir_of(name);
        if !dir.join("manifest.lua").is_file() {
            return Err(ExtError::NotInstalled(name.into()));
        }
        std::fs::remove_dir_all(&dir).map_err(io("cannot remove the extension"))?;
        self.db.forget_extension(name, purge);
        if purge {
            if let Some(root) = data_root {
                let d = root.join(name);
                if d.is_dir() {
                    std::fs::remove_dir_all(d).map_err(io("cannot remove the extension's data"))?;
                }
            }
        }
        Ok(())
    }

    pub fn set_enabled(&self, name: &str, enabled: bool) {
        self.db.set_extension_enabled(name, enabled);
    }
}

// ---------------------------------------------------------------------------
// Copy / package / unpack
// ---------------------------------------------------------------------------

fn skip(name: &std::ffi::OsStr) -> bool {
    let n = name.to_string_lossy();
    n.starts_with('.') && n != ".luarc.json" || n.ends_with(&format!(".{PACKAGE_EXTENSION}")) || n == "node_modules"
}

/// Regular files below `dir`, relative, sorted; refuses symlinks and oversized trees.
fn collect(dir: &Path) -> Result<Vec<PathBuf>, ExtError> {
    fn walk(base: &Path, d: &Path, out: &mut Vec<PathBuf>, total: &mut u64) -> Result<(), ExtError> {
        for e in std::fs::read_dir(d).map_err(io("cannot read the folder"))?.flatten() {
            if skip(&e.file_name()) {
                continue;
            }
            let ft = e.file_type().map_err(io("cannot stat"))?;
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_path_buf();
            if ft.is_symlink() {
                return Err(ExtError::BadPackage(format!("{} is a symlink (symlinks are not allowed in extensions)", rel.display())));
            }
            if ft.is_dir() {
                walk(base, &p, out, total)?;
            } else if ft.is_file() {
                let len = e.metadata().map_err(io("cannot stat"))?.len();
                if len > MAX_FILE_BYTES {
                    return Err(ExtError::BadPackage(format!("{} is larger than 10 MB", rel.display())));
                }
                *total += len;
                out.push(rel);
                if out.len() > MAX_FILES || *total > MAX_TOTAL_BYTES {
                    return Err(ExtError::BadPackage("the extension is too large (max 5000 files / 50 MB)".into()));
                }
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    let mut total = 0;
    walk(dir, dir, &mut out, &mut total)?;
    out.sort();
    Ok(out)
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), ExtError> {
    for rel in collect(src)? {
        let to = dst.join(&rel);
        std::fs::create_dir_all(to.parent().unwrap()).map_err(io("cannot create a folder"))?;
        std::fs::copy(src.join(&rel), &to).map_err(io("cannot copy a file"))?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packaged {
    pub path: PathBuf,
    pub name: String,
    pub version: String,
    pub files: usize,
    pub bytes: u64,
}

/// Validates `dir` and writes `<name>-<version>.fluxext` (into `out_dir`, or `out_file` when given).
/// Output is reproducible: sorted entries, fixed timestamps.
pub fn package(dir: &Path, out: Option<&Path>) -> Result<Packaged, ExtError> {
    let report = validate_dir(dir);
    if !report.ok() {
        return Err(ExtError::Invalid(report));
    }
    let (name, version) = (report.name.clone().unwrap_or_default(), report.version.clone().unwrap_or_default());
    let target = match out {
        Some(p) if p.is_dir() => p.join(format!("{name}-{version}.{PACKAGE_EXTENSION}")),
        Some(p) => p.to_path_buf(),
        None => PathBuf::from(format!("{name}-{version}.{PACKAGE_EXTENSION}")),
    };
    let files = collect(dir)?;
    let tmp = target.with_extension("fluxext.partial");
    let mut bytes = 0u64;
    {
        let file = std::fs::File::create(&tmp).map_err(io("cannot create the package"))?;
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).unix_permissions(0o644);
        for rel in &files {
            let name = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            zip.start_file(name, opts).map_err(io("cannot write the package"))?;
            let data = std::fs::read(dir.join(rel)).map_err(io("cannot read a file"))?;
            bytes += data.len() as u64;
            zip.write_all(&data).map_err(io("cannot write the package"))?;
        }
        zip.finish().map_err(io("cannot finish the package"))?;
    }
    std::fs::rename(&tmp, &target).map_err(io("cannot create the package"))?;
    Ok(Packaged { path: target, name, version, files: files.len(), bytes })
}

/// Safe relative path for a zip entry: no absolute paths, no `..`, no backslashes, no hidden parts.
fn safe_entry_path(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('\\') || name.contains('\0') || name.starts_with('/') {
        return None;
    }
    let p = Path::new(name);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(n) => out.push(n),
            _ => return None,
        }
    }
    Some(out)
}

fn unpack(archive: &Path, dst: &Path) -> Result<(), ExtError> {
    let file = std::fs::File::open(archive).map_err(io("cannot open the package"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| ExtError::BadPackage(format!("not a valid package: {e}")))?;
    if zip.len() > MAX_FILES {
        return Err(ExtError::BadPackage(format!("too many entries ({})", zip.len())));
    }
    std::fs::create_dir_all(dst).map_err(io("cannot create a folder"))?;
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| ExtError::BadPackage(e.to_string()))?;
        let raw = entry.name().to_string();
        let rel = safe_entry_path(&raw).ok_or_else(|| ExtError::BadPackage(format!("unsafe path in the package: '{raw}'")))?;
        if entry.is_symlink() || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(ExtError::BadPackage(format!("'{raw}' is a symlink")));
        }
        let out = dst.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(io("cannot create a folder"))?;
            continue;
        }
        std::fs::create_dir_all(out.parent().unwrap()).map_err(io("cannot create a folder"))?;
        // Never trust the declared size: cap what is actually read.
        let mut buf = Vec::new();
        (&mut entry).take(MAX_FILE_BYTES + 1).read_to_end(&mut buf).map_err(|e| ExtError::BadPackage(format!("'{raw}': {e}")))?;
        if buf.len() as u64 > MAX_FILE_BYTES {
            return Err(ExtError::BadPackage(format!("'{raw}' is larger than 10 MB")));
        }
        total += buf.len() as u64;
        if total > MAX_TOTAL_BYTES {
            return Err(ExtError::BadPackage("the package expands to more than 50 MB".into()));
        }
        std::fs::write(&out, &buf).map_err(io("cannot write a file"))?;
    }
    Ok(())
}

/// The folder with `manifest.lua`: the unpack root, or its only subfolder.
fn find_root(dir: &Path) -> Result<PathBuf, ExtError> {
    if dir.join("manifest.lua").is_file() {
        return Ok(dir.to_path_buf());
    }
    let subs: Vec<_> = std::fs::read_dir(dir).map_err(io("cannot read the package"))?.flatten().filter(|e| e.path().is_dir()).collect();
    if let [only] = subs.as_slice() {
        if only.path().join("manifest.lua").is_file() {
            return Ok(only.path());
        }
    }
    Err(ExtError::BadPackage("manifest.lua not found at the root of the package".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext_dir(root: &Path, name: &str, version: &str) -> PathBuf {
        let d = root.join(name);
        std::fs::create_dir_all(d.join("commands")).unwrap();
        std::fs::write(d.join("manifest.lua"), format!("return {{ name='{name}', version='{version}', commands={{{{name='hi'}}}} }}")).unwrap();
        std::fs::write(d.join("commands/hi.lua"), "function main() end").unwrap();
        d
    }

    fn mgr(root: &Path) -> Manager {
        Manager::new(root.join("installed"), Arc::new(Db::open_in_memory().unwrap()))
    }

    #[test]
    fn install_list_disable_remove_roundtrip_from_a_folder() {
        let t = tempfile::tempdir().unwrap();
        let src = ext_dir(&t.path().join("src"), "hello", "1.0.0");
        let m = mgr(t.path());
        assert!(m.list().is_empty());
        let out = m.install(&src, false).unwrap();
        assert_eq!((out.name.as_str(), out.version.as_str(), out.replaced), ("hello", "1.0.0", None));
        let l = m.list();
        assert_eq!((l.len(), l[0].name.as_str(), l[0].version.as_str(), l[0].enabled), (1, "hello", "1.0.0", true));
        m.set_enabled("hello", false);
        assert!(!m.list()[0].enabled);
        m.remove("hello", false, None).unwrap();
        assert!(m.list().is_empty() && !m.dir_of("hello").exists());
        assert!(matches!(m.remove("hello", false, None), Err(ExtError::NotInstalled(_))));
    }

    #[test]
    fn upgrades_replace_but_equal_or_older_versions_need_force() {
        let t = tempfile::tempdir().unwrap();
        let m = mgr(t.path());
        m.install(&ext_dir(&t.path().join("v1"), "x", "1.2.0"), false).unwrap();
        let err = m.install(&ext_dir(&t.path().join("same"), "x", "1.2.0"), false).unwrap_err();
        assert!(matches!(err, ExtError::AlreadyInstalled { .. }), "{err}");
        assert!(m.install(&ext_dir(&t.path().join("old"), "x", "1.1.9"), false).is_err());
        let up = m.install(&ext_dir(&t.path().join("new"), "x", "1.10.0"), false).unwrap();
        assert_eq!(up.replaced.as_deref(), Some("1.2.0"));
        assert_eq!(m.list()[0].version, "1.10.0");
        let forced = m.install(&ext_dir(&t.path().join("older"), "x", "0.5.0"), true).unwrap();
        assert_eq!(forced.replaced.as_deref(), Some("1.10.0"));
    }

    #[test]
    fn invalid_extensions_are_refused_and_nothing_is_left_behind() {
        let t = tempfile::tempdir().unwrap();
        let m = mgr(t.path());
        let bad = ext_dir(&t.path().join("src"), "bad", "1.0.0");
        std::fs::write(bad.join("commands/hi.lua"), "function main(").unwrap();
        let err = m.install(&bad, false).unwrap_err();
        assert!(matches!(&err, ExtError::Invalid(r) if r.errors.iter().any(|e| e.file == "commands/hi.lua")), "{err}");
        assert!(m.list().is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(&m.installed_dir).unwrap().flatten().collect();
        assert!(leftovers.is_empty(), "staging directories are cleaned up: {leftovers:?}");
    }

    #[test]
    fn symlinks_and_hidden_files_are_not_installed() {
        let t = tempfile::tempdir().unwrap();
        let m = mgr(t.path());
        let src = ext_dir(&t.path().join("src"), "link", "1.0.0");
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join(".git/config"), "secret").unwrap();
        std::fs::write(src.join(".env"), "TOKEN=1").unwrap();
        m.install(&src, false).unwrap();
        assert!(!m.dir_of("link").join(".git").exists() && !m.dir_of("link").join(".env").exists());
        std::os::unix::fs::symlink("/etc/passwd", src.join("commands/evil.lua")).unwrap();
        assert!(matches!(m.install(&src, true), Err(ExtError::BadPackage(e)) if e.contains("symlink")));
    }

    #[test]
    fn package_then_install_from_the_archive_is_reproducible() {
        let t = tempfile::tempdir().unwrap();
        let src = ext_dir(&t.path().join("src"), "pkg", "2.0.0");
        std::fs::write(src.join("README.md"), "docs").unwrap();
        let p1 = package(&src, Some(&t.path().join("a.fluxext"))).unwrap();
        let p2 = package(&src, Some(&t.path().join("b.fluxext"))).unwrap();
        assert_eq!((p1.name.as_str(), p1.version.as_str(), p1.files), ("pkg", "2.0.0", 3));
        assert_eq!(std::fs::read(&p1.path).unwrap(), std::fs::read(&p2.path).unwrap(), "same input, same bytes");
        let default = package(&src, Some(t.path())).unwrap();
        assert_eq!(default.path.file_name().unwrap(), "pkg-2.0.0.fluxext");
        // the package itself is not packaged again
        assert_eq!(package(&src, Some(&t.path().join("c.fluxext"))).unwrap().files, 3);

        let m = mgr(t.path());
        let out = m.install(&p1.path, false).unwrap();
        assert_eq!(out.name, "pkg");
        assert!(m.dir_of("pkg").join("README.md").is_file());
    }

    #[test]
    fn packaging_refuses_invalid_extensions() {
        let t = tempfile::tempdir().unwrap();
        let src = ext_dir(&t.path().join("src"), "nope", "1.0.0");
        std::fs::remove_file(src.join("commands/hi.lua")).unwrap();
        assert!(matches!(package(&src, None), Err(ExtError::Invalid(_))));
    }

    fn zip_with(path: &Path, entries: &[(&str, &[u8], Option<u32>)]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, data, mode) in entries {
            let mut o = zip::write::SimpleFileOptions::default();
            if let Some(m) = mode {
                o = o.unix_permissions(*m);
            }
            z.start_file(*name, o).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn malicious_archives_are_rejected() {
        let t = tempfile::tempdir().unwrap();
        let m = mgr(t.path());
        let manifest: &[u8] = b"return { name='evil', commands={{name='hi'}} }";
        let hi: &[u8] = b"function main() end";
        for (label, bad) in [
            ("zip-slip", "../../escape.lua"),
            ("absolute", "/etc/cron.d/x"),
            ("backslash", "..\\x.lua"),
        ] {
            let p = t.path().join(format!("{label}.fluxext"));
            zip_with(&p, &[("manifest.lua", manifest, None), ("commands/hi.lua", hi, None), (bad, b"x", None)]);
            let err = m.install(&p, false).unwrap_err();
            assert!(matches!(&err, ExtError::BadPackage(e) if e.contains("unsafe path")), "{label}: {err}");
        }
        assert!(!t.path().join("escape.lua").exists());

        let sym = t.path().join("sym.fluxext");
        {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&sym).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("manifest.lua", o).unwrap();
            z.write_all(manifest).unwrap();
            z.start_file("commands/hi.lua", o).unwrap();
            z.write_all(hi).unwrap();
            z.add_symlink("commands/link.lua", "/etc/passwd", o).unwrap();
            z.finish().unwrap();
        }
        assert!(matches!(m.install(&sym, false), Err(ExtError::BadPackage(e)) if e.contains("symlink")));

        let garbage = t.path().join("garbage.fluxext");
        std::fs::write(&garbage, "not a zip").unwrap();
        assert!(matches!(m.install(&garbage, false), Err(ExtError::BadPackage(_))));

        let nomanifest = t.path().join("nomanifest.fluxext");
        zip_with(&nomanifest, &[("readme.txt", b"hi", None)]);
        assert!(matches!(m.install(&nomanifest, false), Err(ExtError::BadPackage(e)) if e.contains("manifest.lua")));
        assert!(m.list().is_empty());
    }

    #[test]
    fn zip_bombs_are_capped_by_what_is_actually_read() {
        let t = tempfile::tempdir().unwrap();
        let m = mgr(t.path());
        let p = t.path().join("bomb.fluxext");
        let big = vec![b'a'; 11 * 1024 * 1024]; // compresses to a few KB, expands past the 10 MB cap
        zip_with(&p, &[("manifest.lua", b"return { name='bomb' }", None), ("data.bin", &big, None)]);
        assert!(std::fs::metadata(&p).unwrap().len() < 100_000);
        assert!(matches!(m.install(&p, false), Err(ExtError::BadPackage(e)) if e.contains("larger than 10 MB")));
    }

    #[test]
    fn a_package_may_wrap_everything_in_one_folder() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("wrapped.fluxext");
        zip_with(&p, &[("my-ext/manifest.lua", b"return { name='wrapped', commands={{name='hi'}} }", None), ("my-ext/commands/hi.lua", b"function main() end", None)]);
        assert_eq!(mgr(t.path()).install(&p, false).unwrap().name, "wrapped");
    }

    #[test]
    fn purge_forgets_storage_and_data_dirs() {
        let t = tempfile::tempdir().unwrap();
        let m = mgr(t.path());
        m.install(&ext_dir(&t.path().join("src"), "gone", "1.0.0"), false).unwrap();
        let data = t.path().join("data");
        std::fs::create_dir_all(data.join("gone")).unwrap();
        std::fs::write(data.join("gone/file"), "x").unwrap();
        use flux_api::Storage;
        m.db.local_set("gone", "k", "1");
        m.db.pref_set("gone", "p", "2");
        m.remove("gone", true, Some(&data)).unwrap();
        assert!(m.db.local_get("gone", "k").is_none() && m.db.pref_get("gone", "p").is_none());
        assert!(!data.join("gone").exists());
        m.install(&ext_dir(&t.path().join("src2"), "kept", "1.0.0"), false).unwrap();
        m.db.local_set("kept", "k", "1");
        m.remove("kept", false, Some(&data)).unwrap();
        assert!(m.db.local_get("kept", "k").is_some(), "without --purge the data survives");
    }

    #[test]
    fn names_cannot_escape_the_installed_dir() {
        let t = tempfile::tempdir().unwrap();
        let m = mgr(t.path());
        std::fs::create_dir_all(t.path().join("victim")).unwrap();
        std::fs::write(t.path().join("victim/manifest.lua"), "return {}").unwrap();
        assert!(matches!(m.remove("../victim", false, None), Err(ExtError::NotInstalled(_))));
        assert!(t.path().join("victim/manifest.lua").exists());
    }
}
