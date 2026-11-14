use super::*;
use crate::core::storage::Storage;
use gpui_kit::{point, size, TestAppContext};

fn desktop_window(
    cx: &mut TestAppContext,
    model: Desktop,
) -> (Entity<DesktopView>, &mut gpui_kit::VisualTestContext) {
    cx.update(crate::gui::bind_shortcuts);
    let mut desktop = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| DesktopView::new(model, window, cx));
        desktop = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    (desktop.unwrap(), cx)
}

struct Sidebar(Entity<DesktopView>);
impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.0.update(cx, |view, cx| view.sidebar(cx))
    }
}

#[gpui_kit::test]
fn note_list_reserves_space_and_keeps_selected_rows_aligned(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.create_note("Test note", false).unwrap();
    cx.update(|cx| theme::apply(model.theme.theme, cx));
    let (view, cx) = desktop_window(cx, model);
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.page = Page::Document;
            cx.notify();
        })
    });
    let sidebar = cx.update(|_, cx| cx.new(|_| Sidebar(view.clone())));
    cx.draw(
        point(px(0.), px(0.)),
        size(px(SIDEBAR_WIDTH), px(700.)),
        |_, _| sidebar.into_any_element(),
    );
    let list = cx.debug_bounds("note-list").unwrap();
    let row = cx.debug_bounds("first-note").unwrap();
    assert!(row.origin.y >= list.origin.y + px(LIST_TOP));
    assert_eq!(row.size.height, px(NOTE_ROW_HEIGHT + 2.));
    assert_eq!(row.size.width, list.size.width - px(8.));
    assert!(cx.debug_bounds("selected-indicator").is_none());
}

#[gpui_kit::test]
fn navigation_saves_the_draft_before_opening_another_document(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.create_note("Test note", false).unwrap();
    let path = model.selected.clone().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.editor.update(cx, |editor, cx| {
                editor.set_value("# Saved draft", window, cx)
            });
            view.navigate(None, window, cx);
            assert_eq!(
                view.model.workspace.read_document(&path).unwrap(),
                "# Saved draft"
            );
            assert!(view.model.selected.is_none());
            assert!(view.editor.read(cx).value().is_empty());
        })
    });
}
#[gpui_kit::test]
fn document_editor_grows_with_the_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.create_note("Test note", false).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.page = Page::Document;
            view.preview = false;
            cx.notify();
        })
    });
    cx.simulate_resize(size(px(1280.), px(900.)));
    cx.run_until_parked();
    let tall = cx.debug_bounds("document-editor").unwrap();
    assert!(tall.size.height > px(740.));
    cx.simulate_resize(size(px(1280.), px(640.)));
    cx.run_until_parked();
    let short = cx.debug_bounds("document-editor").unwrap();
    assert_eq!(tall.size.height - short.size.height, px(260.));
}
#[gpui_kit::test]
fn note_grid_reflows_as_the_window_narrows(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Alpha").unwrap();
    model.workspace.create_note("Beta").unwrap();
    model.refresh().unwrap();
    let (_, cx) = desktop_window(cx, model);
    cx.simulate_resize(size(px(1360.), px(900.)));
    cx.run_until_parked();
    let first = cx.debug_bounds("first-card").unwrap();
    let second = cx.debug_bounds("second-card").unwrap();
    assert_eq!(first.origin.y, second.origin.y);
    assert!(second.origin.x > first.origin.x + first.size.width);
    cx.simulate_resize(size(px(600.), px(900.)));
    cx.run_until_parked();
    let first = cx.debug_bounds("first-card").unwrap();
    let second = cx.debug_bounds("second-card").unwrap();
    assert_eq!(first.origin.x, second.origin.x);
    assert!(second.origin.y >= first.origin.y + first.size.height);
}

