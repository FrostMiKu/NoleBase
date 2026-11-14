use super::surfaces::reading_row;
use super::*;
use crate::core::agent::{ApprovalDecision, AskUserResponse, PrivateTerminalInputDecision};
use crate::gui::presentation::{RADIUS_LG, RADIUS_MD, SPACE_MD, SPACE_SM, TEXT_META, TEXT_UI};

impl DesktopView {
    #[cfg(feature = "gui-test")]
    pub(in crate::gui) fn review_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::core::agent::{ApprovalKind, ApprovalRequest, CommandApproval};
        use crate::core::agent_session::AgentPanelEntry;
        self.open_page(Page::Agent, window, cx);
        let agent = self.agent.as_mut().expect("review Agent session");
        agent
            .panel
            .push_prompt("查看工作区文件，并帮我整理阅读笔记。", false);
        agent.panel.push_entry(AgentPanelEntry::Assistant {
            text: "我会先查看工作区中的文件，再根据内容整理笔记。".into(),
            streaming: false,
            final_output: false,
        });
        agent.panel.push_entry(AgentPanelEntry::Tool {
            text: "Calling Shell...\nls -la workspace".into(),
            active: true,
            preview: None,
        });
        agent.rebuild_rows();
        self.agent_list.reset(agent.rows.len());
        agent.approval = Some(ApprovalRequest {
            title: "运行命令".into(),
            message: String::new(),
            kind: ApprovalKind::Command(CommandApproval {
                purpose: "查看工作区文件，确认需要整理的笔记。".into(),
                label: "Shell".into(),
                code: "ls -la workspace".into(),
            }),
        });
        cx.notify();
    }

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
                self.refresh_sessions();
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
        let Some(agent) = &self.agent else {
            return div().id("agent-unavailable").child(self.empty_state(
                "agent-unavailable-state",
                "nole/agent",
                "Agent 暂时无法打开",
                "检查 config/ai.toml 中的模型配置后重试。",
            ));
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
            .relative()
            .flex()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .when(agent.rows.is_empty(), |body| {
                                body.child(self.empty_state(
                                    "agent-empty",
                                    "nole/agent",
                                    "开始与 Agent 对话",
                                    "描述你的目标，Agent 会结合知识库查找资料、整理笔记或完成任务。用 [[笔记名]] 引用笔记。",
                                ))
                            })
                            .relative()
                            .child(transcript)
                            .child(self.main_scrollbar(&self.agent_list, 0.)),
                    )
                    .child(
                        div()
                            .id("agent-interaction-scroll")
                            .when(agent.question.is_none(), |panel| {
                                panel.max_h(px(300.)).overflow_y_scroll()
                            })
                            .flex_shrink_0()
                            .child(reading_row(interactions)),
                    )
                    .child(
                        reading_row(
                            div().pt(px(SPACE_MD)).child(
                                self.composer(
                                    "agent-composer",
                                    super::persistence::Compose::Agent,
                                    86.,
                                    "Enter 发送 · Shift+Enter 换行",
                                    if running {
                                        self.button("cancel-agent", "停止")
                                            .icon(icon("nole/close").size(px(14.)))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if let Some(agent) = &this.agent {
                                                    agent.current.session.cancel();
                                                }
                                                cx.notify();
                                            }))
                                    } else {
                                        self.primary_button("send-agent", "发送")
                                            .child(icon("nole/arrow").size(px(14.)))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.send_agent(window, cx)
                                            }))
                                    },
                                    cx,
                                ),
                            ),
                        )
                        .flex_shrink_0(),
                    ),
            )
            .when(self.echo_open, |surface| {
                surface.child(
                    div().absolute().top_0().right_0()
                        .w(px(280.0_f32.min((self.content_width(window) - PAGE_INSET * 2.).max(0.))))
                        .child(self.agent_echo(window))
                )
            })
    }

    fn agent_interactions(&self, _window: &mut Window, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let Some(agent) = &self.agent else {
            return div();
        };
        let card = || {
            div()
                .w_full()
                .min_w_0()
                .p(px(SPACE_MD + 2.))
                .rounded(px(RADIUS_LG))
                .border_1()
                .border_color(color(t.ui_action_ai).opacity(0.35))
                .bg(color(t.surface_raised))
                .flex()
                .flex_col()
                .gap(px(SPACE_SM + 2.))
        };
        let heading = |label: &'static str, text: String| {
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_start()
                .gap(px(SPACE_SM))
                .text_size(px(TEXT_UI))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(TEXT_META))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(color(t.ui_action_ai))
                        .child(label),
                )
                .child(
                    div()
                        .debug_selector(|| "agent-interaction-heading".into())
                        .flex_1()
                        .min_w_0()
                        .whitespace_normal()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(text),
                )
        };
        let mut panel = div()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(SPACE_SM + 2.));
        if let Some(approval) = &agent.approval {
            panel = panel.child(self.agent_approval(approval, cx));
        } else if let Some(question) = &agent.question {
            let mut body = card()
                .debug_selector(|| "agent-question".into())
                .child(heading("需要你的选择", question.question.clone()));
            let hover = self.row_hover();
            for (index, option) in question.options.iter().enumerate() {
                let answer = option.clone();
                body = body.child(
                    div()
                        .id(("agent-option", index))
                        .debug_selector(move || format!("agent-option-{index}"))
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .px(px(SPACE_MD))
                        .py(px(SPACE_SM))
                        .rounded(px(RADIUS_MD))
                        .border_1()
                        .border_color(color(t.ui_border_subtle))
                        .cursor_pointer()
                        .hover(move |row| row.bg(hover))
                        .child(option.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.agent_answer(AskUserResponse::Answer(answer.clone()), cx)
                        })),
                );
            }
            panel = panel.child(
                body.child(Input::new(&self.answer_input)).child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(SPACE_SM))
                        .child(self.button("cancel-answer", "取消").on_click(cx.listener(
                            |this, _, _, cx| this.agent_answer(AskUserResponse::Cancelled, cx),
                        )))
                        .child(self.primary_button("answer-agent", "提交回答").on_click(
                            cx.listener(|this, _, window, cx| {
                                let answer = this.answer_input.read(cx).value();
                                this.agent_answer(AskUserResponse::Answer(answer.to_string()), cx);
                                this.answer_input
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                            }),
                        )),
                ),
            );
        } else if let Some(request) = &agent.private_input {
            panel = panel.child(
                card()
                    .child(heading("需要私密输入", request.purpose.clone()))
                    .child(
                        div()
                            .text_size(px(TEXT_META))
                            .text_color(color(t.text_muted))
                            .child(request.prompt.clone()),
                    )
                    .child(Input::new(&self.private_input))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(SPACE_SM))
                            .child(self.button("cancel-private", "取消").on_click(cx.listener(
                                |this, _, window, cx| this.submit_private(true, window, cx),
                            )))
                            .child(self.primary_button("submit-private", "提交").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.submit_private(false, window, cx)
                                }),
                            )),
                    ),
            );
        }
        panel
    }

    pub(super) fn agent_decision(&mut self, decision: ApprovalDecision, cx: &mut Context<Self>) {
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
