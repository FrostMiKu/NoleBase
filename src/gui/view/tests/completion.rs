use super::*;
use gpui_kit::test::TestWindowExt;

#[gpui_kit::test]
fn wiki_completion_accepts_with_enter_and_keeps_the_message_as_a_draft(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("研究 A").unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_page(Page::Agent, window, cx)));
    cx.simulate_input("你好 [[研");
    cx.run_until_parked();
    assert!(cx.debug_bounds("completion-popup").is_some());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("completion-popup").is_none());
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert_eq!(
            view.agent_input.read(cx).value().as_str(),
            "你好 [[研究 A]]"
        );
        assert!(view.agent.as_ref().unwrap().rows.is_empty());
    });
}

#[gpui_kit::test]
fn completion_selection_leads_and_the_window_follows_at_the_edge(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    for index in 0..15 {
        model
            .workspace
            .create_note(&format!("Note {index:02}"))
            .unwrap();
    }
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_page(Page::Agent, window, cx)));
    cx.simulate_input("[[");
    cx.run_until_parked();
    let first = cx.debug_bounds("completion-option-0").unwrap();
    let label = cx.debug_bounds("completion-label-0").unwrap();
    let marker = cx.debug_bounds("completion-selection").unwrap();
    assert!(marker.right() <= label.left());
    assert_eq!(marker.size.height, first.size.height);
    for _ in 0..10 {
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
    }
    cx.update(|_, cx| {
        let state = view.read(cx).completion.as_ref().unwrap();
        assert_eq!(state.selected, 10);
        assert_eq!(state.scroll.get(), 3);
    });
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let state = view.read(cx).completion.as_ref().unwrap();
        assert_eq!(state.selected, 9);
        assert_eq!(state.scroll.get(), 3);
    });
    assert!(cx.debug_bounds("completion-option-3").is_some());
    for _ in 0..30 {
        cx.simulate_keystrokes("down");
    }
    cx.run_until_parked();
    cx.update(|_, cx| assert_eq!(view.read(cx).completion.as_ref().unwrap().selected, 14));
    for _ in 0..30 {
        cx.simulate_keystrokes("up");
    }
    cx.run_until_parked();
    cx.update(|_, cx| {
        let state = view.read(cx).completion.as_ref().unwrap();
        assert_eq!(state.selected, 0);
        assert_eq!(state.scroll.get(), 0);
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("completion-popup").is_none());
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    assert!(cx.debug_bounds("completion-popup").is_none());
}

#[gpui_kit::test]
fn tag_completion_supports_mouse_selection_in_the_daily_input(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Tagged").unwrap();
    model
        .workspace
        .write_document("data/Tagged.md", "#research")
        .unwrap();
    model.refresh().unwrap();
    // Publish a real document index before opening the UI.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !model.refresh_index() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| window.click("daily-nav", cx));
    cx.simulate_input("记录 #re");
    cx.run_until_parked();
    let row = cx.debug_bounds("completion-option-0").unwrap();
    cx.simulate_click(row.center(), Default::default());
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).editor.read(cx).value().as_str(),
            "记录 #research "
        )
    });
    assert!(cx.debug_bounds("completion-popup").is_none());
}

#[gpui_kit::test]
fn document_completion_preserves_the_text_after_a_unicode_cursor_and_supports_undo(
    cx: &mut TestAppContext,
) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("研究 A").unwrap();
    model.create_note("Draft", false).unwrap();
    model.workspace.write_document("data/Draft.md", "").unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.navigate(Some("data/Draft.md".into()), window, cx)
        })
    });
    cx.simulate_keystrokes("secondary-e");
    cx.simulate_input("你好 [[研 后文");
    for _ in 0..3 {
        cx.simulate_keystrokes("left");
    }
    cx.run_until_parked();
    assert!(cx.debug_bounds("completion-popup").is_some());
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).editor.read(cx).value().as_str(),
            "你好 [[研究 A]] 后文"
        )
    });
    cx.simulate_keystrokes("secondary-z");
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).editor.read(cx).value().as_str(),
            "你好 [[研 后文"
        )
    });
}
