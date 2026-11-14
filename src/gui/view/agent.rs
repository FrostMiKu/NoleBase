use super::*;
use crate::core::agent::{ApprovalDecision, AskUserResponse, PrivateTerminalInputDecision};

impl DesktopView {
    pub(super) fn send_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(agent) = &mut self.agent else {
            return;
        };
        let prompt = self.agent_input.read(cx).value();
        match agent.send(prompt.to_string()) {
            Ok(()) => {
                self.agent_input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.agent_list.reset(agent.rows.len());
                self.rebuild_tree();
                self.status.clear();
            }
            Err(error) => self.status = format!("发送遇到问题：{error}"),
        }
        cx.notify();
    }

    pub(super) fn agent_surface(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let t = self.model.theme.theme;
        let Some(agent) = &self.agent else {
            return div().id("agent-unavailable");
        };
        let running = agent.current.session.is_running();
        let view = cx.entity().downgrade();
        let transcript = gpui_kit::list(self.agent_list.clone(), move |index, window, cx| {
            view.update(cx, |view, cx| {
                let Some(agent) = &view.agent else {
                    return div().into_any_element();
                };
                let Some(row) = agent.rows.get(index) else {
                    return div().into_any_element();
                };
                view.agent_entry(row, window, cx).into_any_element()
            })
            .unwrap_or_else(|_| div().into_any_element())
        })
        .size_full();
        let interactions = self.agent_interactions(window, cx);
        div()
            .id("agent-surface")
            .debug_selector(|| "agent-surface".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .when(agent.rows.is_empty(), |body| {
                                body.child(
                                    div()
                                        .py(px(LIST_TOP))
                                        .text_color(color(t.text_muted))
                                        .child("描述你的目标，Agent 会结合知识库协助完成。"),
                                )
                            })
                            .relative()
                            .child(transcript)
                            .child(Scrollbar::vertical(&self.agent_list)),
                    )
                    .child(
                        div()
                            .id("agent-interaction-scroll")
                            .max_h(px(300.))
                            .overflow_y_scroll()
                            .flex_shrink_0()
                            .child(interactions),
                    )
                    .child(
                        div().w_full().pt(px(12.)).flex_shrink_0().child(
                            div()
                                .border_1()
                                .border_color(color(t.ui_border_subtle))
                                .rounded(px(10.))
                                .p(px(12.))
                                .child(self.compose_input(
                                    super::persistence::Compose::Agent,
                                    86.,
                                    cx,
                                ))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .mt(px(6.))
                                        .child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(color(t.text_muted))
                                                .child("Enter 发送 · Shift+Enter 换行"),
                                        )
                                        .child(if running {
                                            self.button("cancel-agent", "停止").on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    if let Some(agent) = &this.agent {
                                                        agent.current.session.cancel();
                                                    }
                                                    cx.notify();
                                                }),
                                            )
                                        } else {
                                            self.button("send-agent", "发送")
                                                .bg(color(t.ui_action))
                                                .text_color(color(t.text_on_accent))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.send_agent(window, cx)
                                                }))
                                        }),
                                ),
                        ),
                    ),
            )
    }

    fn agent_interactions(&self, _window: &mut Window, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let Some(agent) = &self.agent else {
            return div();
        };
        let mut panel = div().flex_shrink_0().flex().flex_col().gap(px(10.));
        if let Some(approval) = &agent.approval {
            let mut details = div()
                .id("approval-details")
                .h(px(180.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(10.))
                .text_size(px(12.))
                .child(
                    div()
                        .font_family("monospace")
                        .child(approval.message.clone()),
                );
            if let crate::core::agent::ApprovalKind::Command(command) = &approval.kind {
                details = details
                    .child(format!("{} · {}", command.label, command.purpose))
                    .child(
                        div()
                            .debug_selector(|| "approval-command".into())
                            .p(px(12.))
                            .bg(color(t.surface_panel))
                            .font_family("monospace")
                            .child(command.code.clone()),
                    );
            }
            panel = panel
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(color(t.ui_warning))
                        .child(approval.title.clone()),
                )
                .child(details)
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(self.button("approve-agent", "允许").on_click(cx.listener(
                            |this, _, _, cx| this.agent_decision(ApprovalDecision::Approve, cx),
                        )))
                        .child(self.button("deny-agent", "拒绝").on_click(cx.listener(
                            |this, _, _, cx| this.agent_decision(ApprovalDecision::Deny, cx),
                        ))),
                );
        } else if let Some(question) = &agent.question {
            panel = panel.child(div().text_size(px(13.)).child(question.question.clone()));
            for (index, option) in question.options.iter().enumerate() {
                let answer = option.clone();
                panel = panel.child(
                    div()
                        .id(("agent-option", index))
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_size(px(13.))
                        .hover(move |row| row.bg(color(t.surface_panel)))
                        .child(option.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.agent_answer(AskUserResponse::Answer(answer.clone()), cx)
                        })),
                );
            }
            panel = panel.child(Input::new(&self.answer_input)).child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        self.button("answer-agent", "提交回答")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let answer = this.answer_input.read(cx).value();
                                this.agent_answer(AskUserResponse::Answer(answer.to_string()), cx);
                                this.answer_input
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                            })),
                    )
                    .child(self.button("cancel-answer", "取消").on_click(cx.listener(
                        |this, _, _, cx| this.agent_answer(AskUserResponse::Cancelled, cx),
                    ))),
            );
        } else if let Some(request) = &agent.private_input {
            panel = panel
                .child(div().text_size(px(13.)).child(request.purpose.clone()))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(color(t.text_muted))
                        .child(request.prompt.clone()),
                )
                .child(Input::new(&self.private_input))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(self.button("submit-private", "提交").on_click(cx.listener(
                            |this, _, window, cx| this.submit_private(false, window, cx),
                        )))
                        .child(self.button("cancel-private", "取消").on_click(cx.listener(
                            |this, _, window, cx| this.submit_private(true, window, cx),
                        ))),
                );
        }
        panel
    }

    fn agent_decision(&mut self, decision: ApprovalDecision, cx: &mut Context<Self>) {
        if let Some(agent) = &mut self.agent {
            if let Err(error) = agent.approve(decision) {
                self.status = error.to_string();
            }
        }
        cx.notify();
    }
    fn agent_answer(&mut self, answer: AskUserResponse, cx: &mut Context<Self>) {
        if let Some(agent) = &mut self.agent {
            if let Err(error) = agent.answer(answer) {
                self.status = error.to_string();
            }
        }
        cx.notify();
    }
    fn submit_private(&mut self, cancel: bool, window: &mut Window, cx: &mut Context<Self>) {
        let answer = if cancel {
            PrivateTerminalInputDecision::Cancelled
        } else {
            PrivateTerminalInputDecision::Submit(zeroize::Zeroizing::new(
                self.private_input.read(cx).value().to_string(),
            ))
        };
        self.private_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        if let Some(agent) = &mut self.agent {
            if let Err(error) = agent.private_answer(answer) {
                self.status = error.to_string();
            }
        }
        cx.notify();
    }
}
