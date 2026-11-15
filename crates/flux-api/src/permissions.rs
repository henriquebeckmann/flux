//! The permission model: what an extension may do (capabilities) and, for some of them, where
//! (grants). A [`PermissionSet`] is what an extension *declares*; the effective set is that
//! declaration narrowed by the user's [`UserOverride`] and is what every layer enforces.
//!
//! The manifest is only a request. Nothing here trusts it: enforcement happens when the
//! operation runs (see `docs/permissions.md`).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// One thing an extension can be allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Capability {
    #[serde(rename = "network")]
    Network,
    #[serde(rename = "filesystem.read")]
    FilesystemRead,
    #[serde(rename = "filesystem.write")]
    FilesystemWrite,
    #[serde(rename = "process.execute")]
    ProcessExecute,
    #[serde(rename = "process.kill")]
    ProcessKill,
    #[serde(rename = "clipboard.read")]
    ClipboardRead,
    #[serde(rename = "clipboard.write")]
    ClipboardWrite,
    #[serde(rename = "credentials.read")]
    CredentialsRead,
    #[serde(rename = "credentials.write")]
    CredentialsWrite,
    #[serde(rename = "browser.open")]
    BrowserOpen,
}

/// Names reserved for APIs that do not exist yet. Manifests that ask for them are rejected, so
/// that no extension holds a permission that silently does nothing.
pub const RESERVED: &[&str] = &["application.read", "window.control", "notification.send"];

/// Names used by manifests before capabilities were split; they still work (see [`Capability::legacy`]).
pub const LEGACY: &[&str] = &["network", "clipboard", "process", "filesystem", "browser", "notifications", "keyboard", "application", "window"];

impl Capability {
    pub const ALL: [Capability; 10] = [
        Self::Network,
        Self::FilesystemRead,
        Self::FilesystemWrite,
        Self::ProcessExecute,
        Self::ProcessKill,
        Self::ClipboardRead,
        Self::ClipboardWrite,
        Self::CredentialsRead,
        Self::CredentialsWrite,
        Self::BrowserOpen,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == s)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::FilesystemRead => "filesystem.read",
            Self::FilesystemWrite => "filesystem.write",
            Self::ProcessExecute => "process.execute",
            Self::ProcessKill => "process.kill",
            Self::ClipboardRead => "clipboard.read",
            Self::ClipboardWrite => "clipboard.write",
            Self::CredentialsRead => "credentials.read",
            Self::CredentialsWrite => "credentials.write",
            Self::BrowserOpen => "browser.open",
        }
    }

    /// What an old-style permission name expands to. `Some(&[])` means "accepted but it never
    /// enforced anything"; `None` means the name is not a legacy name.
    pub fn legacy(name: &str) -> Option<&'static [Capability]> {
        Some(match name {
            "network" => &[Self::Network],
            "clipboard" => &[Self::ClipboardRead, Self::ClipboardWrite],
            "process" => &[Self::ProcessExecute, Self::ProcessKill],
            "filesystem" => &[Self::FilesystemRead],
            "browser" => &[Self::BrowserOpen],
            "notifications" | "keyboard" | "application" | "window" => &[],
            _ => return None,
        })
    }
}

/// Restrictions that narrow a capability.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grants {
    /// Hosts `flux.http` and image loading may reach (`api.example.com` or `*.example.com`).
    /// Empty = any public host.
    pub hosts: Vec<String>,
    /// Directories readable with `filesystem.read` (`~/` allowed). `None` = anywhere that is not protected.
    pub fs_read: Option<Vec<String>>,
    /// Directories writable with `filesystem.write`, besides the extension's own data directory.
    pub fs_write: Option<Vec<String>>,
    /// Commands `process.execute` may start: a bare name (`xdg-open`) or an absolute path, matched
    /// exactly. `None` = any command (the behaviour of manifests written before grants existed).
    pub exec: Option<Vec<String>>,
    /// Commands from `exec` that run **without** the confinement every other command gets
    /// (they need the desktop session, for example `notify-send`). Each must also be in `exec`.
    pub exec_unconfined: Vec<String>,
    /// `process.kill` may signal any process of the user (`grants.kill = "user"`). By default it
    /// may only signal processes this extension started.
    pub kill_any: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSet {
    pub capabilities: BTreeSet<Capability>,
    pub grants: Grants,
}

fn host_covers(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(suffix) => host.len() > suffix.len() && host.ends_with(suffix) && host.as_bytes()[host.len() - suffix.len() - 1] == b'.',
        None => pattern == host,
    }
}

