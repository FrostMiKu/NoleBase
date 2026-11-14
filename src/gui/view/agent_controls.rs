use super::*;
use crate::core::agent::{ApprovalDecision, PermissionMode};
use gpui_kit::component::{
    dialog::{Dialog, DialogEntrance},
    menu::{DropdownMenu, PopupMenu, PopupMenuItem},
    WindowExt,
};

#[derive(Clone, Copy)]
pub(super) enum SessionAction {
    Rename(uuid::Uuid),
    Delete(uuid::Uuid),
}

impl DesktopView {
    pub(super) fn session_context_menu(
        &self,
        id: uuid::Uuid,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = cx.entity().downgrade();
        move |menu, _, _| {
            let rename = view.clone();
            let delete = view.clone();
            menu.item(PopupMenuItem::new("重命名").on_click(move |_, window, cx| {
                let _ = rename.update(cx, |view, cx| {
                    view.begin_session_action(SessionAction::Rename(id), window, cx);
                });
            }))
            .separator()
            .item(PopupMenuItem::new("删除").on_click(move |_, window, cx| {
                let _ = delete.update(cx, |view, cx| {
                    view.begin_session_action(SessionAction::Delete(id), window, cx);
                });
            }))
        }
    }

    fn begin_session_action(
        &mut self,
        action: SessionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SessionAction::Rename(id) = action {
            let Some(session) = self.sessions.iter().find(|session| session.id == id) else {
                return;
            };
            self.session_action_name.update(cx, |input, cx| {
                input.set_value(session.title.clone(), window, cx)
            });
        }
        self.session_action = Some(action);
        self.status.clear();
        let view = cx.entity().downgrade();
        let initial_focus = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            view.update(cx, |view, cx| {
                if initial_focus.replace(false)
                    && matches!(view.session_action, Some(SessionAction::Rename(_)))
                {
                    view.session_action_name
                        .update(cx, |input, cx| input.focus(window, cx));
                }
                view.session_action_dialog(dialog, cx)
            })
            .unwrap_or_else(|_| Dialog::new(cx))
        });
        cx.notify();
    }

    fn session_action_dialog(&self, dialog: Dialog, cx: &mut Context<Self>) -> Dialog {
        let rename = matches!(self.session_action, Some(SessionAction::Rename(_)));
        dialog
            .title(if rename {
                "重命名会话"
            } else {
                "删除会话"
            })
            .centered()
            .entrance(DialogEntrance::FadeSlide)
            .w(px(420.))
            .on_close(cx.listener(|view, _, _, cx| {
                view.session_action = None;
                cx.notify();
            }))
            .when(rename, |dialog| {
                dialog.child(Input::new(&self.session_action_name).aria_label("会话名称"))
            })
            .when(!rename, |dialog| {
                let title = self
                    .session_action
                    .and_then(|action| match action {
                        SessionAction::Delete(id) => {
                            self.sessions.iter().find(|session| session.id == id)
                        }
                        _ => None,
                    })
                    .map(|session| session.title.as_str())
                    .unwrap_or("会话");
                dialog.child(format!(
                    "将永久删除「{title}」及其对话记录，并停止该会话的任务。"
                ))
            })
            .when(!self.status.is_empty(), |dialog| {
                dialog.child(
                    div()
                        .text_color(color(self.model.theme.theme.ui_error))
                        .child(self.status.clone()),
                )
            })
            .footer(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        self.button("cancel-session-action", "取消")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.session_action = None;
                                window.close_dialog(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        self.primary_button(
                            "confirm-session-action",
                            if rename { "重命名" } else { "删除" },
                        )
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.confirm_session_action(window, cx)
                        })),
                    ),
            )
    }

    pub(super) fn confirm_session_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.session_action else {
            return;
        };
        let result = (|| -> anyhow::Result<()> {
            match action {
                SessionAction::Rename(id) => {
                    self.model
                        .sessions
                        .get(id)?
                        .rename(&self.session_action_name.read(cx).value())?;
                }
                SessionAction::Delete(id) => {
                    self.model.sessions.delete(id)?;
                    if self
                        .agent
                        .as_ref()
                        .is_some_and(|agent| agent.current.summary().id == id)
                    {
                        if let Some(next) = self.model.sessions.list().first() {
                            self.open_agent_session(next.id, window, cx);
                        } else {
                            self.new_agent_session(window, cx);
                        }
                    }
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.refresh_sessions();
                self.status.clear();
                self.session_action = None;
                window.close_dialog(cx);
            }
            Err(error) => self.status = format!("操作遇到问题：{error}"),
        }
        cx.notify();
    }

    pub(super) fn agent_permission_control(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self
            .agent
            .as_ref()
            .map(|agent| agent.current.service.permission_mode())
            .unwrap_or(PermissionMode::Approve);
        let view = cx.entity().downgrade();
        let muted = color(self.model.theme.theme.text_muted);
        Button::new("agent-permission")
            .ghost()
            .small()
            .debug_selector(|| "agent-permission".into())
            .accessibility_label(mode.label())
            .child(div().text_color(muted).child(mode.label()))
            .child(icon("nole/chevron-down").size(px(12.)).text_color(muted))
            .dropdown_menu_with_anchor(gpui_kit::Anchor::BottomLeft, move |mut menu, _, _| {
                for choice in [
                    PermissionMode::Approve,
                    PermissionMode::Auto,
                    PermissionMode::Yolo,
                ] {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(choice.label())
                            .checked(mode == choice)
                            .on_click(move |_, _, cx| {
                                let _ = view
                                    .update(cx, |view, cx| view.set_agent_permission(choice, cx));
                            }),
                    );
                }
                menu
            })
    }

    fn set_agent_permission(&mut self, mode: PermissionMode, cx: &mut Context<Self>) {
        let Some(agent) = &mut self.agent else {
            return;
        };
        let previous = agent.current.service.permission_mode();
        agent.current.service.set_permission_mode(mode);
        if let Err(error) = agent.current.persist() {
            agent.current.service.set_permission_mode(previous);
            self.status = format!("保存权限遇到问题：{error}");
        } else {
            self.status.clear();
            if mode == PermissionMode::Yolo && agent.approval.is_some() {
                if let Err(error) = agent.approve(ApprovalDecision::Approve) {
                    self.status = format!("审批遇到问题：{error}");
                }
            }
        }
        cx.notify();
    }
}