#[gpui_kit::test]
fn large_vault_scroll_renders_only_the_visible_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.create_note("Test note", false).unwrap();
    let template = model.documents[0].clone();
    model.documents = (0..10_000)
        .map(|index| {
            let mut document = template.clone();
            document.path = format!("data/Note-{index}.md");
            document.title = format!("Note {index}");
            document
        })
        .collect();
    model.rebuild_collections();
    let (view, cx) = desktop_window(cx, model);
    cx.simulate_resize(size(px(1360.), px(900.)));
    cx.run_until_parked();
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert!(
            view.home_rendered.get() < 100,
            "cards: {}",
            view.home_rendered.get()
        );
        assert!(
            view.sidebar_rendered.get() < 200,
            "sidebar: {}",
            view.sidebar_rendered.get()
        );
        view.home_rendered.set(0);
        view.sidebar_rendered.set(0);
    });
    cx.simulate_event(gpui_kit::ScrollEvent {
        position: point(px(800.), px(500.)),
        delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-20_000.))),
        modifiers: gpui_kit::Modifiers::default(),
        touch_phase: gpui_kit::TouchPhase::Moved,
        source: gpui_kit::ScrollSource::default(),
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let view = view.read(cx);
        let cards = view.home_rendered.get();
        assert!(cards > 0 && cards < 100, "cards after scrolling: {cards}");
        assert!(view.sidebar_rendered.get() < 200);
        eprintln!("10,000 notes: rendered {cards} cards after scrolling");
    });
}

#[gpui_kit::test]
fn text_buttons_center_their_label(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (_, cx) = desktop_window(cx, model);
    let button = cx.debug_bounds("new-note").unwrap();
    let label = cx.debug_bounds("new-note-label").unwrap();
    assert!((button.center().y - label.center().y).abs() <= px(1.));
    let button = cx.debug_bounds("empty-new").unwrap();
    let label = cx.debug_bounds("empty-new-label").unwrap();
    assert!((button.center().x - label.center().x).abs() <= px(1.));
    assert!((button.center().y - label.center().y).abs() <= px(1.));
}

#[gpui_kit::test]
fn approval_control_sends_the_users_decision(cx: &mut TestAppContext) {
    use crate::core::agent::{ApprovalDecision, ApprovalKind, ApprovalRequest};
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    let (tx, mut decisions) = tokio::sync::mpsc::unbounded_channel();
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_page(Page::Agent, window, cx);
            let agent = view.agent.as_mut().unwrap();
            agent.current.service.set_approval_sender_for_test(tx);
            agent.approval = Some(ApprovalRequest {
                title: "更新笔记".into(),
                message: "确认更新笔记内容。".into(),
                kind: ApprovalKind::Confirm,
            });
            cx.notify();
        })
    });
    cx.run_until_parked();
    assert!(decisions.try_recv().is_err());
    let approve = cx.debug_bounds("approve-agent").unwrap();
    cx.simulate_click(approve.center(), gpui_kit::Modifiers::default());
    assert_eq!(decisions.try_recv().unwrap(), ApprovalDecision::Approve);
    cx.update(|_, cx| assert!(view.read(cx).agent.as_ref().unwrap().approval.is_none()));
}

#[gpui_kit::test]
fn private_agent_input_is_cleared_and_kept_out_of_the_transcript(cx: &mut TestAppContext) {
    use crate::core::agent::{PrivateTerminalInputDecision, PrivateTerminalInputRequest};
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    let (tx, mut inputs) = tokio::sync::mpsc::unbounded_channel();
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_page(Page::Agent, window, cx);
            view.agent
                .as_ref()
                .unwrap()
                .current
                .service
                .set_private_input_sender_for_test(tx);
            view.private_input
                .update(cx, |input, cx| input.set_value("private value", window, cx));
            view.agent.as_mut().unwrap().private_input = Some(PrivateTerminalInputRequest {
                session_id: "test".into(),
                purpose: "输入测试".into(),
                prompt: "请输入".into(),
            });
            cx.notify();
        })
    });
    cx.run_until_parked();
    let submit = cx.debug_bounds("submit-private").unwrap();
    cx.simulate_click(submit.center(), gpui_kit::Modifiers::default());
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert!(view.private_input.read(cx).value().is_empty());
        assert!(view.agent.as_ref().unwrap().panel.is_empty());
    });
    match inputs.try_recv().unwrap() {
        PrivateTerminalInputDecision::Submit(value) => assert_eq!(value.as_str(), "private value"),
        PrivateTerminalInputDecision::Cancelled => panic!("expected submitted value"),
    }
}

