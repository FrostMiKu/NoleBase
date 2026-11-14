use super::*;
use crate::core::agent::{ApprovalDecision, ApprovalKind, ApprovalRequest, CommandApproval};
use crate::core::agent_session::AgentPanelEntry;
use gpui_kit::test::TestWindowExt;

#[gpui_kit::test]
fn command_approval_keeps_actions_visible_for_long_commands(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.simulate_resize(size(px(820.), px(640.)));
    let command = "printf '%s\\n' '完整命令'\n".repeat(25);
    for (selector, decision) in [
        ("approve-agent", ApprovalDecision::Approve),
        ("deny-agent", ApprovalDecision::Deny),
    ] {
        let (tx, mut decisions) = tokio::sync::mpsc::unbounded_channel();
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_page(Page::Agent, window, cx);
                let agent = view.agent.as_mut().unwrap();
                agent.current.service.set_approval_sender_for_test(tx);
                agent.approval = Some(ApprovalRequest {
                    title: "运行命令".into(),
                    message: String::new(),
                    kind: ApprovalKind::Command(CommandApproval {
                        purpose: "查看工作区内容".into(),
                        label: "Shell".into(),
                        code: command.clone(),
                    }),
                });
                cx.notify();
            });
        });
        cx.run_until_parked();
        let card = cx.debug_bounds("agent-approval").unwrap();
        let details = cx.debug_bounds("approval-details").unwrap();
        let action = cx.debug_bounds(selector).unwrap();
        let compose = cx.debug_bounds("send-agent").unwrap();
        assert!(details.size.height <= px(136.));
        assert!(action.top() >= details.bottom());
        assert!(action.bottom() <= card.bottom());
        assert!(card.bottom() <= compose.top());
        cx.update(|window, cx| {
            window.click(selector, cx);
        });
        assert_eq!(decisions.try_recv().unwrap(), decision);
        assert!(decisions.try_recv().is_err());
        cx.run_until_parked();
        assert!(cx.debug_bounds("agent-approval").is_none());
    }
}

#[gpui_kit::test]
fn prompt_text_supports_drag_selection_and_keyboard_copy(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    let prompt = "alpha beta gamma delta epsilon";
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_page(Page::Agent, window, cx);
            let agent = view.agent.as_mut().unwrap();
            agent.panel.push_prompt(prompt, false);
            agent.rebuild_rows();
            view.agent_list.reset(agent.rows.len());
            cx.notify();
        });
    });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("agent-prompt").unwrap();
    let start = point(bounds.left() + px(40.), bounds.top() + px(25.));
    let end = point(start.x + px(110.), start.y);
    cx.simulate_mouse_down(start, gpui_kit::MouseButton::Left, Default::default());
    cx.simulate_mouse_move(end, Some(gpui_kit::MouseButton::Left), Default::default());
    cx.simulate_mouse_up(end, gpui_kit::MouseButton::Left, Default::default());
    cx.simulate_keystrokes("secondary-c");
    cx.update(|_, cx| {
        let selected = cx.read_from_clipboard().unwrap().text().unwrap();
        assert!(!selected.is_empty());
        assert!(prompt.contains(&selected), "{selected}");
    });
}

#[gpui_kit::test]
fn agent_reply_thinking_and_tool_results_support_drag_selection(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    let text = "alpha beta gamma delta epsilon";
    for (entry, selector, inset) in [
        (
            AgentPanelEntry::Assistant {
                text: text.into(),
                streaming: false,
                final_output: true,
            },
            "markdown-view",
            0.,
        ),
        (
            AgentPanelEntry::Thinking {
                text: text.into(),
                streaming: false,
            },
            "agent-thinking-body",
            0.,
        ),
        (
            AgentPanelEntry::Tool {
                text: format!("Completed Shell.\n{text}"),
                active: false,
                preview: None,
            },
            "agent-tool-result",
            15.,
        ),
        (AgentPanelEntry::Error(text.into()), "agent-error", 0.),
    ] {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_page(Page::Agent, window, cx);
                let agent = view.agent.as_mut().unwrap();
                agent.panel = Default::default();
                agent.panel.push_entry(entry);
                agent.rebuild_rows();
                view.agent_list.reset(agent.rows.len());
                cx.notify();
            });
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("sentinel".into()));
        });
        cx.run_until_parked();
        if selector == "agent-tool-result" {
            assert!(cx.debug_bounds(selector).is_none());
            let toggle = cx.debug_bounds("agent-tool-toggle").unwrap().center();
            cx.simulate_click(toggle, Default::default());
            cx.run_until_parked();
        }
        let bounds = cx.debug_bounds(selector).unwrap();
        let start = point(bounds.left() + px(inset + 5.), bounds.center().y);
        let end = point(start.x + px(100.), start.y);
        cx.simulate_mouse_down(start, gpui_kit::MouseButton::Left, Default::default());
        cx.simulate_mouse_move(end, Some(gpui_kit::MouseButton::Left), Default::default());
        cx.simulate_mouse_up(end, gpui_kit::MouseButton::Left, Default::default());
        cx.simulate_keystrokes("secondary-c");
        cx.update(|_, cx| {
            let selected = cx.read_from_clipboard().unwrap().text().unwrap();
            assert!(!selected.is_empty(), "{selector}");
            assert!(text.contains(&selected), "{selector}: {selected}");
            assert!(view.read(cx).expanded_thinking.is_empty());
        });
    }
}

