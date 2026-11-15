//! Global shortcut through the XDG `GlobalShortcuts` portal.
//!
//! Wayland compositors do not let applications grab keys. Compositors that ship a portal
//! backend with `GlobalShortcuts` (xdg-desktop-portal-hyprland, -kde, -gnome) let us register a
//! named shortcut; **the user/compositor still decides which keys trigger it** (the preferred
//! trigger is only a hint). On Hyprland, add to `hyprland.conf`:
//!
//! ```text
//! bind = ALT, space, global, dev.flux.Launcher:toggle
//! ```
//!
//! Compositors without that portal fall back to `flux toggle` bound by the user.

use std::str::FromStr;

use ashpd::{
    AppID,
    desktop::global_shortcuts::{GlobalShortcuts, NewShortcut},
    register_host_app_with_connection,
    zbus,
};
use async_channel::Sender;
use futures_util::StreamExt;

use crate::ipc::DaemonCommand;

pub const APP_ID: &str = "dev.flux.Launcher";
pub const SHORTCUT_ID: &str = "toggle";

/// Registers the shortcut on a background thread. Never fails the application: when the
/// portal is missing the reason is logged once and the CLI/socket remains the way in.
pub fn spawn(preferred_trigger: String, tx: Sender<DaemonCommand>) {
    std::thread::Builder::new()
        .name("flux-hotkey".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => return log::warn!("global shortcut disabled: {e}"),
            };
            if let Err(e) = rt.block_on(run(&preferred_trigger, tx)) {
                log::warn!(
                    "global shortcut unavailable ({e}). Bind a key to `flux toggle` in your compositor instead."
                );
            }
        })
        .expect("spawn hotkey thread");
}

async fn run(trigger: &str, tx: Sender<DaemonCommand>) -> Result<(), String> {
    let id = AppID::from_str(APP_ID).map_err(|e| e.to_string())?;
    // Portals identify non-sandboxed apps through the *connection* that registered an app id,
    // so registration and the GlobalShortcuts proxy must share one D-Bus connection.
    let conn = zbus::Connection::session().await.map_err(|e| format!("no session bus: {e}"))?;
    register_host_app_with_connection(conn.clone(), id).await.map_err(|e| {
        format!("could not register the app id: {e}. Run `flux install-desktop` once to create the desktop entry the portal needs")
    })?;
    let portal = GlobalShortcuts::with_connection(conn).await.map_err(|e| format!("no GlobalShortcuts portal: {e}"))?;
    let session = portal.create_session(Default::default()).await.map_err(|e| e.to_string())?;
    let shortcut = NewShortcut::new(SHORTCUT_ID, "Show or hide the Flux launcher").preferred_trigger(trigger);
    portal
        .bind_shortcuts(&session, &[shortcut], None, Default::default())
        .await
        .map_err(|e| e.to_string())?
        .response()
        .map_err(|e| e.to_string())?;
    log::info!("global shortcut registered ({APP_ID}:{SHORTCUT_ID}, preferred trigger {trigger})");

    let mut activated = portal.receive_activated().await.map_err(|e| e.to_string())?;
    while let Some(a) = activated.next().await {
        if a.shortcut_id() == SHORTCUT_ID && tx.send(DaemonCommand::Toggle).await.is_err() {
            break;
        }
    }
    drop(session);
    Ok(())
}
