//! Built-in icon set (original 24×24 line drawings, embedded in the binary) and the rendering of
//! `Icon`/`Image` values. SVGs are tinted with the text colour, images are loaded by GPUI from
//! local files or from the download cache.

use std::{borrow::Cow, collections::HashMap, path::PathBuf};

use anyhow::Result;
use flux_api::{BUILTIN_ICONS, COLOR_NAMES, Icon, IconKind, Image, ImageMask, ImageSource};
use gpui::{AnyElement, AssetSource, Global, Hsla, IntoElement, ObjectFit, Rgba, SharedString, StyledImage, div, img, prelude::*, px, rgb, svg};

use super::{images::Images, theme};

const HEAD: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">"#;

const ICONS: &[(&str, &str)] = &[
    ("app", r#"<rect x="4" y="4" width="16" height="16" rx="4"/><circle cx="12" cy="12" r="3"/>"#),
    ("archive", r#"<rect x="3" y="4" width="18" height="5" rx="1"/><path d="M5 9v10a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V9M10 13h4"/>"#),
    ("bell", r#"<path d="M6 16v-5a6 6 0 0 1 12 0v5l2 2H4z"/><path d="M10 21h4"/>"#),
    ("book", r#"<path d="M5 4h11a3 3 0 0 1 3 3v13H8a3 3 0 0 1-3-3z"/><path d="M5 17a3 3 0 0 1 3-3h11"/>"#),
    ("bookmark", r#"<path d="M7 3h10v18l-5-4-5 4z"/>"#),
    ("calculator", r#"<rect x="5" y="3" width="14" height="18" rx="2"/><path d="M8 7h8M8 12h2M12 12h2M16 12h.01M8 16h2M12 16h2M16 16h.01"/>"#),
    ("calendar", r#"<rect x="4" y="5" width="16" height="16" rx="2"/><path d="M4 10h16M8 3v4M16 3v4"/>"#),
    ("check", r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#),
    ("chevron-right", r#"<path d="M9 5l7 7-7 7"/>"#),
    ("clipboard", r#"<rect x="6" y="5" width="12" height="16" rx="2"/><path d="M9 5V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v1M9 11h6M9 15h6"/>"#),
    ("clock", r#"<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>"#),
    ("code", r#"<path d="M8 7l-5 5 5 5M16 7l5 5-5 5M14 4l-4 16"/>"#),
    ("copy", r#"<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h9"/>"#),
    ("cpu", r#"<rect x="7" y="7" width="10" height="10" rx="1"/><path d="M10 3v4M14 3v4M10 17v4M14 17v4M3 10h4M3 14h4M17 10h4M17 14h4"/>"#),
    ("download", r#"<path d="M12 4v11M7 11l5 5 5-5M5 20h14"/>"#),
    ("external-link", r#"<path d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1h5"/>"#),
    ("file", r#"<path d="M6 3h8l5 5v12a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z"/><path d="M14 3v5h5"/>"#),
    ("file-text", r#"<path d="M6 3h8l5 5v12a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z"/><path d="M14 3v5h5M8 13h8M8 17h6"/>"#),
    ("film", r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M8 4v16M16 4v16M3 9h5M3 15h5M16 9h5M16 15h5"/>"#),
    ("folder", r#"<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#),
    ("folder-open", r#"<path d="M3 18V6a1 1 0 0 1 1-1h5l2 2h7a1 1 0 0 1 1 1v2"/><path d="M3 18l2.5-8h17L20 18z"/>"#),
    ("gear", r#"<circle cx="12" cy="12" r="3"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M18.4 5.6l-2.1 2.1M7.7 16.3l-2.1 2.1"/>"#),
    ("git-branch", r#"<circle cx="6" cy="5" r="2"/><circle cx="6" cy="19" r="2"/><circle cx="18" cy="9" r="2"/><path d="M6 7v10M18 11a6 6 0 0 1-6 6H8"/>"#),
    ("globe", r#"<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/>"#),
    ("hard-drive", r#"<rect x="3" y="13" width="18" height="7" rx="2"/><path d="M5 13l2-8h10l2 8M7 16.5h.01M11 16.5h.01"/>"#),
    ("history", r#"<path d="M4 12a8 8 0 1 0 3-6.2M4 4v4h4M12 8v4l3 2"/>"#),
    ("image", r#"<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="9" cy="10" r="1.5"/><path d="M4 18l5-5 4 4 3-3 4 4"/>"#),
    ("info", r#"<circle cx="12" cy="12" r="9"/><path d="M12 11v6M12 7.5v.01"/>"#),
    ("key", r#"<circle cx="8" cy="15" r="4"/><path d="M11 12l9-9M16 7l3 3M14 9l2 2"/>"#),
    ("link", r#"<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>"#),
    ("list", r#"<path d="M9 6h11M9 12h11M9 18h11M4 6h.01M4 12h.01M4 18h.01"/>"#),
    ("lock", r#"<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V8a4 4 0 0 1 8 0v3"/>"#),
    ("music", r#"<path d="M9 18V5l11-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="17" cy="16" r="3"/>"#),
    ("package", r#"<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M4 7.5l8 4.5 8-4.5M12 12v9"/>"#),
    ("plus", r#"<path d="M12 5v14M5 12h14"/>"#),
    ("refresh", r#"<path d="M20 11a8 8 0 0 0-14-4M4 4v4h4M4 13a8 8 0 0 0 14 4M20 20v-4h-4"/>"#),
    ("search", r#"<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4"/>"#),
    ("star", r#"<path d="M12 3l2.7 5.6 6.1.9-4.4 4.3 1 6.1L12 17l-5.4 2.9 1-6.1L3.2 9.5l6.1-.9z"/>"#),
    ("tag", r#"<path d="M3 12V4a1 1 0 0 1 1-1h8l9 9-9 9z"/><circle cx="8" cy="8" r="1.5"/>"#),
    ("terminal", r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7 9l3 3-3 3M12 15h5"/>"#),
    ("trash", r#"<path d="M4 7h16M10 7V4h4v3M6 7l1 13h10l1-13M10 11v6M14 11v6"/>"#),
    ("user", r#"<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>"#),
    ("warning", r#"<path d="M12 3l10 17H2z"/><path d="M12 10v5M12 17.5v.01"/>"#),
    ("x", r#"<path d="M6 6l12 12M18 6L6 18"/>"#),
];

/// Serves the embedded icons to GPUI as `icons/<name>.svg`.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let Some(name) = path.strip_prefix("icons/").and_then(|p| p.strip_suffix(".svg")) else { return Ok(None) };
        Ok(ICONS.iter().find(|(n, _)| *n == name).map(|(_, body)| Cow::Owned(format!("{HEAD}{body}</svg>").into_bytes())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(if path.trim_end_matches('/') == "icons" { ICONS.iter().map(|(n, _)| SharedString::from(format!("icons/{n}.svg"))).collect() } else { vec![] })
    }
}

/// Palette for named colors (tags, tints).
pub fn named_color(name: &str) -> Hsla {
    let c: Rgba = match name {
        "red" => rgb(0xf0_6b_6b),
        "orange" => rgb(0xf0_9a_4a),
        "yellow" => rgb(0xe8_c9_4f),
        "green" => rgb(0x5f_c9_7a),
        "blue" => rgb(0x6b_9b_ff),
        "purple" => rgb(0xa7_7b_ff),
        "pink" => rgb(0xf0_7b_c0),
        "gray" => rgb(0x9a_9a_a8),
        hex if hex.starts_with('#') => {
            let h = &hex[1..];
            let h: String = if h.len() == 3 { h.chars().flat_map(|c| [c, c]).collect() } else { h.to_string() };
            rgb(u32::from_str_radix(&h, 16).unwrap_or(0x9a9aa8))
        }
        _ => theme::text_dim(),
    };
    c.into()
}

pub fn tint_or(tint: &Option<String>, fallback: Hsla) -> Hsla {
    tint.as_deref().map(named_color).unwrap_or(fallback)
}

/// A square icon of `size` px.
pub fn render_icon(icon: &Icon, size: f32, fallback_color: Hsla, cx: &mut gpui::App) -> AnyElement {
    let color = tint_or(&icon.tint, fallback_color);
    match &icon.kind {
        IconKind::Named(name) => svg().path(format!("icons/{name}.svg")).size(px(size)).flex_none().text_color(color).into_any_element(),
        IconKind::Text(t) => {
            let glyph: String = t.chars().take(2).collect();
            div().size(px(size)).flex_none().flex().items_center().justify_center().text_size(px(size * 0.85)).text_color(color).child(glyph).into_any_element()
        }
        IconKind::Image(image) => render_image(image, size, size, ObjectFit::Cover, fallback_color, cx),
    }
}

fn local_or_cached(source: &ImageSource, cx: &mut gpui::App) -> Option<PathBuf> {
    match source {
        ImageSource::Path(p) => Some(PathBuf::from(p)),
        ImageSource::Url(u) => Images::request(u, cx),
    }
}

/// An image box. While a remote image downloads, or when it cannot be loaded, the fallback
/// (icon name or text) is shown instead.
pub fn render_image(image: &Image, w: f32, h: f32, fit: ObjectFit, color: Hsla, cx: &mut gpui::App) -> AnyElement {
    let radius = match image.mask {
        ImageMask::None => px(0.),
        ImageMask::Circle => px(w.max(h)),
        ImageMask::RoundedRectangle => px(w.min(h) * 0.18),
    };
    let frame = div().w(px(w)).h(px(h)).flex_none().overflow_hidden().rounded(radius).flex().items_center().justify_center();
    match local_or_cached(&image.source, cx) {
        Some(path) if !Images::failed(&image.source, cx) => frame.child(img(path).w(px(w)).h(px(h)).object_fit(fit)).into_any_element(),
        _ => {
            let fb = image.fallback.as_deref().map(Icon::from_str).unwrap_or_else(|| Icon { kind: IconKind::Named("image".into()), tint: None });
            let size = (w.min(h) * 0.5).clamp(12., 40.);
            let failed = Images::failed(&image.source, cx);
            // A fallback for an image that could not be loaded carries a small warning dot, so it is not mistaken for the real thing.
            frame
                .relative()
                .bg(theme::chip())
                .child(render_icon(&fb, size, color, cx))
                .when(failed && w.min(h) >= 24., |f| f.child(div().absolute().right(px(2.)).bottom(px(2.)).size(px(6.)).rounded_full().bg(theme::destructive())))
                .into_any_element()
        }
    }
}

/// Names the UI can draw; kept in sync with `flux_api::BUILTIN_ICONS` by a test.
pub fn available() -> impl Iterator<Item = &'static str> {
    ICONS.iter().map(|(n, _)| *n)
}

#[allow(dead_code)]
pub fn colors() -> HashMap<&'static str, Hsla> {
    COLOR_NAMES.iter().map(|n| (*n, named_color(n))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_icon_has_a_drawing_and_vice_versa() {
        let mut have: Vec<&str> = available().collect();
        let mut want: Vec<&str> = BUILTIN_ICONS.to_vec();
        have.sort();
        want.sort();
        assert_eq!(have, want);
    }

    #[test]
    fn icons_are_served_as_well_formed_svg_and_unknown_ones_are_not() {
        let svg = Assets.load("icons/folder.svg").unwrap().unwrap();
        let text = String::from_utf8(svg.to_vec()).unwrap();
        assert!(text.starts_with("<svg") && text.ends_with("</svg>") && text.contains("<path"));
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
        assert!(Assets.load("../etc/passwd").unwrap().is_none());
        assert_eq!(Assets.list("icons").unwrap().len(), BUILTIN_ICONS.len());
        for (name, _) in ICONS {
            let doc = String::from_utf8(Assets.load(&format!("icons/{name}.svg")).unwrap().unwrap().to_vec()).unwrap();
            assert_eq!(doc.matches('<').count(), doc.matches('>').count(), "{name}: unbalanced tags");
        }
    }

    #[test]
    fn colors_parse() {
        assert_ne!(named_color("red"), named_color("green"));
        assert_eq!(named_color("#fff"), named_color("#ffffff"));
    }
}
