use super::*;
use gpui_kit::test::TestWindowExt;

#[gpui_kit::test]
fn main_scrollbars_use_the_window_edge_across_main_views(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    for width in [820., 1600.] {
        cx.simulate_resize(size(px(width), px(900.)));
        for page in [
            Page::Home,
            Page::Daily,
            Page::Agent,
            Page::Todo,
            Page::Search,
            Page::Tags,
        ] {
            cx.update(|window, cx| view.update(cx, |view, cx| view.open_page(page, window, cx)));
            cx.run_until_parked();
            assert_eq!(
                cx.debug_bounds("main-scrollbar-viewport").unwrap().right(),
                px(width)
            );
        }
    }
}

#[gpui_kit::test]
fn reading_and_editing_scroll_at_the_window_edge(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    cx.update(|cx| {
        gpui_kit::component::Theme::set_scrollbar_mode(
            gpui_kit::component::scroll::ScrollbarMode::Always,
            cx,
        )
    });
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.create_note("Long note", false).unwrap();
    model
        .workspace
        .write_document(
            "data/Long note.md",
            &"A line of document content.\n\n".repeat(160),
        )
        .unwrap();
    model
        .open_document(Some("data/Long note.md".into()))
        .unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.simulate_resize(size(px(1600.), px(900.)));
    for preview in [true, false] {
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.page = Page::Document;
                view.preview = preview;
                cx.notify();
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.render_frame(cx));
        let position = point(px(1593.), px(700.));
        cx.simulate_mouse_move(position, None, Default::default());
        cx.simulate_click(position, Default::default());
        cx.run_until_parked();
        cx.update(|_, cx| {
            let view = view.read(cx);
            let offset = if preview {
                view.document_scroll.offset()
            } else {
                view.editor.read(cx).scroll_offset()
            };
            assert!(offset.y < px(0.), "preview={preview}, offset={offset:?}");
        });
    }
}