#[gpui_kit::test]
fn tool_details_expand_and_collapse_while_status_stays_visible(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_page(Page::Agent, window, cx);
            let agent = view.agent.as_mut().unwrap();
            agent.panel.push_entry(AgentPanelEntry::Tool {
                text: "Calling Shell...\nprintf hello".into(),
                active: true,
                preview: Some("hello\nworld".into()),
            });
            agent.rebuild_rows();
            view.agent_list.reset(agent.rows.len());
            cx.notify();
        });
    });
    cx.run_until_parked();
    for expanded in [false, true, false] {
        assert!(cx.debug_bounds("agent-tool-status").is_some());
        assert_eq!(cx.debug_bounds("agent-tool-result").is_some(), expanded);
        let toggle = cx.debug_bounds("agent-tool-toggle").unwrap().center();
        cx.simulate_click(toggle, Default::default());
        cx.run_until_parked();
    }
}

fn click_menu_label(cx: &mut gpui_kit::VisualTestContext, label: &str) {
    let target = cx.update(|window, cx| {
        window.render_frame(cx);
        window
            .within("popup-menu")
            .find_by_label(label)
            .bounds()
            .center()
    });
    cx.simulate_click(target, Default::default());
    cx.run_until_parked();
}

#[gpui_kit::test]
fn session_context_actions_rename_and_delete_the_selected_session(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let session = model.sessions.create("Original").unwrap();
    let id = session.summary().id;
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.open_agent_session(id, window, cx));
    });
    for action in ["重命名", "删除"] {
        cx.run_until_parked();
        let position = cx.update(|window, cx| {
            window.render_frame(cx);
            window
                .find(gpui_kit::SharedString::from(id.to_string()))
                .bounds()
                .center()
        });
        cx.simulate_mouse_down(position, gpui_kit::MouseButton::Right, Default::default());
        cx.simulate_mouse_up(position, gpui_kit::MouseButton::Right, Default::default());
        cx.run_until_parked();
        click_menu_label(cx, action);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.session_action_name
                    .update(cx, |input, cx| input.set_value("Renamed", window, cx));
            });
            window.click("confirm-session-action", cx);
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            let view = view.read(cx);
            if action == "重命名" {
                assert_eq!(
                    view.sessions
                        .iter()
                        .find(|session| session.id == id)
                        .unwrap()
                        .title,
                    "Renamed"
                );
                assert_eq!(
                    view.model.sessions.get(id).unwrap().summary().title,
                    "Renamed"
                );
            } else {
                assert!(view.model.sessions.get(id).is_err());
                assert!(view.sessions.is_empty());
                assert_ne!(view.agent.as_ref().unwrap().current.summary().id, id);
                assert!(view.agent.as_ref().unwrap().rows.is_empty());
            }
        });
    }
    assert!(!root.path().join("agents").join(id.to_string()).exists());
}

#[gpui_kit::test]
fn permission_menu_updates_the_session_and_restores_on_reopen(cx: &mut TestAppContext) {
    use crate::core::{agent::PermissionMode, agent_sessions::AgentSessions};
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let id = model.sessions.create("Permissions").unwrap().summary().id;
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.open_agent_session(id, window, cx));
    });
    cx.run_until_parked();
    let composer = cx.debug_bounds("agent-composer").unwrap();
    let permission = cx.debug_bounds("agent-permission").unwrap();
    assert!(permission.left() <= composer.left() + px(24.));
    for (label, mode) in [
        ("AUTO", PermissionMode::Auto),
        ("YOLO", PermissionMode::Yolo),
        ("APPROVE", PermissionMode::Approve),
    ] {
        let (tx, mut decisions) = tokio::sync::mpsc::unbounded_channel();
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                let agent = view.agent.as_mut().unwrap();
                agent.current.service.set_approval_sender_for_test(tx);
                agent.approval = Some(ApprovalRequest {
                    title: "运行命令".into(),
                    message: String::new(),
                    kind: ApprovalKind::Confirm,
                });
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.click("agent-permission", cx));
        cx.run_until_parked();
        click_menu_label(cx, label);
        cx.update(|_, cx| {
            let view = view.read(cx);
            assert_eq!(
                view.agent
                    .as_ref()
                    .unwrap()
                    .current
                    .service
                    .permission_mode(),
                mode
            );
            if mode == PermissionMode::Yolo {
                assert!(view.agent.as_ref().unwrap().approval.is_none());
                assert_eq!(decisions.try_recv().unwrap(), ApprovalDecision::Approve);
            } else {
                assert!(view.agent.as_ref().unwrap().approval.is_some());
                assert!(decisions.try_recv().is_err());
            }
            let restored = AgentSessions::open(view.model.workspace.clone()).unwrap();
            assert_eq!(restored.get(id).unwrap().service.permission_mode(), mode);
        });
    }
}

