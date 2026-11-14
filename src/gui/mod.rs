//! Native GPUI desktop client, sharing workspace services and semantic themes.
mod agent;
mod home;
mod icons;
mod markdown;
mod model;
mod motion;
mod presentation;
#[cfg(feature = "gui-test")]
pub mod review;
mod rich_text;
mod theme;
mod view;

use crate::core::storage::Storage;
use anyhow::Result;
use gpui_kit::{prelude::*, px, size, App, Bounds, WindowBounds, WindowOptions};

fn init(cx: &mut App) {
    gpui_kit::init(cx);
    bind_shortcuts(cx);
}

fn bind_shortcuts(cx: &mut App) {
    let context = std::rc::Rc::new(
        gpui_kit::KeyBindingContextPredicate::parse("Desktop")
            .expect("the desktop key context is valid"),
    );
    let shortcuts: [(&str, Box<dyn gpui_kit::Action>); 5] = [
        ("secondary-n", Box::new(view::NewNote)),
        ("secondary-e", Box::new(view::TogglePreview)),
        ("secondary-f", Box::new(view::OpenSearch)),
        ("secondary-j", Box::new(view::OpenAgent)),
        ("secondary-b", Box::new(view::ToggleSidebar)),
    ];
    let bindings = shortcuts.map(|(keys, action)| {
        gpui_kit::KeyBinding::load(keys, action, Some(context.clone()), false, None, cx)
            .expect("desktop shortcuts resolve for the host operating system")
    });
    cx.bind_keys(bindings);
}

pub fn run() -> Result<()> {
    let desktop = model::Desktop::open(Storage::resolve_root()?)?;
    gpui_kit::application()
        .with_assets(icons::Assets)
        .run(move |cx: &mut App| {
            init(cx);
            theme::apply(desktop.theme.theme, cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1360.), px(960.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(820.), px(640.))),
                    titlebar: Some(gpui_kit::TitlebarOptions {
                        title: Some("NólëBase".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| view::DesktopView::new(desktop, window, cx)),
            )
            .expect("opening NólëBase window");
            cx.activate(true);
        });
    Ok(())
}
