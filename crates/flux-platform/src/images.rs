//! Downloads remote images for the launcher into a cache directory.
//!
//! Safety: only http(s); the host is resolved here, every address must be public (so a hostname
//! cannot point the launcher at the local network) and the connection is pinned to the validated
//! address; redirects are not followed; responses are capped at 5 MB and must look like an image
//! (checked by magic bytes, not by the server's word).

use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use flux_api::is_private_ip;

pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// Extension for known image formats, by magic bytes.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("jpg"),
        [b'G', b'I', b'F', b'8', ..] => Some("gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("webp"),
        _ => {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_lowercase();
            (head.contains("<svg")).then_some("svg")
        }
    }
}

/// Stable file name for a URL (FNV-1a; the cache needs uniqueness, not secrecy).
pub fn cache_name(url: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// The cached file for `url`, if it was downloaded before.
pub fn cached(url: &str, dir: &Path) -> Option<PathBuf> {
    let prefix = cache_name(url);
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy() == prefix))
}

/// Downloads `url` (or returns the cached copy). Blocks; run it on a background thread.
pub fn fetch_blocking(url: &str, dir: &Path) -> Result<PathBuf, String> {
    if let Some(p) = cached(url, dir) {
        return Ok(p);
    }
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("only http(s) images".into());
    }
    let host = parsed.host_str().ok_or("no host")?.to_string();
    let port = parsed.port_or_known_default().unwrap_or(80);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    let bytes = rt.block_on(async {
        let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => tokio::net::lookup_host((host.as_str(), port)).await.map_err(|e| format!("cannot resolve {host}: {e}"))?.collect(),
        };
        if addrs.is_empty() || addrs.iter().any(|a| is_private_ip(a.ip())) {
            return Err(format!("{host} is not a public address"));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .resolve(&host, addrs[0])
            .user_agent(concat!("Flux/", env!("CARGO_PKG_VERSION"), " (image loader)"))
            .build()
            .map_err(|e| e.to_string())?;
        let mut resp = client.get(parsed).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status().as_u16()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
            if body.len() + chunk.len() > MAX_IMAGE_BYTES {
                return Err("image larger than 5 MB".into());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    })?;
    let ext = sniff(&bytes).ok_or("the response is not an image")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.{ext}", cache_name(url)));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing_recognises_images_by_content() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some("png"));
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0xx"), Some("jpg"));
        assert_eq!(sniff(b"GIF89a.."), Some("gif"));
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("webp"));
        assert_eq!(sniff(b"<?xml version='1.0'?><svg xmlns='x'></svg>"), Some("svg"));
        assert_eq!(sniff(b"<html><body>not an image</body></html>"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn cache_names_are_stable_and_distinct() {
        assert_eq!(cache_name("https://a/x.png"), cache_name("https://a/x.png"));
        assert_ne!(cache_name("https://a/x.png"), cache_name("https://a/y.png"));
        assert_eq!(cache_name("").len(), 16);
    }

    #[test]
    fn private_hosts_and_bad_schemes_are_refused_before_any_connection() {
        let d = tempfile::tempdir().unwrap();
        for url in ["http://127.0.0.1/a.png", "http://localhost/a.png", "http://10.0.0.5/a.png", "http://[::1]/a.png", "file:///etc/passwd", "ftp://x/a.png", "not a url"] {
            let r = fetch_blocking(url, d.path());
            assert!(r.is_err(), "{url} must be refused");
        }
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
        // a local server proves nothing was contacted
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let url = format!("http://127.0.0.1:{}/a.png", l.local_addr().unwrap().port());
        assert!(fetch_blocking(&url, d.path()).is_err());
        assert!(l.accept().is_err(), "no connection may reach a loopback server");
    }

    #[test]
    fn cached_files_are_found_by_url() {
        let d = tempfile::tempdir().unwrap();
        let url = "https://example.com/pic.png";
        assert!(cached(url, d.path()).is_none());
        let p = d.path().join(format!("{}.png", cache_name(url)));
        std::fs::write(&p, b"\x89PNG....").unwrap();
        assert_eq!(cached(url, d.path()), Some(p.clone()));
        assert_eq!(fetch_blocking(url, d.path()).unwrap(), p, "a cache hit needs no network");
    }
}
