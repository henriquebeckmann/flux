//! Static validation of an extension folder: used by `flux validate`, `flux package`,
//! `flux install` and the loader's diagnostics. Nothing in the extension is *run* except the
//! declarative files (`manifest.lua`, `preferences.lua`), which are evaluated in the sandbox.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use flux_api::{Capability, Limits};
use crate::eval::Evaluator;
use mlua::Lua;
use serde::Serialize;

use crate::{
    limits::Heartbeat,
    manifest::{KNOWN_KEYS, Manifest},
    preferences,
    sandbox::Sandbox,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Diag {
    /// Path relative to the extension folder.
    pub file: String,
    pub line: Option<u32>,
    pub message: String,
}

impl std::fmt::Display for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(l) => write!(f, "{}:{l}: {}", self.file, self.message),
            None => write!(f, "{}: {}", self.file, self.message),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub name: Option<String>,
    pub version: Option<String>,
    pub errors: Vec<Diag>,
    pub warnings: Vec<Diag>,
    pub commands: usize,
    pub files: usize,
    /// What the manifest asks for (before the user's overrides).
    pub permissions: Option<flux_api::PermissionSet>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    fn error(&mut self, file: &str, line: Option<u32>, message: impl Into<String>) {
        self.errors.push(Diag { file: file.into(), line, message: message.into() });
    }

    fn warn(&mut self, file: &str, line: Option<u32>, message: impl Into<String>) {
        self.warnings.push(Diag { file: file.into(), line, message: message.into() });
    }
}

/// Splits `"<chunk>:<line>: message"` (mlua syntax errors) into a line and a clean message.
fn parse_lua_error(e: &mlua::Error) -> (Option<u32>, String) {
    let text = e.to_string();
    let text = text.lines().next().unwrap_or("").trim_start_matches("syntax error: ").to_string();
    let mut parts = text.splitn(3, ':');
    let (_, line, rest) = (parts.next(), parts.next(), parts.next());
    match (line.and_then(|l| l.trim().parse::<u32>().ok()), rest) {
        (Some(l), Some(r)) => (Some(l), r.trim().to_string()),
        _ => (None, text),
    }
}

/// Every `.lua` file below `dir`, sorted, never following symlinks.
fn lua_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() {
                walk(&p, out);
            } else if ft.is_file() && p.extension().is_some_and(|x| x == "lua") {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(dir, &mut v);
    v.sort();
    v
}

fn rel(dir: &Path, p: &Path) -> String {
    p.strip_prefix(dir).unwrap_or(p).to_string_lossy().into_owned()
}

/// (namespace used in source, any of these capabilities lets it work)
const API_PERMISSIONS: &[(&str, &[Capability])] = &[
    ("flux.http", &[Capability::Network]),
    ("flux.clipboard", &[Capability::ClipboardRead, Capability::ClipboardWrite]),
    ("flux.process", &[Capability::ProcessExecute, Capability::ProcessKill]),
    ("flux.browser", &[Capability::BrowserOpen]),
    ("flux.filesystem", &[Capability::FilesystemRead, Capability::FilesystemWrite]),
];

pub fn validate_dir(dir: &Path) -> Report {
    validate_dir_with(dir, default_evaluator().as_ref())
}

/// Inside the real `flux` program (or when `FLUX_HOST_EXE` names a host program) declarations are
/// evaluated in a confined child process. Best effort: validation also has to work where the
/// kernel offers no confinement (the supervisor, which loads extensions, is strict).
fn default_evaluator() -> Option<crate::hostproc::HostOptions> {
    let exe = std::env::var_os("FLUX_HOST_EXE").map(PathBuf::from).or_else(|| {
        let me = std::env::current_exe().ok()?;
        (me.file_name()? == "flux").then_some(me)
    })?;
    let mut opts = crate::hostproc::HostOptions::new(exe);
    opts.sandbox = flux_api::wire::SandboxMode::BestEffort;
    Some(opts)
}