#[gpui_kit::test]
fn agent_question_accepts_a_custom_answer(cx: &mut TestAppContext) {
    use crate::core::agent::{AskUserRequest, AskUserResponse};
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    let (tx, mut answers) = tokio::sync::mpsc::unbounded_channel();
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_page(Page::Agent, window, cx);
            let agent = view.agent.as_mut().unwrap();
            agent.current.service.set_user_sender_for_test(tx);
            agent.question = Some(AskUserRequest {
                question: "选择整理方式".into(),
                options: vec!["按时间".into(), "按主题".into()],
            });
            view.answer_input
                .update(cx, |input, cx| input.set_value("按项目整理", window, cx));
            cx.notify();
        })
    });
    cx.run_until_parked();
    let submit = cx.debug_bounds("answer-agent").unwrap();
    cx.simulate_click(submit.center(), gpui_kit::Modifiers::default());
    assert_eq!(
        answers.try_recv().unwrap(),
        AskUserResponse::Answer("按项目整理".into())
    );
    cx.update(|_, cx| assert!(view.read(cx).answer_input.read(cx).value().is_empty()));
}

#[gpui_kit::test]
fn home_new_note_creates_the_named_document(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    let new = cx.debug_bounds("new-note").unwrap();
    cx.simulate_click(new.center(), gpui_kit::Modifiers::default());
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            assert!(view.new_note_open);
            view.new_note_name
                .update(cx, |input, cx| input.set_value("Project Atlas", window, cx));
        })
    });
    let create = cx.debug_bounds("confirm-new-note").unwrap();
    cx.simulate_click(create.center(), gpui_kit::Modifiers::default());
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert_eq!(
            view.model.selected.as_deref(),
            Some("data/Project Atlas.md")
        );
        assert!(!view.new_note_open);
        assert!(view.page == Page::Document && !view.preview);
        assert_eq!(
            view.model
                .workspace
                .read_document("data/Project Atlas.md")
                .unwrap(),
            "# Project Atlas\n\n"
        );
    });
}

#[gpui_kit::test]
fn home_multi_selection_archives_and_restores_the_selected_notes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Alpha").unwrap();
    model.workspace.create_note("Beta").unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    let manage = cx.debug_bounds("manage-notes").unwrap();
    cx.simulate_click(manage.center(), gpui_kit::Modifiers::default());
    for selector in ["first-card", "second-card"] {
        let card = cx.debug_bounds(selector).unwrap();
        cx.simulate_click(card.center(), gpui_kit::Modifiers::default());
    }
    cx.update(|_, cx| assert_eq!(view.read(cx).home.selected.len(), 2));
    let archive = cx.debug_bounds("archive-selection").unwrap();
    cx.simulate_click(archive.center(), gpui_kit::Modifiers::default());
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert!(view.model.notes.is_empty());
        assert_eq!(view.model.archives.len(), 2);
    });
    let tab = cx.debug_bounds("tree-archives").unwrap();
    cx.simulate_click(tab.center(), gpui_kit::Modifiers::default());
    cx.update(|_, cx| assert_eq!(view.read(cx).home.collection, Collection::Archives));
    let manage = cx.debug_bounds("manage-notes").unwrap();
    cx.simulate_click(manage.center(), gpui_kit::Modifiers::default());
    let card = cx.debug_bounds("first-card").unwrap();
    cx.simulate_click(card.center(), gpui_kit::Modifiers::default());
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).home.selected.len(),
            1,
            "selection after clicking archive card"
        )
    });
    let restore = cx.debug_bounds("archive-selection").unwrap();
    cx.simulate_click(restore.center(), gpui_kit::Modifiers::default());
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert_eq!(view.model.notes.len(), 1, "{}", view.status);
        assert_eq!(view.model.archives.len(), 1);
        assert!(view.home.selected.is_empty());
    });
}

