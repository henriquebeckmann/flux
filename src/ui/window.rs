use flux_core::Core;
use gpui::{
    App, AppContext, Bounds, Entity, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
    layer_shell::{KeyboardInteractivity, Layer, LayerShellOptions},
    px, size,
};

use super::launcher::Launcher;

pub const WIDTH: f32 = 680.0;
pub const HEIGHT: f32 = 440.0;

/// The launcher's height, limited to 90 % of the screen on very small outputs.
fn height(cx: &App) -> f32 {
    cx.primary_display().map(|d| f32::from(d.bounds().size.height) * 0.9).map_or(HEIGHT, |max| HEIGHT.min(max.max(240.)))
}

fn options(kind: WindowKind, cx: &App) -> WindowOptions {
    WindowOptions {
        titlebar: None,
        // For layer-shell the size applies; with no anchors the compositor centers the surface.
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(WIDTH), px(height(cx))), cx))),
        app_id: Some("dev.flux.Launcher".into()),
        window_background: WindowBackgroundAppearance::Transparent,
        focus: true,
        kind,
        is_movable: false,
        is_resizable: false,
        ..Default::default()
    }
}

/// Opens the launcher window: layer-shell (Wayland, rofi/dmenu style) or, if the
/// compositor does not support it, a centered pop-up window (X11 etc.).
pub fn open_launcher(cx: &mut App, core: Entity<Core>) -> anyhow::Result<WindowHandle<Launcher>> {
    let layer = WindowKind::LayerShell(LayerShellOptions {
        namespace: "flux".into(),
        layer: Layer::Overlay,
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        ..Default::default()
    });
    let first = cx.open_window(options(layer, cx), {
        let core = core.clone();
        move |window, cx| cx.new(|cx| Launcher::new(core, window, cx))
    });
    match first {
        Ok(handle) => Ok(handle),
        Err(e) => {
            log::warn!("layer-shell unavailable ({e}); falling back to a pop-up window");
            Ok(cx.open_window(options(WindowKind::PopUp, cx), move |window, cx| cx.new(|cx| Launcher::new(core, window, cx)))?)
        }
    }
}
