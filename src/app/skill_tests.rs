use std::fs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;

fn make_app() -> (App, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::new(directory.path()).unwrap();
    storage.ensure_files().unwrap();
    (App::new(storage).unwrap(), directory)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn write_skill(app: &App, id: &str, description: &str, body: &str) -> PathBuf {
    let directory = app.storage.skills_dir.join(id);
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(crate::skill::SKILL_FILE_NAME);
    fs::write(
        &path,
        format!("---\nname: {id}\ndescription: {description}\n---\n\n{body}\n"),
    )
    .unwrap();
    path
}

#[test]
fn command_palette_skill_browser_lists_name_and_description_then_previews() {
    let (mut app, _directory) = make_app();
    let alpha = write_skill(&app, "alpha", "Alpha description", "# Alpha body");

    app.execute_app_command(AppCommand::BrowseSkills);

    assert_eq!(app.overlay, Some(Overlay::Dialog));
    let dialog = app.dialog.as_ref().unwrap();
    assert_eq!(dialog.purpose, DialogPurpose::SkillBrowser);
    assert_eq!(dialog.options[0].label, "alpha");
    assert_eq!(dialog.options[0].hint.as_deref(), Some("Alpha description"));
    assert!(dialog
        .options
        .iter()
        .any(|option| option.label == "create-skill"));

    app.handle_key(key(KeyCode::Enter));
    let document = app.document.as_ref().unwrap();
    assert_eq!(
        document.kind,
        DocumentKind::Skill(fs::canonicalize(alpha).unwrap())
    );
    assert_eq!(document.title, "alpha");
    assert_eq!(document.source, "# Alpha body");
    assert_eq!(document.return_to, DocumentReturn::Skills);
    assert_eq!(app.current_note_archived(), None);
    assert!(!app.command_available(AppCommand::ArchiveCurrentNote));
}

#[test]
fn skill_document_supports_append_edit_rename_delete_and_returns_to_browser() {
    let (mut app, _directory) = make_app();
    let original = write_skill(&app, "alpha", "Alpha description", "# Alpha body");
    app.execute_app_command(AppCommand::BrowseSkills);
    app.handle_key(key(KeyCode::Enter));

    assert_eq!(
        app.handle_document(key(KeyCode::Char('e'))),
        Some(Command::Edit(fs::canonicalize(&original).unwrap()))
    );

    app.input = "New instruction".to_string();
    app.input_cursor = app.input.chars().count();
    app.send_message();
    assert!(fs::read_to_string(&original)
        .unwrap()
        .contains("New instruction"));
    assert!(app
        .document
        .as_ref()
        .unwrap()
        .source
        .contains("New instruction"));

    app.rename_current_note();
    {
        let dialog = app.dialog.as_mut().unwrap();
        dialog.input = "renamed-skill".to_string();
        dialog.cursor = dialog.input.chars().count();
    }
    app.handle_key(key(KeyCode::Enter));
    let renamed = app
        .storage
        .skills_dir
        .join("renamed-skill")
        .join(crate::skill::SKILL_FILE_NAME);
    assert!(!original.exists());
    assert!(renamed.exists());
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::Skill(fs::canonicalize(&renamed).unwrap()))
    );

    app.delete_current_note();
    app.handle_key(key(KeyCode::Enter));
    assert!(!renamed.exists());
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::SkillBrowser)
    );

    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.center_view, CenterView::Daily);
    assert_eq!(app.overlay, None);
}

#[test]
fn escape_from_skill_preview_restores_the_browser_selection() {
    let (mut app, _directory) = make_app();
    write_skill(&app, "alpha", "Alpha description", "# Alpha body");
    write_skill(&app, "beta", "Beta description", "# Beta body");
    app.execute_app_command(AppCommand::BrowseSkills);
    app.handle_key(key(KeyCode::Down));
    let selected = app.dialog_selected();
    let selected_name = app.skill_entries[selected].name.clone();
    app.handle_key(key(KeyCode::Enter));

    app.handle_document(key(KeyCode::Esc));

    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::SkillBrowser)
    );
    assert_eq!(app.dialog_selected(), selected);
    assert_eq!(app.skill_entries[selected].name, selected_name);
}