#[gpui_kit::test]
fn search_result_opens_the_matching_source_line(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Match").unwrap();
    model
        .workspace
        .write_document("data/Match.md", "# Match\n\nneedle\n")
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while model.search("needle").is_empty() {
        model.refresh_index();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    model.query = "needle".into();
    model.search_results = model.search("needle");
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_page(Page::Search, window, cx)));
    cx.run_until_parked();
    let result = cx.debug_bounds("first-search-result").unwrap();
    cx.simulate_click(result.center(), gpui_kit::Modifiers::default());
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert_eq!(view.model.selected.as_deref(), Some("data/Match.md"));
        assert_eq!(view.editor.read(cx).cursor_position().line, 2);
        assert!(!view.preview);
    });
}

#[gpui_kit::test]
fn document_changes_save_automatically(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.create_note("Autosave", false).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.page = Page::Document;
            view.preview = false;
            cx.notify();
        })
    });
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.editor.update(cx, |input, cx| {
                input.replace_all("# Autosave\n\nSaved while editing.", window, cx)
            })
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
                .read_document("data/Autosave.md")
                .unwrap(),
            "# Autosave\n\nSaved while editing."
        );
        assert!(!view.save_pending);
    });
}

#[gpui_kit::test]
fn automatic_save_preserves_an_external_edit_and_the_local_draft(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.create_note("Conflict", false).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.editor.update(cx, |input, cx| {
                input.set_value("My local draft", window, cx)
            });
            view.model
                .workspace
                .write_document("data/Conflict.md", "External version")
                .unwrap();
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
                .read_document("data/Conflict.md")
                .unwrap(),
            "External version"
        );
        assert_eq!(view.editor.read(cx).value().as_str(), "My local draft");
    });
}

#[gpui_kit::test]
fn tags_fill_the_main_workspace_and_titles_align_to_its_left_edge(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.append_to_today("A note #research").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while model.tags.is_empty() {
        model.refresh_index();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let (view, cx) = desktop_window(cx, model);
    cx.simulate_resize(size(px(1360.), px(900.)));
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_page(Page::Tags, window, cx)));
    cx.run_until_parked();
    let workspace = cx.debug_bounds("main-workspace").unwrap();
    let body = cx.debug_bounds("page-content").unwrap();
    let tag = cx.debug_bounds("first-tag").unwrap();
    let title = cx.debug_bounds("page-title").unwrap();
    assert_eq!(body.size.width, workspace.size.width);
    assert!((tag.size.width - (workspace.size.width - px(48.))).abs() <= px(1.));
    assert_eq!(title.origin.x, workspace.origin.x + px(24.));
    assert_eq!(workspace.right(), px(1360.));
    assert!(cx.debug_bounds("todo-page").is_none());
}

#[gpui_kit::test]
fn home_keeps_large_cards_and_todo_toggles_persistently(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Card").unwrap();
    model
        .workspace
        .append_to_today("- [ ] Finish the note")
        .unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.simulate_resize(size(px(1360.), px(900.)));
    cx.run_until_parked();
    let card = cx.debug_bounds("first-card").unwrap();
    assert!(card.size.width > px(200.) && card.size.height > px(200.));
    let nav = cx.debug_bounds("todo-nav").unwrap();
    cx.simulate_click(nav.center(), gpui_kit::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("todo-page").unwrap().size.width,
        cx.debug_bounds("main-workspace").unwrap().size.width - px(48.)
    );
    let task = cx.debug_bounds("first-todo").unwrap();
    cx.simulate_click(task.center(), gpui_kit::Modifiers::default());
    cx.update(|_, cx| assert!(view.read(cx).model.workspace.todos().unwrap()[0].checked));
    let task = cx.debug_bounds("first-todo").unwrap();
    cx.simulate_click(task.center(), gpui_kit::Modifiers::default());
    cx.update(|_, cx| assert!(!view.read(cx).model.workspace.todos().unwrap()[0].checked));
}

