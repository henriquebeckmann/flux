//! `manifest.lua`: identity, commands, permissions. Validation reports **every** problem with
//! the key that caused it, so authors fix a manifest in one pass.

use std::collections::HashSet;

use flux_api::{Capability, CommandMode, Grants, LEGACY, PermissionSet, RESERVED};
use mlua::{Lua, LuaSerdeExt, Table};
use serde::{Deserialize, Serialize};

/// The extension API version this build implements (`api_version` in manifests).
pub const API_VERSION: u32 = 1;

/// Every API version this build can run. A major version stays here for at least one release
/// after its successor ships (see docs/api-versioning.md).
pub const SUPPORTED_API_VERSIONS: &[u32] = &[1];

/// Version of Flux itself, for `min_flux_version` and messages.
pub const FLUX_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Keys a manifest may contain; others produce a warning in `flux validate`.
pub const KNOWN_KEYS: &[&str] = &[
    "name", "title", "description", "version", "author", "license", "homepage", "icon", "api_version", "min_flux_version", "commands", "permissions",
    "allowed_hosts", "grants",
];

#[derive(Debug, Deserialize)]
struct RawCommand {
    name: String,
    title: Option<String>,
    description: Option<String>,
    icon: Option<String>,
    mode: Option<String>,
}

/// `api_version = 1` and `api_version = "1"` both work.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawApiVersion {
    Number(u32),
    Text(String),
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrants {
    exec: Option<Vec<String>>,
    exec_unconfined: Option<Vec<String>>,
    kill: Option<String>,
    fs_read: Option<Vec<String>>,
    fs_write: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    name: String,
    title: Option<String>,
    description: Option<String>,
    version: Option<String>,
    author: Option<String>,
    license: Option<String>,
    homepage: Option<String>,
    icon: Option<String>,
    api_version: Option<RawApiVersion>,
    min_flux_version: Option<String>,
    grants: Option<RawGrants>,
    #[serde(default)]
    commands: Vec<RawCommand>,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    allowed_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestCommand {
    pub name: String,
    pub title: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub mode: CommandMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub title: String,
    pub description: Option<String>,
    pub version: String,
    pub author: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    pub icon: Option<String>,
    pub api_version: u32,
    pub min_flux_version: Option<String>,
    pub commands: Vec<ManifestCommand>,
    /// What the manifest asks for. The effective set (after the user's overrides) is computed by the supervisor.
    pub permissions: PermissionSet,
    /// Non-fatal findings: deprecated names, a missing `api_version`, …
    pub warnings: Vec<String>,
    /// Hosts `flux.http` may reach (exact or `*.suffix`); empty = any public host.
    pub allowed_hosts: Vec<String>,
}

pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// `MAJOR.MINOR.PATCH` with optional `-prerelease` / `+build` (numbers without leading zeros).
pub fn valid_semver(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && (p.len() == 1 || !p.starts_with('0')))
        && v[core.len()..].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

/// Compares two valid versions numerically (pre-release/build ignored).
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let nums = |v: &str| -> Vec<u64> { v.split(['-', '+']).next().unwrap_or("").split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    nums(a).cmp(&nums(b))
}

fn valid_host(h: &str) -> bool {
    let body = h.strip_prefix("*.").unwrap_or(h);
    (!body.is_empty() && body.split('.').all(|l| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')))
        || h.parse::<std::net::IpAddr>().is_ok()
}

impl Manifest {
    /// Parses and validates; the error lists every problem found.
    pub fn parse(lua: &Lua, table: Table) -> Result<Self, Vec<String>> {
        Self::parse_supporting(lua, table, SUPPORTED_API_VERSIONS)
    }

    /// [`parse`](Self::parse) for a Flux that supports these API versions (lets tests and
    /// future releases exercise the rules of an API this build does not run yet).
    pub fn parse_supporting(lua: &Lua, table: Table, supported: &[u32]) -> Result<Self, Vec<String>> {
        let raw: RawManifest = lua
            .from_value(mlua::Value::Table(table))
            .map_err(|e| vec![format!("invalid manifest: {}", e.to_string().lines().next().unwrap_or(""))])?;
        let mut errors = Vec::new();

        if !valid_name(&raw.name) {
            errors.push(format!("name: '{}' is invalid (use a-z, 0-9, - and _, up to 64 characters)", raw.name));
        }
        let version = raw.version.clone().unwrap_or_else(|| "0.0.0".into());
        if !valid_semver(&version) {
            errors.push(format!("version: '{version}' is not a valid version (expected MAJOR.MINOR.PATCH, e.g. 1.0.0)"));
        }
        let mut warnings = Vec::new();
        let api_version = match &raw.api_version {
            None => {
                warnings.push(format!("api_version is not set; assuming {API_VERSION}. Declare it (api_version = \"{API_VERSION}\") so a future Flux can tell which API this extension was written for"));
                API_VERSION
            }
            Some(v) => {
                let parsed = match v {
                    RawApiVersion::Number(n) => Some(*n),
                    RawApiVersion::Text(t) => t.trim().parse::<u32>().ok(),
                };
                match parsed {
                    Some(n) if supported.contains(&n) => n,
                    Some(n) => {
                        let supported = supported.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
                        errors.push(format!("api_version: this extension targets API {n}, but Flux {FLUX_VERSION} supports API {supported}. {}", if n > API_VERSION { "Update Flux, or use an older version of the extension." } else { "Update the extension to a supported API (see docs/api-versioning.md)." }));
                        API_VERSION
                    }
                    None => {
                        errors.push("api_version: expected a whole number such as 1".to_string());
                        API_VERSION
                    }
                }
            }
        };
        if let Some(min) = &raw.min_flux_version {
            if !valid_semver(min) {
                errors.push(format!("min_flux_version: '{min}' is not a valid version"));
            } else if compare_versions(min, FLUX_VERSION) == std::cmp::Ordering::Greater {
                errors.push(format!("min_flux_version: this extension needs Flux {min} or newer, this is Flux {FLUX_VERSION}"));
            }
        }

        let mut capabilities = std::collections::BTreeSet::new();
        for p in &raw.permissions {
            if let Some(c) = Capability::parse(p) {
                capabilities.insert(c);
            } else if let Some(expanded) = Capability::legacy(p) {
                capabilities.extend(expanded.iter().copied());
                warnings.push(match expanded {
                    [] => format!("permissions: '{p}' has never done anything and can be removed"),
                    _ => format!(
                        "permissions: '{p}' is deprecated; use {}",
                        expanded.iter().map(|c| format!("'{}'", c.as_str())).collect::<Vec<_>>().join(" and ")
                    ),
                });
            } else if RESERVED.contains(&p.as_str()) {
                errors.push(format!("permissions: '{p}' is reserved for an API that does not exist yet"));
            } else {
                errors.push(format!("permissions: unknown permission '{p}'"));
            }
        }
        // Secure storage has always been available without a declaration. Declaring either
        // credentials permission opts in to the strict model (only what is listed).
        if !capabilities.contains(&Capability::CredentialsRead) && !capabilities.contains(&Capability::CredentialsWrite) {
            capabilities.insert(Capability::CredentialsRead);
            capabilities.insert(Capability::CredentialsWrite);
        }
        let _ = LEGACY;

        let mut allowed_hosts = Vec::new();
        for h in &raw.allowed_hosts {
            let h = h.trim().to_lowercase();
            if valid_host(&h) {
                allowed_hosts.push(h);
            } else {
                errors.push(format!("allowed_hosts: invalid entry '{h}' (use a bare host like api.example.com or *.example.com, without scheme, port or path)"));
            }
        }

        let raw_grants = raw.grants.unwrap_or_default();
        for (key, list) in [("exec", &raw_grants.exec), ("fs_read", &raw_grants.fs_read), ("fs_write", &raw_grants.fs_write)] {
            for entry in list.iter().flatten() {
                let bad = entry.trim().is_empty() || entry.contains('\0') || (key != "exec" && !(entry.starts_with('/') || entry.starts_with("~/") || entry == "~"));
                let bad_exec = key == "exec" && entry.contains('/') && !entry.starts_with('/');
                if bad || bad_exec {
                    errors.push(format!("grants.{key}: invalid entry '{entry}' ({})", if key == "exec" { "use a command name like xdg-open or an absolute path" } else { "use an absolute directory or ~/…" }));
                }
            }
        }
        if raw_grants.fs_read.is_some() && !capabilities.contains(&Capability::FilesystemRead) {
            warnings.push("grants.fs_read is set but 'filesystem.read' is not declared".into());
        }
        if raw_grants.fs_write.is_some() && !capabilities.contains(&Capability::FilesystemWrite) {
            warnings.push("grants.fs_write is set but 'filesystem.write' is not declared".into());
        }
        if raw_grants.exec.is_some() && !capabilities.contains(&Capability::ProcessExecute) {
            warnings.push("grants.exec is set but 'process.execute' is not declared".into());
        }
        let exec_unconfined = raw_grants.exec_unconfined.clone().unwrap_or_default();
        for c in &exec_unconfined {
            if !raw_grants.exec.as_ref().is_some_and(|l| l.contains(c)) {
                errors.push(format!("grants.exec_unconfined: '{c}' must also be listed in grants.exec"));
            }
        }
        let kill_any = match raw_grants.kill.as_deref() {
            None | Some("own") => false,
            Some("user") => true,
            Some(other) => {
                errors.push(format!("grants.kill: '{other}' is invalid (own or user)"));
                false
            }
        };
        if kill_any && !capabilities.contains(&Capability::ProcessKill) {
            warnings.push("grants.kill is set but 'process.kill' is not declared".into());
        }
        if capabilities.contains(&Capability::ProcessExecute) && raw_grants.exec.is_none() {
            if api_version >= 2 {
                errors.push("permissions: 'process.execute' needs grants.exec (list the commands the extension runs); API 2 no longer allows \"any command\"".into());
            } else {
                warnings.push("'process.execute' without grants.exec lets the extension start any command; API 2 will require grants.exec. List the commands it needs (grants = { exec = { \"xdg-open\" } })".into());
            }
        }
        let grants = Grants { hosts: allowed_hosts.clone(), fs_read: raw_grants.fs_read, fs_write: raw_grants.fs_write, exec: raw_grants.exec, exec_unconfined, kill_any };

        let mut commands = Vec::new();
        let mut seen = HashSet::new();
        for (i, c) in raw.commands.into_iter().enumerate() {
            let at = format!("commands[{}]", i + 1);
            if !valid_name(&c.name) {
                errors.push(format!("{at}.name: '{}' is invalid (use a-z, 0-9, - and _)", c.name));
                continue;
            }
            if !seen.insert(c.name.clone()) {
                errors.push(format!("{at}.name: duplicate command '{}'", c.name));
            }
            let mode = match c.mode.as_deref().unwrap_or("view") {
                "view" => CommandMode::View,
                "no-view" => CommandMode::NoView,
                other => {
                    errors.push(format!("{at}.mode: '{other}' is invalid (view or no-view)"));
                    continue;
                }
            };
            commands.push(ManifestCommand { title: c.title.unwrap_or_else(|| c.name.clone()), name: c.name, description: c.description, icon: c.icon, mode });
        }

        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Manifest {
            title: raw.title.unwrap_or_else(|| raw.name.clone()),
            name: raw.name,
            description: raw.description,
            version,
            author: raw.author,
            license: raw.license,
            homepage: raw.homepage,
            icon: raw.icon,
            api_version,
            min_flux_version: raw.min_flux_version,
            commands,
            permissions: PermissionSet { capabilities, grants },
            warnings,
            allowed_hosts,
        })
    }

    pub fn from_lua(lua: &Lua, table: Table) -> Result<Self, String> {
        Self::parse(lua, table).map_err(|e| e.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver() {
        for ok in ["0.0.0", "1.2.3", "10.20.30", "1.0.0-beta.1", "1.0.0+build.5", "1.0.0-rc.1+x"] {
            assert!(valid_semver(ok), "{ok}");
        }
        for bad in ["1", "1.2", "1.2.3.4", "01.2.3", "a.b.c", "1.2.x", "", "1..3"] {
            assert!(!valid_semver(bad), "{bad}");
        }
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("1.2.3", "1.10.0"), Less);
        assert_eq!(compare_versions("2.0.0", "1.99.99"), Greater);
        assert_eq!(compare_versions("1.0.0-beta", "1.0.0"), Equal);
    }

    fn parse(src: &str) -> Result<Manifest, Vec<String>> {
        let lua = Lua::new();
        let t: Table = lua.load(src).eval().unwrap();
        Manifest::parse(&lua, t)
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let errs = parse("return { name = 'Bad Name', version = '1', permissions = { 'teleport' }, allowed_hosts = { 'https://x.com/a' }, api_version = 99, commands = { { name = 'ok' }, { name = 'ok' }, { name = 'BAD' }, { name = 'x', mode = 'weird' } } }").unwrap_err();
        let all = errs.join("\n");
        for needle in ["name:", "version:", "unknown permission 'teleport'", "allowed_hosts:", "targets API 99", "duplicate command 'ok'", "commands[3].name", "commands[4].mode"] {
            assert!(all.contains(needle), "missing '{needle}' in:\n{all}");
        }
    }

    #[test]
    fn minimal_manifest_gets_defaults() {
        let m = parse("return { name = 'a' }").unwrap();
        assert_eq!((m.version.as_str(), m.api_version, m.title.as_str()), ("0.0.0", API_VERSION, "a"));
        assert!(m.warnings.iter().any(|w| w.contains("api_version is not set")));
    }
}

#[cfg(test)]
mod grant_tests {
    use super::*;

    fn parse2(src: &str) -> Result<Manifest, Vec<String>> {
        let lua = Lua::new();
        let t: Table = lua.load(src).eval().unwrap();
        Manifest::parse_supporting(&lua, t, &[1, 2])
    }

    #[test]
    fn api_2_requires_grants_exec_api_1_only_warns() {
        let src = |v: u32, g: &str| format!("return {{ name = 'a', api_version = {v}, permissions = {{ 'process.execute' }}, {g} }}");
        assert!(parse2(&src(2, "")).unwrap_err().join(" ").contains("needs grants.exec"));
        assert!(parse2(&src(2, "grants = { exec = { 'ps' } }")).is_ok());
        let m = parse2(&src(1, "")).unwrap();
        assert!(m.warnings.iter().any(|w| w.contains("without grants.exec")));
    }

    #[test]
    fn unconfined_commands_must_be_granted_and_kill_scope_is_validated() {
        let e = parse2("return { name = 'a', api_version = 1, permissions = { 'process.execute' }, grants = { exec = { 'ps' }, exec_unconfined = { 'notify-send' } } }").unwrap_err().join(" ");
        assert!(e.contains("must also be listed in grants.exec"), "{e}");
        let e = parse2("return { name = 'a', api_version = 1, permissions = { 'process.kill' }, grants = { kill = 'everyone' } }").unwrap_err().join(" ");
        assert!(e.contains("grants.kill"), "{e}");
        let m = parse2("return { name = 'a', api_version = 1, permissions = { 'process.kill' }, grants = { kill = 'user' } }").unwrap();
        assert!(m.permissions.grants.kill_any);
        let m = parse2("return { name = 'a', api_version = 1, permissions = { 'process.execute' }, grants = { exec = { 'ps', 'notify-send' }, exec_unconfined = { 'notify-send' } } }").unwrap();
        assert!(!m.permissions.command_confined("notify-send") && m.permissions.command_confined("ps"));
    }
}
