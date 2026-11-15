//! The XDG portals identify host (non-sandboxed) apps by a desktop entry named after the app id.
//! Without `dev.flux.Launcher.desktop` the GlobalShortcuts portal refuses to register Flux.

use std::path::{Path, PathBuf};

use crate::hotkey::APP_ID;

pub fn entry_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    base.join("applications").join(format!("{APP_ID}.desktop"))
}

pub fn entry_contents(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Flux\nComment=Extensible launcher\nExec={} toggle\nTerminal=false\nNoDisplay=true\nCategories=Utility;\n",
        exe.display()
    )
}

/// Writes the desktop entry at `path` (creating directories).
pub fn install(path: &Path, exe: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, entry_contents(exe))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_valid_entry_named_after_the_app_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("applications").join(format!("{APP_ID}.desktop"));
        install(&path, Path::new("/opt/flux/bin/flux")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("[Desktop Entry]\n") && text.contains("Exec=/opt/flux/bin/flux toggle\n"));
        assert!(entry_path().ends_with(format!("applications/{APP_ID}.desktop")));
    }
}
