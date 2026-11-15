//! [`SecretStore`] on top of the freedesktop Secret Service (gnome-keyring, KWallet, KeePassXC…)
//! through `oo7`. If no service is reachable every call returns a clear error: credentials are
//! never written to disk in plaintext as a fallback.

use std::{sync::Arc, time::Duration};

use flux_api::{BoxFuture, SecretError, SecretStore};
use tokio::sync::OnceCell;

const TIMEOUT: Duration = Duration::from_secs(15); // unlocking may show a prompt

/// Runs all D-Bus work on one dedicated runtime so the connection outlives any extension
/// worker (workers are restarted on reload).
pub struct KeyringStore {
    rt: Option<Arc<tokio::runtime::Runtime>>,
    keyring: Arc<OnceCell<Result<oo7::Keyring, String>>>,
}

impl KeyringStore {
    pub fn new() -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).thread_name("flux-keyring").enable_all().build()?;
        Ok(Self { rt: Some(Arc::new(rt)), keyring: Arc::new(OnceCell::new()) })
    }

    fn rt(&self) -> &Arc<tokio::runtime::Runtime> {
        self.rt.as_ref().expect("runtime lives until drop")
    }

    /// Runs `f` against the keyring on the dedicated runtime.
    fn run<T, F, Fut>(&self, f: F) -> BoxFuture<'_, Result<T, SecretError>>
    where
        T: Send + 'static,
        F: FnOnce(Arc<OnceCell<Result<oo7::Keyring, String>>>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, SecretError>> + Send + 'static,
    {
        let keyring = self.keyring.clone();
        let handle = self.rt().spawn(async move {
            tokio::time::timeout(TIMEOUT, f(keyring)).await.unwrap_or_else(|_| Err(SecretError("timed out talking to the Secret Service".into())))
        });
        Box::pin(async move { handle.await.map_err(|e| SecretError(format!("keyring task failed: {e}")))? })
    }
}

impl Drop for KeyringStore {
    fn drop(&mut self) {
        // Dropping a runtime inside async code panics; hand it to a background shutdown.
        if let Some(rt) = self.rt.take().and_then(|a| Arc::try_unwrap(a).ok()) {
            rt.shutdown_background();
        }
    }
}

fn unavailable(e: impl std::fmt::Display) -> SecretError {
    SecretError(format!("no Secret Service available (start and unlock gnome-keyring, KWallet or KeePassXC): {e}"))
}

async fn open(cell: &OnceCell<Result<oo7::Keyring, String>>) -> Result<&oo7::Keyring, SecretError> {
    let r = cell.get_or_init(|| async { oo7::Keyring::new().await.map_err(|e| e.to_string()) }).await;
    match r {
        Ok(k) => {
            // Unlocks the collection if needed; ignore "already unlocked" style failures.
            let _ = k.unlock().await;
            Ok(k)
        }
        Err(e) => Err(unavailable(e)),
    }
}

fn attrs(ext: &str, key: &str) -> Vec<(String, String)> {
    vec![("application".into(), "flux".into()), ("extension".into(), ext.into()), ("key".into(), key.into())]
}

impl SecretStore for KeyringStore {
    fn get<'a>(&'a self, ext: &'a str, key: &'a str) -> BoxFuture<'a, Result<Option<String>, SecretError>> {
        let (ext, key) = (ext.to_string(), key.to_string());
        self.run(move |cell| async move {
            let k = open(&cell).await?;
            let items = k.search_items(&attrs(&ext, &key)).await.map_err(unavailable)?;
            match items.first() {
                None => Ok(None),
                Some(item) => {
                    let secret = item.secret().await.map_err(unavailable)?;
                    String::from_utf8(secret.to_vec()).map(Some).map_err(|_| SecretError("stored secret is not valid UTF-8".into()))
                }
            }
        })
    }

    fn set<'a>(&'a self, ext: &'a str, key: &'a str, value: &'a str) -> BoxFuture<'a, Result<(), SecretError>> {
        let (ext, key, value) = (ext.to_string(), key.to_string(), value.to_string());
        self.run(move |cell| async move {
            let k = open(&cell).await?;
            let label = format!("Flux: {ext}/{key}");
            k.create_item(&label, &attrs(&ext, &key), value.as_bytes(), true).await.map_err(unavailable)?;
            Ok(())
        })
    }

    fn remove<'a>(&'a self, ext: &'a str, key: &'a str) -> BoxFuture<'a, Result<(), SecretError>> {
        let (ext, key) = (ext.to_string(), key.to_string());
        self.run(move |cell| async move {
            let k = open(&cell).await?;
            k.delete(&attrs(&ext, &key)).await.map_err(unavailable)?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a running, unlocked Secret Service: `cargo test -p flux-platform -- --ignored`.
    #[test]
    #[ignore = "requires a live Secret Service"]
    fn real_keyring_round_trip_is_isolated_per_extension() {
        let store = KeyringStore::new().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            store.set("flux-test-a", "k", "value-a").await.unwrap();
            assert_eq!(store.get("flux-test-a", "k").await.unwrap().as_deref(), Some("value-a"));
            assert_eq!(store.get("flux-test-b", "k").await.unwrap(), None);
            store.remove("flux-test-a", "k").await.unwrap();
            assert_eq!(store.get("flux-test-a", "k").await.unwrap(), None);
        });
    }
}
