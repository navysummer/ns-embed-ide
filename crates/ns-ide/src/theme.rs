//! Warm ink & gold design tokens from the project brief.
//!
//! Two palettes are provided: the default warm dark theme (墨金) and a light
//! theme. A process-wide mode flag is consulted by [`color`] and [`alpha`], so
//! every call site that resolves a token through those helpers automatically
//! follows the active theme. Set it with [`set_light`].

use gpui::{Background, Hsla, linear_color_stop, linear_gradient, rgb};
use std::sync::atomic::{AtomicBool, Ordering};

/// Dark-theme base tokens (the default 墨金 look).
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

/// Light-theme tokens: warm paper background, dark warm-ink text, deepened
/// gold/jade accents that keep contrast on a light surface.
pub const L_BG_0: u32 = 0xf3ecdf;
pub const L_BG_1: u32 = 0xfaf4e9;
pub const L_BG_2: u32 = 0xffffff;
pub const L_PANEL_SOLID: u32 = 0xffffff;
pub const L_TEXT: u32 = 0x2b2118;
pub const L_TEXT_DIM: u32 = 0x6b5f4d;
pub const L_TEXT_FAINT: u32 = 0x9a8d75;
pub const L_ACCENT: u32 = 0x8f6b2a;
pub const L_ACCENT_2: u32 = 0x6e5120;
pub const L_ACCENT_3: u32 = 0xa9833c;
pub const L_JADE: u32 = 0x3d6b5c;
pub const L_CINNABAR: u32 = 0xa04a3e;
pub const L_DANGER: u32 = 0xb34d40;
pub const L_STROKE: u32 = 0x9a8a68;
pub const L_DARK_TEXT: u32 = 0x221407;

pub const UI_FONT: &str = "PingFang SC";
pub const SERIF_FONT: &str = "Songti SC";
pub const MONO_FONT: &str = "Menlo";

/// Process-wide light-theme flag. Defaults to dark.
static LIGHT: AtomicBool = AtomicBool::new(false);

/// Enable (`true`) or disable (`false`) the light theme.
pub fn set_light(on: bool) {
    LIGHT.store(on, Ordering::Relaxed);
}

/// Whether the light theme is active.
pub fn is_light() -> bool {
    LIGHT.load(Ordering::Relaxed)
}

/// Resolve a dark-token hex to the active theme's hex.
pub fn resolve(hex: u32) -> u32 {
    if is_light() {
        match hex {
            BG_0 => L_BG_0,
            BG_1 => L_BG_1,
            BG_2 => L_BG_2,
            PANEL_SOLID => L_PANEL_SOLID,
            TEXT => L_TEXT,
            TEXT_DIM => L_TEXT_DIM,
            TEXT_FAINT => L_TEXT_FAINT,
            ACCENT => L_ACCENT,
            ACCENT_2 => L_ACCENT_2,
            ACCENT_3 => L_ACCENT_3,
            JADE => L_JADE,
            CINNABAR => L_CINNABAR,
            DANGER => L_DANGER,
            STROKE => L_STROKE,
            DARK_TEXT => L_DARK_TEXT,
            other => other,
        }
    } else {
        hex
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_is_default_and_identity() {
        set_light(false);
        assert!(!is_light());
        assert_eq!(resolve(TEXT), TEXT);
        assert_eq!(resolve(BG_0), BG_0);
    }

    #[test]
    fn light_remaps_tokens_and_keeps_unknown() {
        set_light(true);
        assert!(is_light());
        assert_eq!(resolve(TEXT), L_TEXT);
        assert_eq!(resolve(BG_0), L_BG_0);
        assert_eq!(resolve(ACCENT), L_ACCENT);
        assert_eq!(resolve(JADE), L_JADE);
        // A literal hex that is not a token stays as-is.
        assert_eq!(resolve(0x123456), 0x123456);
        set_light(false);
    }
}

pub fn color(hex: u32) -> Hsla {
    rgb(resolve(hex)).into()
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
