use std::{
    io::Write,
    path::{Path, PathBuf},
};

use flux_core::{ExtError, PACKAGE_EXTENSION, log_path, package as pack, tail_file};
use flux_runtime::{valid_name, validate::{Report, validate_dir}};
use serde_json::json;

use crate::{CliError, DaemonCommand, Env, templates};

type R = Result<(), CliError>;

fn io_err(what: &str, e: impl std::fmt::Display) -> CliError {
    CliError::Failed(format!("{what}: {e}"))
}

/// Tells the running launcher about a change. Not an error when it is not running: it will read
/// the new state from disk when it starts.
fn notify(env: &mut Env, cmd: DaemonCommand) {
    match env.daemon.send(&cmd) {
        Ok(()) => {}
        Err(_) => {
            let _ = writeln!(env.err, "note: the launcher is not running; the change applies the next time it starts");
        }
    }
}

// ---------------------------------------------------------------- create

pub fn create(env: &mut Env, name: &str, template: &str, dir: Option<PathBuf>) -> R {
    if !valid_name(name) {
        return Err(format!("'{name}' is not a valid extension id (use a-z, 0-9, - and _)").into());
    }
    let Some(files) = templates::files(template) else {
        return Err(format!("unknown template '{template}' (available: {})", templates::TEMPLATES.join(", ")).into());
    };
    let parent = dir.map(|d| if d.is_absolute() { d } else { env.cwd.join(d) }).unwrap_or_else(|| env.cwd.clone());
    let target = parent.join(name);
    if target.exists() && std::fs::read_dir(&target).map(|mut d| d.next().is_some()).unwrap_or(true) {
        return Err(format!("{} already exists and is not empty", target.display()).into());
    }
    for f in files {
        let path = target.join(f.path);
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| io_err("cannot create a folder", e))?;
        std::fs::write(&path, templates::render(f.content, name)).map_err(|e| io_err("cannot write a file", e))?;
    }
    // a fresh project must pass its own validation
    let report = validate_dir(&target);
    if !report.ok() {
        return Err(format!("the generated project is invalid: {}", report.errors[0]).into());
    }
    writeln!(env.out, "Created {} ({template} template)", target.display())?;
    writeln!(env.out, "\n  cd {}\n  flux dev", target.display())?;
    Ok(())
}

// -------------------------------------------------------------- validate

fn print_report(env: &mut Env, path: &Path, r: &Report) -> std::io::Result<()> {
    let label = r.name.clone().unwrap_or_else(|| path.display().to_string());
    for e in &r.errors {
        writeln!(env.err, "error: {e}")?;
    }
    for w in &r.warnings {
        writeln!(env.err, "warning: {w}")?;
    }
    if r.ok() {
        writeln!(env.out, "{label}: valid ({} commands, {} Lua files, {} warning(s))", r.commands, r.files, r.warnings.len())?;
    } else {
        writeln!(env.out, "{label}: invalid ({} error(s), {} warning(s))", r.errors.len(), r.warnings.len())?;
    }
    Ok(())
}

pub fn validate(env: &mut Env, path: &Option<PathBuf>, json: bool) -> R {
    let dir = env.resolve(path);
    let report = validate_dir(&dir);
    if json {
        let v = serde_json::to_string_pretty(&json!({ "path": dir, "valid": report.ok(), "report": report }))
            .map_err(|e| io_err("cannot encode the report", e))?;
        writeln!(env.out, "{v}")?;
    } else {
        print_report(env, &dir, &report)?;
    }
    if report.ok() { Ok(()) } else { Err(CliError::Reported) }
}

