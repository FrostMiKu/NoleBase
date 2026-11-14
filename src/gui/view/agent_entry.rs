use super::*;
use crate::gui::agent::{AgentRow, AgentRowKind};
use gpui_kit::base::SelectableText;

impl DesktopView {
    pub(super) fn agent_entry(
        &self,
        row: &AgentRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let body = match &row.kind {
            AgentRowKind::Assistant => crate::gui::rich_text::markdown(
                row.id.clone(),
                row.text.clone(),
                false,
                t,
                window,
                cx,
            ),
            AgentRowKind::Prompt => div()
                .debug_selector(|| "agent-prompt".into())
                .w_full()
                .p(px(14.))
                .rounded(px(7.))
                .bg(color(t.surface_message_user))
                .text_color(color(t.text_primary))
                .flex()
                .gap(px(10.))
                .child(div().text_color(color(t.ui_agent_user)).child("›"))
                .child(div().flex_1().min_w_0().child(SelectableText::new(
                    gpui_kit::SharedString::from(row.id.clone()),
                    row.text.clone(),
                )))
                .into_any_element(),
            AgentRowKind::Thinking { streaming } => {
                let expanded = self.expanded_thinking.contains(&row.id);
                let id = row.id.clone();
                let body: gpui_kit::SharedString = if expanded {
                    row.text.clone()
                } else {
                    let lines = row.text.lines().collect::<Vec<_>>();
                    lines[lines.len().saturating_sub(5)..].join("\n").into()
                };
                let marker = activity_marker(&row.id, *streaming, color(t.ui_activity_marker));
                div()
                    .id(gpui_kit::SharedString::from(format!("{}-thinking", row.id)))
                    .debug_selector(|| "agent-thinking-card".into())
                    .w_full()
                    .p(px(16.))
                    .rounded(px(8.))
                    .bg(color(t.surface_message_agent))
                    .border_1()
                    .border_color(color(t.ui_border_subtle).opacity(0.45))
                    .text_color(color(t.text_muted))
                    .child(
                        div()
                            .id(gpui_kit::SharedString::from(format!("{}-toggle", row.id)))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .text_size(px(12.))
                            .child(marker)
                            .child("thinking")
                            .child(div().flex_1())
                            .child(
                                icon(if expanded {
                                    "nole/chevron-down"
                                } else {
                                    "nole/chevron-right"
                                })
                                .size(px(14.)),
                            )
                            .cursor_pointer()
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if !view.expanded_thinking.remove(&id) {
                                    view.expanded_thinking.insert(id.clone());
                                }
                                if let Some(index) = view.agent.as_ref().and_then(|agent| {
                                    agent.rows.iter().position(|row| row.id == id)
                                }) {
                                    view.agent_list.splice(index..index + 1, 1);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .debug_selector(|| "agent-thinking-body".into())
                            .mt(px(12.))
                            .text_size(px(13.))
                            .line_height(gpui_kit::relative(1.6))
                            .when(!expanded, |body| body.max_h(px(110.)).overflow_hidden())
                            .child(SelectableText::new(
                                gpui_kit::SharedString::from(format!("{}-body", row.id)),
                                body,
                            )),
                    )
                    .into_any_element()
            }
            AgentRowKind::Tool {
                active,
                details,
                preview,
            } => div()
                .debug_selector(|| "agent-tool".into())
                .w_full()
                .child(
                    div()
                        .debug_selector(|| "agent-tool-status".into())
                        .w_full()
                        .min_h(px(26.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(13.))
                        .text_color(color(t.ui_activity_marker))
                        .child(activity_marker(
                            &row.id,
                            *active,
                            color(t.ui_activity_marker),
                        ))
                        .child(SelectableText::new(
                            gpui_kit::SharedString::from(format!("{}-status", row.id)),
                            row.text.clone(),
                        )),
                )
                .when(!details.is_empty() || preview.is_some(), |tool| {
                    tool.child(
                        div()
                            .debug_selector(|| "agent-tool-result".into())
                            .ml(px(7.))
                            .pl(px(14.))
                            .py(px(6.))
                            .border_l_1()
                            .border_color(color(t.ui_border_subtle))
                            .text_size(px(12.))
                            .line_height(gpui_kit::relative(1.5))
                            .text_color(color(t.text_muted))
                            .when(!details.is_empty(), |body| {
                                body.child(SelectableText::new(
                                    gpui_kit::SharedString::from(format!("{}-details", row.id)),
                                    details.clone(),
                                ))
                            })
                            .when_some(preview.clone(), |body, preview| {
                                body.child(div().child(SelectableText::new(
                                    gpui_kit::SharedString::from(format!("{}-preview", row.id)),
                                    preview,
                                )))
                            }),
                    )
                })
                .into_any_element(),
            AgentRowKind::Error => div()
                .debug_selector(|| "agent-error".into())
                .text_color(color(t.ui_error))
                .text_size(px(13.))
                .child(SelectableText::new(
                    gpui_kit::SharedString::from(row.id.clone()),
                    row.text.clone(),
                ))
                .into_any_element(),
        };
        div()
            .w_full()
            .pb(px(12.))
            .text_size(px(15.))
            .line_height(gpui_kit::relative(1.6))
            .child(body)
    }
}

fn activity_marker(id: &str, active: bool, color: gpui_kit::Hsla) -> gpui_kit::AnyElement {
    if active {
        icon("nole/spinner")
            .size(px(14.))
            .text_color(color)
            .with_animation(
                gpui_kit::SharedString::from(format!("{id}-spinner")),
                Animation::new(Duration::from_millis(1200)).repeat(),
                |icon, delta| icon.rotate(gpui_kit::radians(delta * std::f32::consts::TAU)),
            )
            .into_any_element()
    } else {
        icon("nole/check")
            .size(px(14.))
            .text_color(color)
            .into_any_element()
    }
}