/// Like [`validate_dir`]; with `eval`, the declaration files are evaluated in a confined child
/// process of that program (`<exe> host --eval`) instead of in this one.
pub fn validate_dir_with(dir: &Path, eval: Option<&crate::hostproc::HostOptions>) -> Report {
    let mut r = Report::default();
    let manifest_path = dir.join("manifest.lua");
    if !manifest_path.is_file() {
        r.error("manifest.lua", None, "missing (every extension needs a manifest.lua)");
        return r;
    }

    let files = lua_files(dir);
    r.files = files.len();

    // 1. syntax of every Lua file (compile only; nothing runs)
    let lua = Lua::new();
    let mut sources = Vec::new();
    for f in &files {
        let name = rel(dir, f);
        match std::fs::read_to_string(f) {
            Err(e) => r.error(&name, None, format!("cannot read: {e}")),
            Ok(src) => {
                if let Err(e) = lua.load(&src).set_name(format!("@{name}")).into_function() {
                    let (line, msg) = parse_lua_error(&e);
                    r.error(&name, line, format!("syntax error: {msg}"));
                }
                sources.push((name, src));
            }
        }
    }

    // 2. manifest and 3. preferences (evaluated without `flux`; in a confined child process when `eval` says so)
    let declaration = match eval {
        Some(opts) => Evaluator::Isolated(opts.clone()).declaration(dir),
        None => match Sandbox::new(Limits::default(), Heartbeat::default()) {
            Ok(sb) => Evaluator::Local(sb).declaration(dir),
            Err(e) => Err(vec![format!("manifest.lua: could not create a Lua state: {e}")]),
        },
    };
    let manifest: Option<Manifest> = match declaration {
        Ok(d) => {
            for k in &d.unknown_keys {
                r.warn("manifest.lua", None, format!("unknown key '{k}' (ignored)"));
            }
            r.permissions = Some(d.manifest.permissions.clone());
            Some(d.manifest)
        }
        Err(errors) => {
            for e in errors {
                let (file, msg) = match e.strip_prefix("preferences.lua: ") {
                    Some(m) => ("preferences.lua", m.to_string()),
                    None => ("manifest.lua", e.trim_start_matches("manifest.lua: ").to_string()),
                };
                if !r.errors.iter().any(|d| d.file == file && d.line.is_none() && d.message == msg) {
                    r.error(file, None, msg);
                }
            }
            None
        }
    };

    // 4. structure: commands <-> files
    if let Some(m) = &manifest {
        r.name = Some(m.name.clone());
        r.version = Some(m.version.clone());
        r.commands = m.commands.len();
        if m.commands.is_empty() {
            r.warn("manifest.lua", None, "declares no commands: the extension adds nothing to the launcher");
        }
        let declared: HashSet<&str> = m.commands.iter().map(|c| c.name.as_str()).collect();
        for c in &m.commands {
            let file = format!("commands/{}.lua", c.name);
            match sources.iter().find(|(n, _)| *n == file) {
                None => r.error("manifest.lua", None, format!("command '{}' has no file {file}", c.name)),
                Some((_, src)) => {
                    if !src.contains("function main") && !src.contains("main =") && !src.contains("main=") {
                        r.warn(&file, None, "no main() function found: the command will fail when run");
                    }
                }
            }
        }
        for (name, _) in &sources {
            if let Some(stem) = name.strip_prefix("commands/").and_then(|n| n.strip_suffix(".lua")) {
                if !stem.contains('/') && !declared.contains(stem) {
                    r.warn(name, None, format!("not declared in manifest.lua (command '{stem}' will never run)"));
                }
            }
        }

        // 5. permissions: declared but unused is fine; used but undeclared is a bug
        for (name, src) in &sources {
            if src.trim_start().starts_with("---@meta") {
                continue; // type stubs for editors, not code that runs
            }
            for d in crate::deprecations::scan(src) {
                r.warn(name, None, d.message());
            }
            for (ns, perms) in API_PERMISSIONS {
                if perms.iter().any(|p| m.permissions.has(*p)) {
                    continue;
                }
                let names = perms.iter().map(|p| format!("'{}'", p.as_str())).collect::<Vec<_>>().join(" or ");
                // comments do not call anything
                let hit = src.lines().position(|l| {
                    let code = l.split("--").next().unwrap_or("");
                    code.contains(ns)
                });
                if let Some(i) = hit {
                    r.warn(name, Some(i as u32 + 1), format!("uses {ns} but the {names} permission is not declared in manifest.lua"));
                }
            }
        }
        for w in &m.warnings {
            r.warn("manifest.lua", None, w.clone());
        }
        if !m.allowed_hosts.is_empty() && !m.permissions.has(Capability::Network) {
            r.warn("manifest.lua", None, "allowed_hosts is set but the 'network' permission is not declared");
        }
        if m.permissions.has(Capability::Network) && m.allowed_hosts.is_empty() {
            r.warn("manifest.lua", None, "the 'network' permission without allowed_hosts lets the extension reach any public host; list the hosts it needs");
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (p, c) in files {
            let path = d.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, c).unwrap();
        }
        d
    }

    const GOOD: &str = "return { name = 'good', version = '1.0.0', api_version = 1, commands = { { name = 'hi', title = 'Hi' } } }";

    #[test]
    fn a_good_extension_has_no_errors_or_warnings() {
        let d = make(&[("manifest.lua", GOOD), ("commands/hi.lua", "function main() end")]);
        let r = validate_dir(d.path());
        assert!(r.ok() && r.warnings.is_empty(), "{r:?}");
        assert_eq!((r.name.as_deref(), r.commands, r.files), (Some("good"), 1, 2));
    }

    #[test]
    fn lua_syntax_errors_have_file_and_line() {
        let d = make(&[("manifest.lua", GOOD), ("commands/hi.lua", "function main()\n  local x = = 1\nend"), ("lib/util.lua", "return {")]);
        let r = validate_dir(d.path());
        let hi = r.errors.iter().find(|e| e.file == "commands/hi.lua").unwrap();
        assert_eq!(hi.line, Some(2), "{hi:?}");
        assert!(hi.message.contains("syntax error"));
        assert!(r.errors.iter().any(|e| e.file == "lib/util.lua"));
    }

    #[test]
    fn structure_problems_are_reported() {
        let d = make(&[
            ("manifest.lua", "return { name = 'x', unknown_key = 1, commands = { { name = 'a' }, { name = 'b' } } }"),
            ("commands/a.lua", "-- nothing here"),
            ("commands/orphan.lua", "function main() end"),
        ]);
        let r = validate_dir(d.path());
        assert!(r.errors.iter().any(|e| e.message.contains("command 'b' has no file commands/b.lua")), "{r:?}");
        let w: Vec<String> = r.warnings.iter().map(|w| w.to_string()).collect();
        assert!(w.iter().any(|m| m.contains("unknown key 'unknown_key'")), "{w:?}");
        assert!(w.iter().any(|m| m.contains("commands/a.lua") && m.contains("no main()")), "{w:?}");
        assert!(w.iter().any(|m| m.contains("commands/orphan.lua") && m.contains("not declared")), "{w:?}");
    }

    #[test]
    fn manifest_errors_are_all_listed() {
        let d = make(&[("manifest.lua", "return { name = 'BAD', version = 'x', permissions = { 'nope' } }")]);
        let r = validate_dir(d.path());
        assert!(r.errors.len() >= 3, "{r:?}");
    }

    #[test]
    fn missing_or_broken_manifest() {
        let d = tempfile::tempdir().unwrap();
        assert!(validate_dir(d.path()).errors[0].message.contains("missing"));
        let d = make(&[("manifest.lua", "return {")]);
        let r = validate_dir(d.path());
        assert!(!r.ok() && r.errors.iter().all(|e| e.file == "manifest.lua"), "{r:?}");
        let d = make(&[("manifest.lua", "io.open('/etc/passwd') return {}")]);
        assert!(!validate_dir(d.path()).ok(), "declarations run in the sandbox");
    }

    #[test]
    fn undeclared_permissions_and_network_without_hosts_warn() {
        let d = make(&[
            ("manifest.lua", "return { name = 'p', api_version = 1, permissions = { 'network' }, commands = { { name = 'c' } } }"),
            ("commands/c.lua", "function main()\n  flux.clipboard.setText('x')\nend"),
        ]);
        let w: Vec<String> = validate_dir(d.path()).warnings.iter().map(|w| w.to_string()).collect();
        assert!(w.iter().any(|m| m.contains("commands/c.lua:2") && m.contains("'clipboard.read' or 'clipboard.write' permission")), "{w:?}");
        assert!(w.iter().any(|m| m.contains("without allowed_hosts")), "{w:?}");
    }

    #[test]
    fn comments_and_type_stubs_do_not_count_as_api_use() {
        let d = make(&[
            ("manifest.lua", "return { name = 'p', api_version = 1, commands = { { name = 'c' } } }"),
            ("commands/c.lua", "-- flux.http.get is not used here\nfunction main() end -- flux.clipboard"),
            ("flux.d.lua", "---@meta\nflux.http = {}\nflux.process = {}"),
        ]);
        let r = validate_dir(d.path());
        assert!(r.ok() && r.warnings.is_empty(), "{r:?}");
    }

    #[test]
    fn invalid_preferences_are_errors() {
        let d = make(&[("manifest.lua", GOOD), ("commands/hi.lua", "function main() end"), ("preferences.lua", "return { { name = 'a', type = 'bogus' } }")]);
        let r = validate_dir(d.path());
        assert!(r.errors.iter().any(|e| e.file == "preferences.lua"), "{r:?}");
    }
}
