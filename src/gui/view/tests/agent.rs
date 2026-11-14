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
