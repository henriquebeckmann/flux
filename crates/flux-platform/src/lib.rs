//! OS integration for the launcher daemon. Depends only on `flux-api`:
//!
//! * [`ipc`]: single-instance lock + control socket (`flux toggle|show|hide|quit`).
//! * [`desktop`]: the desktop entry portals need to identify the app.
//! * [`hotkey`]: global shortcut through the XDG GlobalShortcuts portal (Hyprland, KDE, GNOME…).
//! * [`keyring`]: [`flux_api::SecretStore`] backed by the Secret Service (never plaintext).

pub mod desktop;
pub mod files;
pub mod hotkey;
pub mod images;
pub mod ipc;
pub mod keyring;

pub use ipc::DaemonCommand;
pub use keyring::KeyringStore;
