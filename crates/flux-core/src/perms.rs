//! The user's permission overrides (`~/.config/flux/permissions.toml`): read, change, write.
//! Shared by the Manage Extensions screen and `flux permissions`.

use std::path::{Path, PathBuf};

use flux_api::{UserOverride, UserPermissions};

pub fn file(config_dir: &Path) -> PathBuf {
    config_dir.join("permissions.toml")
}

/// Reads the file; a missing file is an empty set of overrides, a broken one is an error.
pub fn load(config_dir: &Path) -> Result<UserPermissions, String> {
    let path = file(config_dir);
    match std::fs::read_to_string(&path) {
        Ok(t) => toml::from_str(&t).map_err(|e| format!("{}: {}", path.display(), e.to_string().lines().next().unwrap_or(""))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserPermissions::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Applies `change` to the extension's override and saves; an override that ends up empty is removed.
pub fn update(config_dir: &Path, extension: &str, change: impl FnOnce(&mut UserOverride)) -> Result<UserOverride, String> {
    let mut all = load(config_dir)?;
    let entry = all.extensions.entry(extension.to_string()).or_default();
    change(entry);
    let result = entry.clone();
    if entry.is_empty() {
        all.extensions.remove(extension);
    }
    std::fs::create_dir_all(config_dir).map_err(|e| format!("cannot create {}: {e}", config_dir.display()))?;
    let text = toml::to_string_pretty(&all).map_err(|e| format!("cannot encode permissions.toml: {e}"))?;
    let path = file(config_dir);
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_api::Capability;

    #[test]
    fn revoke_restore_round_trip_and_empty_overrides_disappear() {
        let d = tempfile::tempdir().unwrap();
        update(d.path(), "a", |o| {
            o.revoke.insert(Capability::Network);
        })
        .unwrap();
        assert!(std::fs::read_to_string(file(d.path())).unwrap().contains("network"));
        update(d.path(), "a", |o| {
            o.revoke.remove(&Capability::Network);
        })
        .unwrap();
        assert!(!std::fs::read_to_string(file(d.path())).unwrap().contains("[extensions.a]"));
        std::fs::write(file(d.path()), "nonsense [").unwrap();
        assert!(load(d.path()).is_err());
    }
}
