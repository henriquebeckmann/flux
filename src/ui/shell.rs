//! Application shell: owns the (optional) launcher window and the long-lived [`Core`].
//! The window is created on show and removed on hide; the state survives in `Core`.

use std::{path::PathBuf, time::Instant};

use flux_core::{Core, Effect};
use gpui::{App, Context, Entity, Global, Window, WindowHandle};

use super::{launcher::Launcher, window};

pub struct Shell {
    core: Entity<Core>,
    window: Option<WindowHandle<Launcher>>,
    socket: PathBuf,
}

impl Global for Shell {}

pub fn install(cx: &mut App, core: Entity<Core>, socket: PathBuf) {
    cx.set_global(Shell { core, window: None, socket });
}

pub fn core(cx: &App) -> Entity<Core> {
    cx.global::<Shell>().core.clone()
}

pub fn is_visible(cx: &App) -> bool {
    cx.global::<Shell>().window.is_some()
}

/// Shows the launcher (or focuses it if it is already open).
pub fn show(cx: &mut App) {
    if let Some(handle) = cx.global::<Shell>().window {
        if handle.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
        cx.global_mut::<Shell>().window = None; // the window disappeared behind our back
    }
    let core = core(cx);
    let was_reset = core.update(cx, |core, _| core.on_show(Instant::now()));
    log::debug!("show (state reset: {was_reset})");
    match window::open_launcher(cx, core) {
        Ok(handle) => cx.global_mut::<Shell>().window = Some(handle),
        Err(e) => log::error!("could not open the launcher window: {e:#}"),
    }
}

/// Hides the launcher from outside its window (daemon commands, runtime events).
pub fn hide(cx: &mut App) {
    let Some(handle) = cx.global_mut::<Shell>().window.take() else { return };
    core(cx).update(cx, |core, _| core.on_hide(Instant::now()));
    let _ = handle.update(cx, |_, window, _| window.remove_window());
}

/// Hides the launcher from inside one of its own event handlers.
pub fn hide_from_window(core: &Entity<Core>, window: &mut Window, cx: &mut Context<Launcher>) {
    cx.global_mut::<Shell>().window = None;
    core.update(cx, |core, _| core.on_hide(Instant::now()));
    window.remove_window();
}

pub fn toggle(cx: &mut App) {
    if is_visible(cx) { hide(cx) } else { show(cx) }
}

pub fn quit(cx: &mut App) {
    let shell = cx.global::<Shell>();
    shell.core.read(cx).shutdown();
    flux_platform::ipc::remove_socket(&shell.socket);
    cx.quit();
}

/// Applies effects produced outside a window (runtime events).
pub fn apply(effects: Vec<Effect>, cx: &mut App) {
    for e in effects {
        match e {
            Effect::Hide => hide(cx),
            Effect::Quit => quit(cx),
            other => apply_one(other, cx),
        }
    }
}

/// Effects that talk to the desktop rather than to the window.
pub fn apply_one(effect: Effect, cx: &mut App) {
    match effect {
        Effect::OpenUrl(url) => cx.open_url(&url),
        Effect::OpenPath(path) => cx.open_with_system(std::path::Path::new(&path)),
        Effect::CopyText(text) => cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)),
        Effect::Hide | Effect::Quit => {}
    }
}

/// Runs the file chooser with the launcher hidden, then stores the result in the open form and
/// shows the launcher again. Errors (no portal) are reported in the footer.
pub fn pick_files(field: String, title: String, multiple: bool, directories: bool, cx: &mut App) {
    log::info!("file picker: requested for field '{field}' (multiple: {multiple}, directories: {directories})");
    let core = core(cx);
    let existing: Vec<String> = core.read(cx).form().map(|(_, s)| s.list(&field).to_vec()).unwrap_or_default();
    // Hide from a fresh task, not from inside the launcher's own key/click handler: removing a
    // window while it is being updated fails silently and would leave it covering the dialog.
    cx.spawn(async move |cx| {
        let task = cx
            .update(|cx| {
                hide(cx);
                log::info!("file picker: launcher hidden, opening the dialog");
                cx.background_executor().spawn(async move { flux_platform::files::pick_blocking(&title, multiple, directories) })
            });
        let result = task.await;
        log::info!("file picker: dialog finished: {:?}", result.as_ref().map(|p| p.len()));
        let _ = cx.update(|cx| {
            core.update(cx, |c, cx| {
                match result {
                    Ok(paths) if !paths.is_empty() => {
                        let mut list = if multiple { existing } else { vec![] };
                        for p in paths {
                            let p = p.to_string_lossy().into_owned();
                            if !list.contains(&p) {
                                list.push(p);
                            }
                        }
                        c.form_set(&field, flux_api::FieldValue::List(list));
                    }
                    Ok(_) => {}
                    Err(e) => c.set_error(format!("{e}. Type a path in the field instead.")),
                }
                cx.notify();
            });
            show(cx);
            log::info!("file picker: launcher shown again");
        });
        // The dialog is still closing when the window appears and may keep the keyboard focus:
        // ask for it again a moment later.
        cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
        let _ = cx.update(|cx| show(cx));
    })
    .detach();
}
