use gpui::{
    App, ClickEvent, Div, ElementId, FontWeight, Hsla, SharedString, Stateful, Window, div,
    prelude::*, px,
};

use crate::theme;

pub fn brand_mark(size: f32) -> Div {
    div()
        .size(px(size))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::gold_gradient())
        .border_1()
        .border_color(theme::alpha(0xffe6b8, 0.55))
        .text_color(theme::color(theme::DARK_TEXT))
        .font_family(theme::SERIF_FONT)
        .font_weight(FontWeight::BOLD)
        .text_size(px(size * 0.42))
        .child("墨")
}

pub fn primary_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    div()
        .id(id)
        .h(px(34.))
        .px(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.))
        .bg(theme::gold_gradient())
        .border_1()
        .border_color(theme::alpha(0xffe6b8, 0.35))
        .text_color(theme::color(theme::DARK_TEXT))
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(13.))
        .cursor_pointer()
        .hover(|style| style.opacity(0.92))
        .active(|style| style.opacity(0.85))
        .child(label)
}

pub fn ghost_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    div()
        .id(id)
        .h(px(34.))
        .px(px(13.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .rounded(px(10.))
        .bg(theme::alpha(0x261d12, 0.55))
        .border_1()
        .border_color(theme::alpha(theme::STROKE, 0.14))
        .text_color(theme::color(theme::TEXT_DIM))
        .text_size(px(12.5))
        .cursor_pointer()
        .hover(|style| {
            style
                .text_color(theme::color(theme::TEXT))
                .border_color(theme::alpha(theme::STROKE, 0.34))
                .bg(theme::alpha(theme::STROKE, 0.08))
        })
        .child(label)
}

/// A destructive confirm button (cinnabar fill, light text).
pub fn danger_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    div()
        .id(id)
        .h(px(34.))
        .px(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.))
        .bg(theme::alpha(theme::CINNABAR, 0.85))
        .border_1()
        .border_color(theme::alpha(theme::CINNABAR, 0.5))
        .text_color(theme::color(theme::TEXT))
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(13.))
        .cursor_pointer()
        .hover(|style| style.opacity(0.9))
        .active(|style| style.opacity(0.85))
        .child(label)
}

pub fn icon_button(
    id: impl Into<ElementId>,
    glyph: impl Into<SharedString>,
    size: f32,
) -> Stateful<Div> {
    let glyph: SharedString = glyph.into();
    div()
        .id(id)
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme::alpha(0x261d12, 0.55))
        .border_1()
        .border_color(theme::alpha(theme::STROKE, 0.14))
        .text_color(theme::color(theme::TEXT_DIM))
        .text_size(px(size * 0.38))
        .cursor_pointer()
        .hover(|style| {
            style
                .text_color(theme::color(theme::TEXT))
                .border_color(theme::alpha(theme::STROKE, 0.34))
                .bg(theme::alpha(theme::STROKE, 0.08))
        })
        .child(glyph)
}


/// A small toggle chip for the in-editor find bar (case-sensitive / whole-word).
pub fn find_toggle(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    div()
        .id(id)
        .px(px(7.))
        .py(px(2.))
        .rounded(px(5.))
        .text_size(px(9.5))
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .bg(if active {
            theme::alpha(theme::ACCENT, 0.22)
        } else {
            theme::alpha(theme::STROKE, 0.06)
        })
        .text_color(theme::color(if active { theme::ACCENT } else { theme::TEXT_FAINT }))
        .hover(|style| style.bg(theme::alpha(theme::ACCENT, 0.14)))
        .on_click(on_click)
        .child(label)
}
pub fn pill(label: impl Into<SharedString>, tone: Hsla) -> Div {
    let label: SharedString = label.into();
    div()
        .h(px(22.))
        .px(px(9.))
        .flex()
        .items_center()
        .rounded_full()
        .bg(tone.opacity(0.14))
        .border_1()
        .border_color(tone.opacity(0.35))
        .text_color(tone)
        .text_size(px(11.))
        .child(label)
}

pub fn section_label(label: impl Into<SharedString>) -> Div {
    let label: SharedString = label.into();
    div()
        .text_color(theme::color(theme::TEXT_FAINT))
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .child(label)
}

pub fn status_dot(color: Hsla, size: f32) -> Div {
    div()
        .size(px(size))
        .rounded_full()
        .bg(color)
        .shadow(vec![gpui::BoxShadow {
            color: color.opacity(0.55),
            offset: gpui::point(px(0.), px(0.)),
            blur_radius: px(8.),
            spread_radius: px(1.),
        }])
}