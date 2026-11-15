//! Presentation layer (GPUI). It holds no business rules: it only renders `flux-core`
//! state and translates user input into `flux-core` calls.

mod form_ui;
mod icons;
mod images;
mod launcher;
mod markdown_view;
pub mod shell;
mod text_area;
mod text_buffer;
mod text_input;
mod theme;
mod window;


/// Registers all key bindings (text editing + launcher).
pub fn bind_keys(cx: &mut gpui::App) {
    text_input::bind_keys(cx);
    text_area::bind_keys(cx);
    launcher::bind_keys(cx);
}

pub use icons::Assets;

static REDUCE_MOTION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `reduce_motion = true` in config.toml: indeterminate loading indicators stay still.
pub fn reduce_motion() -> bool {
    REDUCE_MOTION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Directory for downloaded images and the shell setup that needs it.
pub fn install(cx: &mut gpui::App, cache_dir: std::path::PathBuf, reduce_motion: bool) {
    REDUCE_MOTION.store(reduce_motion, std::sync::atomic::Ordering::Relaxed);
    images::install(cx, cache_dir);
}
