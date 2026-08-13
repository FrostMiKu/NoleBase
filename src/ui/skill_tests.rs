use ratatui::backend::TestBackend;
use ratatui::Terminal;

use super::*;
use crate::app::{DialogOption, DialogState};
use crate::storage::Storage;

fn make_app() -> (App, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::new(directory.path()).unwrap();
    storage.ensure_files().unwrap();
    (App::new(storage).unwrap(), directory)
}

fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            let _ = draw(frame, app);
        })
        .unwrap();
    terminal
}

fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area().width)
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

#[test]
fn skill_browser_uses_two_content_rows_and_the_complete_shared_selection_area() {
    let (mut app, _directory) = make_app();
    app.open_dialog(DialogState::new(
        "Skills · Enter preview",
        String::new(),
        DialogMode::SingleSelect,
        DialogPurpose::SkillBrowser,
        vec![
            DialogOption::with_hint("first-skill", "First description"),
            DialogOption::with_hint("second-skill", "Second description"),
        ],
    ));

    let terminal = render(&mut app, 100, 24);
    let selected = app.dialog_hitboxes.first().unwrap().area;
    let buffer = terminal.backend().buffer();

    assert_eq!(selected.height, 3);
    assert!(row_text(&terminal, selected.y).contains("first-skill"));
    assert!(row_text(&terminal, selected.y + 1).contains("First description"));
    for y in [selected.y - 1, selected.y + 2] {
        for x in selected.x + 1..selected.x + selected.width {
            assert_eq!(buffer[(x, y)].symbol(), " ");
        }
    }

    for y in selected.y - 1..selected.y + selected.height {
        assert_eq!(buffer[(selected.x, y)].symbol(), "▌");
        assert_eq!(buffer[(selected.x, y)].fg, app.theme.selection_indicator);
        assert_eq!(
            buffer[(selected.x + 1, y)].bg,
            app.theme.selection_background
        );
    }
}

#[test]
fn skill_browser_last_item_keeps_description_and_selection_above_footer() {
    let (mut app, _directory) = make_app();
    let mut dialog = DialogState::new(
        "Skills · Enter preview",
        String::new(),
        DialogMode::SingleSelect,
        DialogPurpose::SkillBrowser,
        vec![
            DialogOption::with_hint("first-skill", "First description"),
            DialogOption::with_hint("last-skill", "Last description"),
        ],
    );
    dialog.selected = 1;
    app.open_dialog(dialog);

    let terminal = render(&mut app, 100, 24);
    let selected = app.dialog_hitboxes[1].area;
    let buffer = terminal.backend().buffer();
    let footer_y = (0..buffer.area().height)
        .find(|&y| row_text(&terminal, y).contains("Enter preview · Esc close"))
        .expect("skill browser footer should be visible");

    assert_eq!(selected.height, 3);
    assert!(row_text(&terminal, selected.y).contains("last-skill"));
    assert!(row_text(&terminal, selected.y + 1).contains("Last description"));
    assert!(selected.y + selected.height < footer_y);
    for y in selected.y - 1..selected.y + selected.height {
        assert_eq!(buffer[(selected.x, y)].symbol(), "▌");
        assert_eq!(
            buffer[(selected.x + 1, y)].bg,
            app.theme.selection_background
        );
    }
    assert_ne!(
        buffer[(selected.x + 1, footer_y)].bg,
        app.theme.selection_background
    );
}
