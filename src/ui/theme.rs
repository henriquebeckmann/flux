use gpui::{Rgba, rgba};

pub fn background() -> Rgba { rgba(0x16161cfa) }
pub fn border() -> Rgba { rgba(0xffffff1f) }
pub fn divider() -> Rgba { rgba(0xffffff14) }
pub fn text() -> Rgba { rgba(0xf2f2f5ff) }
pub fn text_dim() -> Rgba { rgba(0x9a9aa8ff) }
/// 4.99:1 on the background (the previous #6b6b78 was 3.4:1, below the 4.5:1 AA threshold for small text).
pub fn text_faint() -> Rgba { rgba(0x85859aff) }
pub fn selected() -> Rgba { rgba(0xffffff2e) }
pub fn hover() -> Rgba { rgba(0xffffff1c) }
pub fn accent() -> Rgba { rgba(0x7c8cffff) }
pub fn error() -> Rgba { rgba(0xff7b7bff) }
pub fn selection_text() -> Rgba { rgba(0x7c8cff55) }
pub fn panel_background() -> Rgba { rgba(0x1d1d26fa) }
pub fn chip() -> Rgba { rgba(0xffffff1a) }
pub fn destructive() -> Rgba { rgba(0xff6b6bff) }