/// For `flux dev`: the folder must be an extension; problems are shown but do not stop the loop
/// (the point of dev mode is to fix them while it runs), except a missing manifest.
pub fn dev_prepare(env: &mut Env, path: &Option<PathBuf>) -> Result<PathBuf, CliError> {
    let dir = env.resolve(path);
    let dir = std::fs::canonicalize(&dir).map_err(|e| io_err(&dir.display().to_string(), e))?;
    if !dir.join("manifest.lua").is_file() {
        return Err(format!("{} has no manifest.lua (run `flux create` or cd into an extension)", dir.display()).into());
    }
    let report = validate_dir(&dir);
    print_report(env, &dir, &report)?;
    Ok(dir)
}

// --------------------------------------------------------------- package

pub fn package(env: &mut Env, path: &Option<PathBuf>, out: Option<PathBuf>) -> R {
    let dir = env.resolve(path);
    let out = out.map(|o| if o.is_absolute() { o } else { env.cwd.join(o) }).or_else(|| Some(env.cwd.clone()));
    match pack(&dir, out.as_deref()) {
        Ok(p) => {
            writeln!(env.out, "Packaged {} {} -> {} ({} files, {} bytes)", p.name, p.version, p.path.display(), p.files, p.bytes)?;
            Ok(())
        }
        Err(ExtError::Invalid(report)) => {
            print_report(env, &dir, &report)?;
            Err(CliError::Reported)
        }
        Err(e) => Err(e.to_string().into()),
    }
}

// --------------------------------------------------------------- install

pub fn install(env: &mut Env, source: &Path, force: bool) -> R {
    let src = if source.is_absolute() { source.to_path_buf() } else { env.cwd.join(source) };
    let manager = env.manager()?;
    match manager.install(&src, force) {
        Ok(o) => {
            match &o.replaced {
                Some(old) => writeln!(env.out, "Updated {} {old} -> {}", o.name, o.version)?,
                None => writeln!(env.out, "Installed {} {}", o.name, o.version)?,
            }
            for w in &o.warnings {
                writeln!(env.err, "warning: {w}")?;
            }
            notify(env, DaemonCommand::ReloadExtension(o.name.clone()));
            Ok(())
        }
        Err(ExtError::Invalid(report)) => {
            print_report(env, &src, &report)?;
            Err(CliError::Reported)
        }
        Err(e) => Err(e.to_string().into()),
    }
}

pub fn remove(env: &mut Env, name: &str, purge: bool) -> R {
    let manager = env.manager()?;
    manager.remove(name, purge, Some(&env.data_dir.join("extensions"))).map_err(|e| CliError::Failed(e.to_string()))?;
    writeln!(env.out, "Removed {name}{}", if purge { " and its data" } else { "" })?;
    notify(env, DaemonCommand::Reload);
    Ok(())
}

pub fn set_enabled(env: &mut Env, name: &str, enabled: bool) -> R {
    if !valid_name(name) {
        return Err(format!("'{name}' is not a valid extension id").into());
    }
    env.db()?.set_extension_enabled(name, enabled);
    writeln!(env.out, "{} {name}", if enabled { "Enabled" } else { "Disabled" })?;
    notify(env, if enabled { DaemonCommand::Enable(name.into()) } else { DaemonCommand::Disable(name.into()) });
    Ok(())
}

pub fn reload(env: &mut Env, name: Option<String>) -> R {
    let cmd = match name {
        Some(n) => DaemonCommand::ReloadExtension(n),
        None => DaemonCommand::Reload,
    };
    env.daemon.send(&cmd).map_err(|_| CliError::Failed("the launcher is not running (start it with `flux daemon`)".into()))?;
    writeln!(env.out, "Reload requested")?;
    Ok(())
}

// ---------------------------------------------------------------- status

