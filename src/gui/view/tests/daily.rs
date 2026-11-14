use super::*;
use gpui_kit::{test::TestWindowExt, Focusable};

#[gpui_kit::test]
fn clicking_daily_dates_edits_the_same_document_and_updates_the_daily_feed(
    cx: &mut TestAppContext,
) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let storage = Storage::new(root.path()).unwrap();
    storage.ensure_files().unwrap();
    let today = chrono::Local::now().date_naive();
    let yesterday = today - chrono::Duration::days(1);
    for (date, source) in [(today, "Today"), (yesterday, "Yesterday")] {
        std::fs::write(storage.daily_file_path(&date.to_string()).unwrap(), source).unwrap();
    }
    let model = Desktop::open(storage).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| window.click("daily-nav", cx));
    cx.run_until_parked();
    for (date, original, replacement) in [
        (today, "Today", "Today edited"),
        (yesterday, "Yesterday", "Yesterday edited"),
    ] {
        cx.update(|window, cx| {
            window.click(
                gpui_kit::SharedString::from(format!("daily-date-{date}")),
                cx,
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("document-editor").is_some());
        assert!(cx.debug_bounds("document-reading").is_none());
        assert!(cx.debug_bounds("mode-switch").is_some());
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert!(view.page == Page::Document);
                assert!(!view.preview);
                assert_eq!(view.model.selected_daily_date(), Some(date));
                assert!(matches!(
                    view.active_nav(),
                    Some(super::super::sidebar::Nav::Daily)
                ));
                assert_eq!(view.editor.read(cx).value().as_str(), original);
                assert!(view.editor.read(cx).focus_handle(cx).is_focused(window));
                view.editor
                    .update(cx, |input, cx| input.replace_all(replacement, window, cx));
            })
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        cx.update(|_, cx| {
            let view = view.read(cx);
            assert_eq!(
                view.model
                    .workspace
                    .read_document(view.model.selected.as_deref().unwrap())
                    .unwrap(),
                replacement
            );
            assert!(!view.save_pending);
        });
        let preview = cx.debug_bounds("read-mode").unwrap().center();
        cx.simulate_click(preview, Default::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("document-reading").is_some());
        assert!(cx.debug_bounds("document-editor").is_none());
        cx.update(|window, cx| window.click("daily-nav", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("daily-surface").is_some());
        cx.update(|_, cx| {
            let view = view.read(cx);
            assert_eq!(view.model.daily_notes.len(), 2);
            assert_eq!(
                view.model
                    .daily_notes
                    .iter()
                    .find(|note| note.date == date)
                    .unwrap()
                    .body
                    .trim(),
                replacement
            );
            assert!(view.model.selected.is_none());
            assert!(view.editor.read(cx).value().is_empty());
        });
    }
}
