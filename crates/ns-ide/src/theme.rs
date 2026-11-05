//! Warm ink & gold design tokens from the project brief.

use gpui::{Background, Hsla, linear_color_stop, linear_gradient, rgb};

pub const BG_0: u32 = 0x0f0d0a;
pub const BG_1: u32 = 0x17130d;
pub const BG_2: u32 = 0x211a11;
pub const PANEL_SOLID: u32 = 0x201a11;
pub const TEXT: u32 = 0xf0e6d2;
pub const TEXT_DIM: u32 = 0xb8a98c;
pub const TEXT_FAINT: u32 = 0x8a7c63;
pub const ACCENT: u32 = 0xc9a25f;
pub const ACCENT_2: u32 = 0xa57a3d;
pub const ACCENT_3: u32 = 0xd8b97f;
pub const JADE: u32 = 0x6f9b8a;
pub const CINNABAR: u32 = 0xb0564a;
pub const DANGER: u32 = 0xcf6b5e;
pub const STROKE: u32 = 0xdbb579;
pub const DARK_TEXT: u32 = 0x221407;

pub const UI_FONT: &str = "PingFang SC";
pub const SERIF_FONT: &str = "Songti SC";
pub const MONO_FONT: &str = "Menlo";

pub fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

pub fn alpha(hex: u32, opacity: f32) -> Hsla {
    color(hex).opacity(opacity)
}

pub fn app_background() -> Background {
    linear_gradient(
        160.,
        linear_color_stop(color(BG_1), 0.),
        linear_color_stop(color(BG_0), 1.),
    )
}

pub fn gold_gradient() -> Background {
    linear_gradient(
        135.,
        linear_color_stop(color(ACCENT_3), 0.),
        linear_color_stop(color(ACCENT_2), 1.),
    )
}