#[gpui_kit::test]
fn agent_enter_sends_shift_enter_adds_a_line_and_save_shortcut_keeps_the_draft(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    std::fs::write(&model.workspace.storage().ai_config_path, "api_key = ''\n").unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_page(Page::Agent, window, cx);
            view.agent_input
                .update(cx, |input, cx| input.insert("First line", window, cx));
        })
    });
    cx.simulate_keystrokes("secondary-s");
    cx.update(|_, cx| assert!(view.read(cx).agent.as_ref().unwrap().panel.is_empty()));
    cx.simulate_keystrokes("shift-enter");
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).agent_input.read(cx).value().as_str(),
            "First line\n"
        )
    });
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert!(view.agent_input.read(cx).value().is_empty());
        assert!(view.agent.as_ref().unwrap().panel.entries().iter().any(|entry| matches!(entry.as_ref(), crate::core::agent_session::AgentPanelEntry::Prompt { text, .. } if text == "First line")));
    });
}

#[gpui_kit::test]
fn enter_during_ime_composition_keeps_the_agent_message_as_a_draft(cx: &mut TestAppContext) {
    use gpui_kit::EntityInputHandler;
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_page(Page::Agent, window, cx);
            view.agent_input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "拼音", Some(0..2), window, cx)
            });
        })
    });
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| assert!(view.read(cx).agent.as_ref().unwrap().panel.is_empty()));
}

#[gpui_kit::test]
fn automatic_save_waits_for_ime_text_to_be_committed(cx: &mut TestAppContext) {
    use gpui_kit::EntityInputHandler;
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("IME").unwrap();
    model.workspace.write_document("data/IME.md", "").unwrap();
    model.open_document(Some("data/IME.md".into())).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.editor.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "拼", Some(0..1), window, cx)
            })
        })
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx)
                .model
                .workspace
                .read_document("data/IME.md")
                .unwrap(),
            ""
        )
    });
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.editor.update(cx, |input, cx| {
                input.replace_text_in_range(None, "拼音", window, cx)
            })
        })
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx)
                .model
                .workspace
                .read_document("data/IME.md")
                .unwrap(),
            "拼音"
        )
    });
}

#[gpui_kit::test]
fn folders_toggle_their_children_independently_and_stay_near_the_sidebar_edge(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Current").unwrap();
    model.workspace.create_note("Past").unwrap();
    model
        .workspace
        .archive_note(&model.workspace.document_path("data/Past.md").unwrap())
        .unwrap();
    let session = model.sessions.draft("Research").unwrap();
    model.sessions.retain(session).unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.simulate_resize(size(px(1360.), px(900.)));
    cx.run_until_parked();
    for (slot, selector) in [(0, "tree-notes"), (2, "tree-agents"), (1, "tree-archives")] {
        let row = cx.debug_bounds(selector).unwrap();
        assert!(row.origin.x <= px(8.));
        cx.simulate_click(row.center(), gpui_kit::Modifiers::default());
        cx.run_until_parked();
        cx.update(|_, cx| {
            let view = view.read(cx);
            assert!(!view.tree_expanded[slot]);
            assert!(!view.tree_rows.iter().any(|row| match (slot, row) {
                (0, FileListRow::File(index)) => !view.model.documents[*index].archived,
                (1, FileListRow::File(index)) => view.model.documents[*index].archived,
                (2, FileListRow::Agent(_)) => true,
                _ => false,
            }));
        });
        let row = cx.debug_bounds(selector).unwrap();
        cx.simulate_click(row.center(), gpui_kit::Modifiers::default());
        cx.run_until_parked();
        cx.update(|_, cx| assert!(view.read(cx).tree_expanded[slot]));
    }
}