#[gpui_kit::test]
fn question_wraps_and_keeps_all_options_and_actions_above_the_composer(cx: &mut TestAppContext) {
    use crate::core::agent::{AskUserRequest, AskUserResponse};
    cx.update(crate::gui::init);
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
                question: "第一个问题（研究线）：aaai_sacdecoder_sacfusion_s12.5_a50_875bps_v4 现在 loss 平台在 ~36.4、discriminator loss 还在变化，你希望接下来优先验证哪个方向？".into(),
                options: vec![
                    "架构已经对了，就差 scale / 跑满 1M 步".into(),
                    "先跑满再说，接着根据训练曲线确定判断点".into(),
                    "已经在准备下一个架构改动的分支了".into(),
                    "其实主要目标是完成实验量，再根据这次的训练曲线和评估结果确定下一轮实验的方向".into(),
                ],
            });
            cx.notify();
        });
    });
    for (width, height, count) in [(1360., 900., 10), (820., 1080., 10), (820., 720., 4)] {
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                let question = view.agent.as_mut().unwrap().question.as_mut().unwrap();
                question.options.truncate(4);
                question
                    .options
                    .extend((4..count).map(|index| format!("实验方向 {}", index + 1)));
                cx.notify();
            });
        });
        cx.simulate_resize(size(px(width), px(height)));
        cx.run_until_parked();
        let card = cx.debug_bounds("agent-question").unwrap();
        let heading = cx.debug_bounds("agent-interaction-heading").unwrap();
        let composer = cx.debug_bounds("agent-composer").unwrap();
        let toolbar = cx.debug_bounds("workspace-toolbar").unwrap();
        assert!(card.size.height > px(300.));
        assert!(card.top() >= toolbar.bottom());
        assert!(heading.size.height > px(30.));
        assert!(heading.right() <= card.right());
        assert!(heading.bottom() < cx.debug_bounds("agent-option-0").unwrap().top());
        for selector in [
            "agent-option-0",
            "agent-option-1",
            "agent-option-2",
            "agent-option-3",
            "agent-option-4",
            "agent-option-5",
            "agent-option-6",
            "agent-option-7",
            "agent-option-8",
            "agent-option-9",
        ]
        .into_iter()
        .take(count)
        {
            let option = cx.debug_bounds(selector).unwrap();
            assert!(option.top() >= card.top());
            assert!(option.bottom() <= card.bottom());
            assert!(option.right() <= card.right());
        }
        for selector in ["cancel-answer", "answer-agent"] {
            let action = cx.debug_bounds(selector).unwrap();
            assert!(action.bottom() <= card.bottom());
            assert!(action.bottom() <= composer.top());
        }
        assert!(
            composer.bottom() <= px(height),
            "{width}x{height}: composer={composer:?}, card={card:?}"
        );
    }
    let last_option = cx.debug_bounds("agent-option-3").unwrap().center();
    cx.simulate_click(last_option, Default::default());
    assert_eq!(
        answers.try_recv().unwrap(),
        AskUserResponse::Answer(
            "其实主要目标是完成实验量，再根据这次的训练曲线和评估结果确定下一轮实验的方向".into()
        )
    );
}

