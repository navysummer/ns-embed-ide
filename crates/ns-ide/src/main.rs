mod agent;
mod app;
mod editor;
mod git;
mod highlight;
mod palette;
mod search;
mod serial;
mod text_field;
mod theme;
mod widgets;
mod terminal;
mod wizard;

use std::path::PathBuf;

use app::IdeApp;
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px,
    size,
};

fn main() {
    let initial = std::env::args().nth(1).map(PathBuf::from);

    Application::new().run(move |cx: &mut App| {
        // Load user keystroke overrides so the keymap honors `keybindings.json`.
        let overrides = ns_project::KeybindingOverrides::load();
        let mut bindings = Vec::new();
        bindings.extend(app::bindings_with(&overrides));
        bindings.extend(editor::bindings());
        bindings.extend(text_field::bindings());
        cx.bind_keys(bindings);
        let bounds = Bounds::centered(None, size(px(1380.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(980.), px(620.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("ns-embed-ide".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(17.))),
                }),
                ..Default::default()
            },
            |_window, cx| {
                let initial = initial.clone();
                cx.new(|cx| IdeApp::new(initial, cx))
            },
        )
        .expect("failed to open ns-embed-ide window");

        cx.activate(true);
    });
}
