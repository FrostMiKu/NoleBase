use super::surfaces::reading_row;
use super::*;
use crate::gui::agent::{AgentRow, AgentRowKind};
use crate::gui::presentation::{
    BODY_SIZE, RADIUS_LG, RADIUS_MD, SPACE_LG, SPACE_MD, SPACE_SM, TEXT_META, TEXT_NAV,
};
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
                .w_full()
                .flex()
                .justify_end()
                .child(
                    div()
                        .debug_selector(|| "agent-prompt".into())
                        .max_w(gpui_kit::relative(0.85))
                        .px(px(SPACE_LG))
                        .py(px(SPACE_MD))
                        .rounded(px(RADIUS_LG))
                        .bg(color(t.surface_message_user))
                        .text_color(color(t.text_primary))
                        .child(SelectableText::new(
                            gpui_kit::SharedString::from(row.id.clone()),
                            row.text.clone(),
                        )),
                )
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
                let marker = activity_marker(&row.id, *streaming, color(t.ui_action_ai));
                div()
                    .id(gpui_kit::SharedString::from(format!("{}-thinking", row.id)))
                    .debug_selector(|| "agent-thinking-card".into())
                    .w_full()
                    .px(px(SPACE_LG))
                    .py(px(SPACE_MD))
                    .rounded(px(RADIUS_LG))
                    .bg(color(t.surface_sunken))
                    .text_color(color(t.text_muted))
                    .child(
                        div()
                            .id(gpui_kit::SharedString::from(format!("{}-toggle", row.id)))
                            .flex()
                            .items_center()
                            .gap(px(SPACE_SM))
                            .text_size(px(TEXT_META))
                            .child(marker)
                            .child(if *streaming {
                                "正在思考"
                            } else {
                                "思考过程"
                            })
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
                            .mt(px(SPACE_SM + 2.))
                            .text_size(px(TEXT_NAV))
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
                        .gap(px(SPACE_SM))
                        .text_size(px(TEXT_NAV))
                        .text_color(color(t.text_secondary))
                        .child(activity_marker(
                            &row.id,
                            *active,
                            color(if *active {
                                t.ui_action_ai
                            } else {
                                t.ui_success
                            }),
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
                            .border_color(color(t.ui_border))
                            .text_size(px(TEXT_META))
                            .line_height(gpui_kit::relative(1.5))
                            .text_color(color(t.text_muted))
                            .font_family(crate::gui::presentation::fonts(cx).mono)
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
            AgentRowKind::Error => {
                let error = color(t.ui_error);
                div()
                    .w_full()
                    .px(px(SPACE_MD))
                    .py(px(SPACE_SM + 2.))
                    .rounded(px(RADIUS_MD))
                    .border_1()
                    .border_color(error.opacity(0.35))
                    .bg(error.opacity(0.08))
                    .flex()
                    .gap(px(SPACE_SM))
                    .text_color(error)
                    .text_size(px(TEXT_NAV))
                    .child(div().pt(px(2.)).child(icon("nole/alert").size(px(16.))))
                    .child(
                        div()
                            .debug_selector(|| "agent-error".into())
                            .flex_1()
                            .min_w_0()
                            .child(SelectableText::new(
                                gpui_kit::SharedString::from(row.id.clone()),
                                row.text.clone(),
                            )),
                    )
                    .into_any_element()
            }
        };
        reading_row(
            div()
                .w_full()
                .pb(px(SPACE_LG))
                .text_size(px(BODY_SIZE))
                .line_height(gpui_kit::relative(1.7))
                .child(body),
        )
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