#[gpui_kit::test]
fn echo_toggles_a_right_overlay_and_follows_the_current_session(cx: &mut TestAppContext) {
    use crate::core::agent::{AgentEvent, JobKind};
    use crate::core::agent_session::{AgentConversation, TodoItem, TodoStatus};
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let current = model.sessions.create("Current").unwrap();
    let other = model.sessions.create("Other").unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.simulate_resize(size(px(1600.), px(900.)));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_agent_session(current.summary().id, window, cx)
        })
    });
    cx.run_until_parked();
    let composer = cx.debug_bounds("agent-composer").unwrap();
    let surface = cx.debug_bounds("agent-surface").unwrap();
    assert_eq!(composer.center().x, surface.center().x);
    assert!(cx.debug_bounds("agent-echo").is_none());
    let job = current
        .service
        .jobs()
        .start_background(JobKind::Shell, "持续检查工作区中的文件变化")
        .unwrap();
    current.observe(&AgentEvent::ConversationUpdated(AgentConversation {
        todos: vec![
            TodoItem {
                content: "检查现有内容".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                content: "整理当前会话中的研究结论与后续行动项".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                content: "复核结果".into(),
                status: TodoStatus::Pending,
            },
        ],
        ..Default::default()
    }));
    cx.update(|_, cx| view.update(cx, |view, cx| view.poll_runtime(cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-echo").is_none());
    cx.update(|window, cx| window.click("toggle-echo", cx));
    cx.run_until_parked();
    let panel = cx.debug_bounds("agent-echo").unwrap();
    let surface = cx.debug_bounds("agent-surface").unwrap();
    assert!(panel.left() > composer.right());
    assert_eq!(panel.right(), surface.right());
    assert_eq!(panel.top(), surface.top());
    assert!(cx.debug_bounds("agent-jobs-widget").is_some());
    assert!(cx.debug_bounds("agent-todos-widget").is_some());
    assert!(cx.debug_bounds("agent-todo-2").is_some());
    assert_eq!(cx.debug_bounds("agent-composer").unwrap(), composer);
    assert!(current.service.jobs().cancel(&job.id));
    cx.update(|_, cx| view.update(cx, |view, cx| view.poll_runtime(cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-jobs-widget").is_none());
    assert!(cx.debug_bounds("agent-todos-widget").is_some());
    cx.simulate_resize(size(px(820.), px(900.)));
    cx.run_until_parked();
    let narrow_panel = cx.debug_bounds("agent-echo").unwrap();
    let narrow_composer = cx.debug_bounds("agent-composer").unwrap();
    assert!(narrow_composer.right() <= px(820.));
    assert!(narrow_panel.right() <= px(820.));
    cx.update(|window, cx| window.click("toggle-echo", cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-echo").is_none());
    assert_eq!(cx.debug_bounds("agent-composer").unwrap(), narrow_composer);
    cx.update(|window, cx| window.click("toggle-echo", cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-echo").is_some());
    assert_eq!(cx.debug_bounds("agent-composer").unwrap(), narrow_composer);
    cx.simulate_resize(size(px(1600.), px(900.)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-todos-widget").is_some());
    assert_eq!(cx.debug_bounds("agent-composer").unwrap(), composer);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_agent_session(other.summary().id, window, cx)
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-echo").is_some());
    assert!(cx.debug_bounds("agent-todos-widget").is_none());
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_agent_session(current.summary().id, window, cx)
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-todos-widget").is_some());
    assert!(cx.debug_bounds("agent-jobs-widget").is_none());
    current.observe(&AgentEvent::ConversationUpdated(
        AgentConversation::default(),
    ));
    cx.update(|_, cx| view.update(cx, |view, cx| view.poll_runtime(cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-echo").is_some());
    assert!(cx.debug_bounds("agent-todos-widget").is_none());
    cx.update(|window, cx| window.click("toggle-echo", cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-echo").is_none());
    assert_eq!(cx.debug_bounds("agent-composer").unwrap(), composer);
}

#[gpui_kit::test]
fn echo_keeps_idle_sub_agents_when_their_background_report_finishes(cx: &mut TestAppContext) {
    use crate::core::agent::{collaboration::CollaboratorAccess, JobKind};
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let current = model.sessions.create("Research").unwrap();
    current
        .service
        .collaborators()
        .create(
            "notes-scout".into(),
            CollaboratorAccess::ReadOnly,
            "Review notes".into(),
        )
        .unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_agent_session(current.summary().id, window, cx)
        });
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.click("toggle-echo", cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-collaborator-0").is_some());
    let report = current
        .service
        .jobs()
        .start_background(JobKind::Collaborator, "Agent notes-scout · report 1")
        .unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.poll_runtime(cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-collaborator-0").is_some());
    assert!(cx.debug_bounds("agent-job-0").is_none());
    assert!(current.service.jobs().cancel(&report.id));
    cx.update(|_, cx| view.update(cx, |view, cx| view.poll_runtime(cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-collaborator-0").is_some());
    assert!(cx.debug_bounds("agent-jobs-widget").is_some());
    assert!(cx.debug_bounds("agent-job-0").is_none());
}
