use super::*;
use gpui_kit::test::TestWindowExt;

fn choose_context_action(cx: &mut gpui_kit::VisualTestContext, path: &str, action: &str) {
    cx.run_until_parked();
    let position = cx.update(|window, cx| {
        window.render_frame(cx);
        window
            .find(gpui_kit::SharedString::from(format!("file-{path}")))
            .bounds()
            .center()
    });
    cx.simulate_mouse_down(position, gpui_kit::MouseButton::Right, Default::default());
    cx.simulate_mouse_up(position, gpui_kit::MouseButton::Right, Default::default());
    cx.run_until_parked();
    let target = cx.update(|window, cx| {
        window.render_frame(cx);
        window
            .within("popup-menu")
            .find_by_label(action)
            .bounds()
            .center()
    });
    cx.simulate_click(target, Default::default());
    cx.run_until_parked();
}

#[gpui_kit::test]
fn file_tree_context_actions_preserve_edits_update_links_and_follow_the_document(
    cx: &mut TestAppContext,
) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.create_note("Original", false).unwrap();
    model.workspace.create_note("Links").unwrap();
    model
        .workspace
        .write_document("data/Links.md", "[[Original]]")
        .unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.navigate(Some("data/Original.md".into()), window, cx);
            view.editor.update(cx, |editor, cx| {
                editor.replace_all("Saved draft", window, cx)
            });
        });
    });
    choose_context_action(cx, "data/Original.md", "重命名");
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.note_action_name
                .update(cx, |input, cx| input.set_value("Renamed", window, cx));
        });
        window.click("confirm-note-action", cx);
        let view = view.read(cx);
        assert_eq!(view.model.selected.as_deref(), Some("data/Renamed.md"));
        assert_eq!(
            view.model
                .workspace
                .read_document("data/Renamed.md")
                .unwrap(),
            "Saved draft"
        );
        assert_eq!(
            view.model.workspace.read_document("data/Links.md").unwrap(),
            "[[Renamed]]"
        );
    });
    choose_context_action(cx, "data/Renamed.md", "归档");
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert_eq!(view.model.selected.as_deref(), Some("archives/Renamed.md"));
        assert_eq!(view.editor.read(cx).value().as_str(), "Saved draft");
    });
    choose_context_action(cx, "archives/Renamed.md", "重命名");
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.note_action_name
                .update(cx, |input, cx| input.set_value("Archived", window, cx))
        });
        window.click("confirm-note-action", cx);
        assert_eq!(
            view.read(cx).model.selected.as_deref(),
            Some("archives/Archived.md")
        );
    });
    choose_context_action(cx, "archives/Archived.md", "恢复");
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).model.selected.as_deref(),
            Some("data/Archived.md")
        )
    });
    choose_context_action(cx, "data/Archived.md", "删除");
    cx.update(|window, cx| {
        window.click("cancel-note-action", cx);
        assert!(view
            .read(cx)
            .model
            .workspace
            .read_document("data/Archived.md")
            .is_ok());
    });
    choose_context_action(cx, "data/Archived.md", "删除");
    cx.update(|window, cx| {
        window.click("confirm-note-action", cx);
        let view = view.read(cx);
        assert!(view.model.selected.is_none());
        assert!(view.page == Page::Home);
        assert!(view
            .model
            .workspace
            .read_document("data/Archived.md")
            .is_err());
    });
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert!(!root.path().join("data/Archived.md").exists());
}

#[gpui_kit::test]
fn renaming_to_an_existing_note_keeps_the_original_and_dialog_open(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Original").unwrap();
    model.workspace.create_note("Existing").unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    choose_context_action(cx, "data/Original.md", "重命名");
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.note_action_name
                .update(cx, |input, cx| input.set_value("Existing", window, cx))
        });
        window.click("confirm-note-action", cx);
        let view = view.read(cx);
        assert!(view.note_action.is_some());
        assert!(!view.status.is_empty());
        assert!(view
            .model
            .workspace
            .read_document("data/Original.md")
            .is_ok());
        assert!(view
            .model
            .workspace
            .read_document("data/Existing.md")
            .is_ok());
    });
}
