//! The real `flux` binary, driven as a user would, against throwaway XDG directories.

use std::{path::Path, process::Command};

fn flux(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(args)
        .current_dir(dir)
        .env("XDG_DATA_HOME", dir.join("xdg/data"))
        .env("XDG_STATE_HOME", dir.join("xdg/state"))
        .env("XDG_CONFIG_HOME", dir.join("xdg/config"))
        .env("XDG_RUNTIME_DIR", dir.join("xdg/run"))
        .env_remove("FLUX_EXTENSIONS_DIR")
        .output()
        .expect("run flux");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

#[test]
fn author_workflow_create_validate_package_install_list_remove() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("xdg/run")).unwrap();
    let p = d.path();

    let (code, out, err) = flux(p, &["create", "demo-ext", "--template", "grid"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Created"));

    let (code, out, err) = flux(p, &["validate", "demo-ext"]);
    assert_eq!((code, err.as_str()), (0, ""), "a fresh project has no warnings");
    assert!(out.contains("valid"));

    // break it: the error names file and line
    std::fs::write(p.join("demo-ext/commands/main.lua"), "function main()\n  x = = 2\nend\n").unwrap();
    let (code, _, err) = flux(p, &["validate", "demo-ext"]);
    assert_eq!(code, 1);
    assert!(err.contains("commands/main.lua:2"), "{err}");
    let (code, _, _) = flux(p, &["package", "demo-ext"]);
    assert_eq!(code, 1, "invalid extensions are not packaged");
    std::fs::write(p.join("demo-ext/commands/main.lua"), "function main() return flux.ui.Grid {} end\n").unwrap();

    let (code, out, err) = flux(p, &["package", "demo-ext"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("demo-ext-0.1.0.fluxext"));
    let (code, out, err) = flux(p, &["install", "demo-ext-0.1.0.fluxext"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Installed demo-ext 0.1.0"));
    assert!(err.contains("launcher is not running"), "installing works without a daemon and says so");

    let (code, out, _) = flux(p, &["list"]);
    assert_eq!(code, 0);
    assert!(out.lines().any(|l| l.starts_with("demo-ext") && l.contains("installed")), "{out}");

    let (code, _, err) = flux(p, &["reload"]);
    assert_eq!((code, err.contains("not running")), (1, true));

    let (code, out, _) = flux(p, &["remove", "demo-ext"]);
    assert_eq!(code, 0);
    assert!(out.contains("Removed"));
    let (code, _, err) = flux(p, &["remove", "demo-ext"]);
    assert_eq!(code, 1);
    assert!(err.contains("not installed"));
}

#[test]
fn usage_errors_and_help() {
    let d = tempfile::tempdir().unwrap();
    let (code, _, err) = flux(d.path(), &["frobnicate"]);
    assert_eq!(code, 2);
    assert!(err.contains("unrecognized subcommand"));
    let (code, out, _) = flux(d.path(), &["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("create") && out.contains("validate") && out.contains("package"));
    let (code, _, _) = flux(d.path(), &["hide"]); // no daemon: reports it, does not fail
    assert_eq!(code, 0);
}
