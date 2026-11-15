//! File and folder selection through the XDG FileChooser portal (works on Hyprland, KDE, GNOME
//! with their portal backend). Not every setup has one: the error says so and the form field
//! keeps its typed-path input as the fallback.

use std::path::PathBuf;

use ashpd::desktop::file_chooser::SelectedFiles;

/// Opens the portal's dialog and returns the chosen local paths (empty = cancelled).
/// Blocks the calling thread; run it off the UI thread.
pub fn pick_blocking(title: &str, multiple: bool, directories: bool) -> Result<Vec<PathBuf>, String> {
    log::info!("file chooser: asking the portal");
    runtime()?.block_on(async {
        let response = SelectedFiles::open_file()
            .title(title)
            .multiple(multiple)
            .directory(directories)
            .modal(true)
            .send()
            .await
            .map_err(|e| format!("no file chooser portal available ({e})"))?
            .response();
        match response {
            Ok(files) => Ok(files.uris().iter().filter_map(|u| file_uri_to_path(&u.to_string())).collect()),
            Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => Ok(vec![]),
            Err(e) => Err(format!("the file chooser failed: {e}")),
        }
    })
}

/// One runtime for the whole process. `ashpd` caches its D-Bus connection globally, and that
/// connection's background tasks live on the runtime that created it: a throw-away runtime per
/// call would kill them and make every call after the first hang forever.
fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RT: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread().worker_threads(1).thread_name("flux-portal").enable_all().build().map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// `file:///home/a%20b/x` -> `/home/a b/x`. Anything else is not a local path.
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(file_uri_to_path("file:///home/a%20b/x.txt"), Some(PathBuf::from("/home/a b/x.txt")));
        assert_eq!(file_uri_to_path("file://localhost/tmp/y"), Some(PathBuf::from("/tmp/y")));
        assert_eq!(file_uri_to_path("file:///caf%C3%A9"), Some(PathBuf::from("/café")));
        assert_eq!(file_uri_to_path("https://x/y"), None);
        assert_eq!(file_uri_to_path("file://host/share"), None);
        assert_eq!(file_uri_to_path("file:///bad%zz"), None);
    }
}