#[gpui_kit::test]
fn markdown_table_fits_the_page_and_wraps_cells_on_resize(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.create_note("Table", false).unwrap();
    let path = model.selected.clone().unwrap();
    let prose = "A long description with several words that should wrap inside the table column. "
        .repeat(4);
    model.workspace.write_document(&path, &format!("| Name | Description | Status |\n| --- | --- | --- |\n| Example | {prose} | Ready |\n")).unwrap();
    model.open_document(Some(path)).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.page = Page::Document;
            view.preview = true;
            cx.notify();
        })
    });
    cx.simulate_resize(size(px(1360.), px(900.)));
    cx.run_until_parked();
    let table = cx.debug_bounds("markdown-view").unwrap();
    let workspace = cx.debug_bounds("main-workspace").unwrap();
    let wide = cx.debug_bounds("markdown-view").unwrap();
    assert!(table.size.width <= workspace.size.width - px(48.));
    assert!(wide.size.height > px(80.));
    cx.simulate_resize(size(px(820.), px(900.)));
    cx.run_until_parked();
    let narrow = cx.debug_bounds("markdown-view").unwrap();
    assert!(narrow.size.width < wide.size.width);
    assert!(narrow.size.width <= px(540.));
}

#[gpui_kit::test]
fn agent_thinking_tools_and_markdown_have_distinct_layouts(cx: &mut TestAppContext) {
    use crate::core::agent_session::AgentPanelEntry;
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.simulate_resize(size(px(1360.), px(1000.)));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_page(Page::Agent, window, cx);
            let agent = view.agent.as_mut().unwrap();
            agent.panel.push_entry(AgentPanelEntry::Thinking {
                text: "Inspect the notes before answering.".into(),
                streaming: false,
            });
            agent.panel.push_entry(AgentPanelEntry::Tool {
                text: "Read notes.md\nMatched the requested document".into(),
                active: false,
                preview: Some("Found three useful sections".into()),
            });
            agent.panel.push_entry(AgentPanelEntry::Assistant {
                text: "## Findings\n\nThe **notes** contain the answer.".into(),
                streaming: false,
                final_output: true,
            });
            agent.rebuild_rows();
            view.agent_list.reset(agent.rows.len());
            cx.notify();
        })
    });
    cx.run_until_parked();
    let thinking = cx.debug_bounds("agent-thinking-card").unwrap();
    let tool = cx.debug_bounds("agent-tool-status").unwrap();
    let result = cx.debug_bounds("agent-tool-result").unwrap();
    assert!(thinking.size.height > px(60.));
    assert_eq!(tool.size.height, px(26.));
    assert!(result.origin.y >= tool.bottom());
    assert!(result.size.height > px(30.));
    cx.update(|_, cx| {
        use crate::gui::agent::AgentRowKind;
        let agent = view.read(cx).agent.as_ref().unwrap();
        assert!(agent.rows.iter().any(|row| matches!(row.kind, AgentRowKind::Assistant) && row.text.starts_with("## Findings")));
        assert!(agent.rows.iter().any(|row| matches!(&row.kind, AgentRowKind::Tool { preview: Some(preview), .. } if preview.as_ref() == "Found three useful sections")));
    });
}

#[gpui_kit::test]
fn reading_and_editing_share_a_centered_measure_that_adapts_to_the_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.create_note("Reading layout", false).unwrap();
    let (view, cx) = desktop_window(cx, model);
    for (window_width, expected_width) in [(1600., 800.), (820., 540.)] {
        cx.simulate_resize(size(px(window_width), px(900.)));
        let mut reading = None;
        for preview in [true, false] {
            cx.update(|_, cx| {
                view.update(cx, |view, cx| {
                    view.page = Page::Document;
                    view.preview = preview;
                    cx.notify();
                })
            });
            cx.run_until_parked();
            let column = cx
                .debug_bounds(if preview {
                    "document-column"
                } else {
                    "document-editor"
                })
                .unwrap();
            let workspace = cx.debug_bounds("main-workspace").unwrap();
            assert_eq!(column.size.width, px(expected_width));
            let left = column.origin.x - workspace.origin.x;
            let right = workspace.right() - column.right();
            assert_eq!(left, right);
            assert!(left >= px(24.));
            if let Some(reading) = reading {
                assert_eq!(column.origin.x, reading);
            } else {
                reading = Some(column.origin.x);
            }
        }
    }
}