/// `flux status`: what each extension is using right now. Asks the running launcher.
pub fn status(env: &mut Env, json: bool) -> R {
    let line = env.daemon.query("status").map_err(|e| CliError::Failed(format!("could not get the status ({e}); is the launcher running? start it with `flux daemon`")))?;
    if json {
        writeln!(env.out, "{line}")?;
        return Ok(());
    }
    let rows: Vec<flux_api::ExtensionStats> = serde_json::from_str(&line).map_err(|e| io_err("unexpected answer from the launcher", e))?;
    if rows.is_empty() {
        writeln!(env.out, "No extensions loaded.")?;
        return Ok(());
    }
    let w = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    writeln!(env.out, "{:<w$}  {:<10}  {:>7}  {:>8}  {:>7}  {:>5}  {:>4}  {:>5}  {:>6}  ISOLATION", "NAME", "STATE", "PID", "RSS", "CPU", "TASKS", "HTTP", "CHILD", "CRASH")?;
    for r in &rows {
        let pid = r.pid.map_or("-".to_string(), |p| p.to_string());
        let rss = r.rss_kb.map_or("-".to_string(), |k| format!("{:.1} MB", k as f64 / 1024.0));
        let cpu = r.cpu_ms.map_or("-".to_string(), |m| format!("{:.1}s", m as f64 / 1000.0));
        writeln!(env.out, "{:<w$}  {:<10}  {:>7}  {:>8}  {:>7}  {:>5}  {:>4}  {:>5}  {:>6}  {}", r.name, r.state, pid, rss, cpu, r.tasks, r.http_in_flight, r.children, r.recent_crashes, r.isolation)?;
    }
    Ok(())
}

// ----------------------------------------------------------- permissions

fn find_extension(env: &Env, name: &str) -> Option<PathBuf> {
    let mut roots = vec![env.installed_dir.clone()];
    roots.extend(env.extension_dirs.iter().cloned());
    for root in roots {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for dir in rd.flatten().map(|e| e.path()).filter(|p| p.join("manifest.lua").is_file()) {
            if validate_dir(&dir).name.as_deref() == Some(name) {
                return Some(dir);
            }
        }
    }
    None
}