fn path_covers(dir: &str, path: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    path == dir || path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

impl PermissionSet {
    pub fn has(&self, c: Capability) -> bool {
        self.capabilities.contains(&c)
    }

    pub fn names(&self) -> Vec<String> {
        self.capabilities.iter().map(|c| c.as_str().to_string()).collect()
    }

    /// May `process.execute` start `command` (exactly as the extension wrote it)?
    pub fn command_allowed(&self, command: &str) -> bool {
        match &self.grants.exec {
            None => true,
            Some(list) => list.iter().any(|c| c == command),
        }
    }

    /// Does this command run confined (the default), or did the manifest list it as unconfined?
    pub fn command_confined(&self, command: &str) -> bool {
        !self.grants.exec_unconfined.iter().any(|c| c == command)
    }

    /// The declaration narrowed by what the user decided. An override can only take things
    /// away: unknown entries (not covered by the declaration) are ignored.
    pub fn restricted(&self, o: &UserOverride) -> PermissionSet {
        let mut out = self.clone();
        out.capabilities.retain(|c| !o.revoke.contains(c));
        if let Some(hosts) = &o.hosts {
            out.grants.hosts = if self.grants.hosts.is_empty() {
                hosts.clone()
            } else {
                hosts.iter().filter(|h| self.grants.hosts.iter().any(|d| host_covers(d, h) || d == *h)).cloned().collect()
            };
            if out.grants.hosts.is_empty() {
                out.capabilities.remove(&Capability::Network); // narrowed to nothing = no network
            }
        }
        out.grants.fs_read = narrow_paths(&self.grants.fs_read, &o.fs_read);
        out.grants.fs_write = narrow_paths(&self.grants.fs_write, &o.fs_write);
        out.grants.exec = match (&self.grants.exec, &o.exec) {
            (_, None) => self.grants.exec.clone(),
            (None, Some(list)) => Some(list.clone()),
            (Some(declared), Some(list)) => Some(list.iter().filter(|c| declared.contains(c)).cloned().collect()),
        };
        out
    }
}

fn narrow_paths(declared: &Option<Vec<String>>, wanted: &Option<Vec<String>>) -> Option<Vec<String>> {
    match (declared, wanted) {
        (_, None) => declared.clone(),
        (None, Some(list)) => Some(list.clone()),
        (Some(d), Some(list)) => Some(list.iter().filter(|p| d.iter().any(|dir| path_covers(dir, p))).cloned().collect()),
    }
}

/// What the user changed for one extension (`~/.config/flux/permissions.toml`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserOverride {
    pub revoke: BTreeSet<Capability>,
    pub hosts: Option<Vec<String>>,
    pub fs_read: Option<Vec<String>>,
    pub fs_write: Option<Vec<String>>,
    pub exec: Option<Vec<String>>,
}

impl UserOverride {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `permissions.toml`: `[extensions.<name>]` tables.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserPermissions {
    pub extensions: std::collections::BTreeMap<String, UserOverride>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(caps: &[Capability], grants: Grants) -> PermissionSet {
        PermissionSet { capabilities: caps.iter().copied().collect(), grants }
    }

    #[test]
    fn names_round_trip_and_legacy_names_expand() {
        for c in Capability::ALL {
            assert_eq!(Capability::parse(c.as_str()), Some(c));
        }
        assert_eq!(Capability::legacy("process"), Some(&[Capability::ProcessExecute, Capability::ProcessKill][..]));
        assert_eq!(Capability::legacy("window"), Some(&[][..]));
        assert_eq!(Capability::legacy("filesystem.read"), None);
        assert!(Capability::parse("window.control").is_none() && RESERVED.contains(&"window.control"));
    }

    #[test]
    fn exec_grants_match_the_exact_string() {
        let p = set(&[Capability::ProcessExecute], Grants { exec: Some(vec!["xdg-open".into(), "/usr/bin/ps".into()]), ..Default::default() });
        assert!(p.command_allowed("xdg-open") && p.command_allowed("/usr/bin/ps"));
        assert!(!p.command_allowed("./xdg-open") && !p.command_allowed("/tmp/xdg-open") && !p.command_allowed("ps"));
        assert!(PermissionSet::default().command_allowed("anything"), "no grant list = legacy, unrestricted");
    }

    #[test]
    fn overrides_only_take_permissions_away() {
        let declared = set(
            &[Capability::Network, Capability::ProcessExecute, Capability::FilesystemRead],
            Grants { hosts: vec!["api.example.com".into(), "*.cdn.example.com".into()], fs_read: Some(vec!["~/Documents".into()]), exec: Some(vec!["ps".into()]), ..Default::default() },
        );
        let o = UserOverride {
            revoke: [Capability::ProcessExecute].into(),
            hosts: Some(vec!["a.cdn.example.com".into(), "evil.com".into()]),
            fs_read: Some(vec!["~/Documents/work".into(), "~/.ssh".into()]),
            exec: Some(vec!["rm".into()]),
            ..Default::default()
        };
        let e = declared.restricted(&o);
        assert!(!e.has(Capability::ProcessExecute) && e.has(Capability::Network));
        assert_eq!(e.grants.hosts, ["a.cdn.example.com"]);
        assert_eq!(e.grants.fs_read, Some(vec!["~/Documents/work".to_string()]));
        assert_eq!(e.grants.exec, Some(vec![]), "an override cannot add commands");
    }

    #[test]
    fn narrowing_hosts_to_nothing_removes_network() {
        let declared = set(&[Capability::Network], Grants { hosts: vec!["a.com".into()], ..Default::default() });
        let e = declared.restricted(&UserOverride { hosts: Some(vec!["b.com".into()]), ..Default::default() });
        assert!(!e.has(Capability::Network));
    }

    #[test]
    fn wildcard_only_matches_subdomains() {
        assert!(host_covers("*.example.com", "a.example.com") && !host_covers("*.example.com", "example.com") && !host_covers("*.example.com", "badexample.com"));
        assert!(path_covers("~/Documents", "~/Documents/a") && !path_covers("~/Documents", "~/DocumentsX"));
    }

    #[test]
    fn user_permissions_parse_from_toml_shape() {
        let json = r#"{"extensions":{"github":{"revoke":["network"],"hosts":["api.github.com"]}}}"#;
        let u: UserPermissions = serde_json::from_str(json).unwrap();
        assert!(u.extensions["github"].revoke.contains(&Capability::Network));
    }
}