#[gpui_kit::test]
fn main_views_share_the_toolbar_content_insets(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Alignment").unwrap();
    model
        .workspace
        .append_to_today("- [ ] A task #research")
        .unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    for width in [1360., 820.] {
        cx.simulate_resize(size(px(width), px(900.)));
        for (page, selector) in [
            (Page::Home, "home-scroll"),
            (Page::Daily, "daily-surface"),
            (Page::Search, "search-surface"),
            (Page::Tags, "tags-surface"),
            (Page::Todo, "todo-page"),
            (Page::Agent, "agent-surface"),
        ] {
            cx.update(|window, cx| view.update(cx, |view, cx| view.open_page(page, window, cx)));
            cx.run_until_parked();
            let host = cx.debug_bounds("page-content").unwrap();
            let body = cx.debug_bounds(selector).unwrap();
            let title = cx.debug_bounds("page-title").unwrap();
            assert_eq!(body.origin.x, title.origin.x, "{selector}");
            assert_eq!(body.origin.y, host.origin.y + px(24.), "{selector}");
            assert_eq!(body.right(), host.right() - px(24.), "{selector}");
            let first_row = match page {
                Page::Home => Some("first-card"),
                Page::Todo => Some("first-todo"),
                _ => None,
            };
            if let Some(selector) = first_row {
                let row = cx.debug_bounds(selector).unwrap();
                assert_eq!(row.origin.x, body.origin.x, "{selector}");
                assert_eq!(row.origin.y, body.origin.y + px(12.), "{selector}");
            }
        }
    }
}

#[gpui_kit::test]
fn cards_show_a_compact_document_page(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Reading").unwrap();
    model.workspace.write_document("data/Reading.md", "# Reading\n\nA paragraph in the document.\n\n## Section\n\nA second paragraph.\n\n> A quote.\n\n- One\n- Two\n\n```rust\nlet x = 1;\n```\n").unwrap();
    model.refresh().unwrap();
    let (_, cx) = desktop_window(cx, model);
    cx.simulate_resize(size(px(1360.), px(900.)));
    cx.run_until_parked();
    let card = cx.debug_bounds("first-card").unwrap();
    let page = cx.debug_bounds("card-page-0").unwrap();
    let title = cx.debug_bounds("card-title-0").unwrap();
    let first = cx.debug_bounds("card-preview-block-0").unwrap();
    let last = cx.debug_bounds("card-preview-block-5").unwrap();
    assert_eq!(page.origin.x, card.origin.x + px(4.));
    assert_eq!(title.origin.y, page.origin.y + px(18.));
    assert_eq!(first.origin.x, title.origin.x);
    assert!(first.origin.y >= title.bottom() + px(8.));
    assert!(first.size.height < px(15.));
    assert!(last.bottom() < card.bottom() - px(18.));
}

#[gpui_kit::test]
fn kit_controls_keep_accessible_names_and_controlled_checkbox_state(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt;
    cx.update(gpui_kit::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("new-note").role(), Some(gpui_kit::Role::Button));
        assert_eq!(window.find("new-note").label(), Some("新建笔记"));
        window.click("new-note", cx);
        assert_eq!(window.find("use-template").checked(), Some(false));
        window.click("use-template", cx);
        assert!(view.read(cx).new_note_template);
        assert_eq!(window.find("use-template").checked(), Some(true));
        window.press("escape", cx);
        assert!(!view.read(cx).new_note_open);
    });
}

#[gpui_kit::test]
fn desktop_shortcuts_open_notes_and_toggle_reading_through_the_registered_keymap(
    cx: &mut TestAppContext,
) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.create_note("Keyboard", false).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.simulate_keystrokes("secondary-n");
    cx.update(|_, cx| assert!(view.read(cx).new_note_open));
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.navigate(Some("data/Keyboard.md".into()), window, cx);
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("secondary-e");
    cx.update(|_, cx| assert!(!view.read(cx).preview));
    cx.simulate_keystrokes("secondary-e");
    cx.update(|_, cx| assert!(view.read(cx).preview));
    cx.simulate_keystrokes("secondary-n");
    cx.update(|_, cx| assert!(view.read(cx).new_note_open));
}