/// `flux permissions <name> [--revoke cap]… [--restore cap]…`: shows what the extension asked for
/// and what it effectively has, and edits the user's overrides (`permissions.toml`).
pub fn permissions(env: &mut Env, name: &str, revoke: &[String], restore: &[String], json: bool) -> R {
    use flux_api::{Capability, UserPermissions};
    if !valid_name(name) {
        return Err(format!("'{name}' is not a valid extension id").into());
    }
    let dir = find_extension(env, name).ok_or_else(|| CliError::Failed(format!("no extension named '{name}' (see `flux list`)")))?;
    let declared = validate_dir(&dir).permissions.ok_or_else(|| CliError::Failed(format!("{name}: the manifest is invalid (run `flux validate {}`)", dir.display())))?;

    let parse = |s: &String| Capability::parse(s).ok_or_else(|| CliError::Failed(format!("'{s}' is not a permission (one of: {})", Capability::ALL.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", "))));
    let changed = !revoke.is_empty() || !restore.is_empty();
    let mut user: UserPermissions = flux_core::perms::load(&env.config_dir).map_err(CliError::Failed)?;
    if changed {
        let mut to_revoke = Vec::new();
        for c in revoke {
            let cap = parse(c)?;
            if !declared.has(cap) {
                return Err(format!("{name} does not declare '{}', so there is nothing to revoke", cap.as_str()).into());
            }
            to_revoke.push(cap);
        }
        let mut to_restore = Vec::new();
        for c in restore {
            to_restore.push(parse(c)?);
        }
        let updated = flux_core::perms::update(&env.config_dir, name, |o| {
            o.revoke.extend(to_revoke.iter().copied());
            for c in &to_restore {
                o.revoke.remove(c);
            }
        })
        .map_err(CliError::Failed)?;
        user.extensions.remove(name);
        if !updated.is_empty() {
            user.extensions.insert(name.to_string(), updated);
        }
    }

    let over = user.extensions.get(name).cloned().unwrap_or_default();
    let effective = declared.restricted(&over);
    if json {
        let v = json!({ "name": name, "declared": declared, "effective": effective, "revoked": over.revoke });
        writeln!(env.out, "{}", serde_json::to_string_pretty(&v).map_err(|e| io_err("cannot encode", e))?)?;
    } else {
        writeln!(env.out, "{name}")?;
        for c in Capability::ALL {
            if declared.has(c) {
                writeln!(env.out, "  {:<18} {}", c.as_str(), if effective.has(c) { "allowed" } else { "revoked" })?;
            }
        }
        let g = &effective.grants;
        if !g.hosts.is_empty() {
            writeln!(env.out, "  hosts:     {}", g.hosts.join(", "))?;
        }
        for (label, list) in [("fs_read", &g.fs_read), ("fs_write", &g.fs_write), ("exec", &g.exec)] {
            if let Some(l) = list {
                writeln!(env.out, "  {:<10} {}", format!("{label}:"), if l.is_empty() { "(nothing)".to_string() } else { l.join(", ") })?;
            }
        }
    }
    if changed {
        match env.daemon.send(&DaemonCommand::ReloadExtension(name.to_string())) {
            Ok(()) => writeln!(env.err, "the launcher reloaded {name}")?,
            Err(_) => writeln!(env.err, "note: the launcher is not running; the change applies the next time it starts")?,
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ list

struct Row {
    name: String,
    version: String,
    source: &'static str,
    enabled: bool,
    status: String,
    path: PathBuf,
    errors: usize,
    warnings: usize,
}

fn scan(root: &Path, source: &'static str, db: &flux_core::Db, rows: &mut Vec<Row>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut dirs: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.join("manifest.lua").is_file()).collect();
    dirs.sort();
    for dir in dirs {
        let r = validate_dir(&dir);
        let name = r.name.clone().unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        let enabled = db.extension_enabled(&name);
        let status = if !r.ok() { "invalid" } else if !enabled { "disabled" } else { "ok" }.to_string();
        rows.push(Row { name, version: r.version.clone().unwrap_or_default(), source, enabled, status, path: dir, errors: r.errors.len(), warnings: r.warnings.len() });
    }
}

pub fn list(env: &mut Env, json: bool) -> R {
    let db = env.db()?;
    let mut rows = Vec::new();
    scan(&env.installed_dir.clone(), "installed", &db, &mut rows);
    for d in env.extension_dirs.clone() {
        scan(&d, "dev", &db, &mut rows);
    }
    if json {
        let v: Vec<_> = rows
            .iter()
            .map(|r| json!({ "name": r.name, "version": r.version, "source": r.source, "enabled": r.enabled, "status": r.status, "path": r.path, "errors": r.errors, "warnings": r.warnings }))
            .collect();
        writeln!(env.out, "{}", serde_json::to_string_pretty(&v).map_err(|e| io_err("cannot encode", e))?)?;
        return Ok(());
    }
    if rows.is_empty() {
        writeln!(env.out, "No extensions found. Create one with `flux create my-extension`.")?;
        return Ok(());
    }
    let w = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    writeln!(env.out, "{:<w$}  {:<9}  {:<9}  {:<8}  PATH", "NAME", "VERSION", "STATUS", "SOURCE")?;
    for r in &rows {
        writeln!(env.out, "{:<w$}  {:<9}  {:<9}  {:<8}  {}", r.name, r.version, r.status, r.source, r.path.display())?;
    }
    Ok(())
}

// ------------------------------------------------------------------ logs

pub fn logs(env: &mut Env, name: &str, follow: bool, lines: usize) -> R {
    if !valid_name(name) {
        return Err(format!("'{name}' is not a valid extension id").into());
    }
    let path = log_path(&env.log_dir, name);
    let tail = tail_file(&env.log_dir, name, lines);
    if tail.is_empty() && !follow {
        writeln!(env.out, "No log lines for {name} yet ({})", path.display())?;
        return Ok(());
    }
    for l in &tail {
        writeln!(env.out, "{l}")?;
    }
    if follow {
        crate::dev::follow_file(&path, &mut env.out, &crate::dev::interrupted_flag());
    }
    Ok(())
}
