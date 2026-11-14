use super::widgets::spinning_icon;
use super::*;
use crate::core::agent::collaboration::CollaboratorStatus;
use crate::core::agent::{JobKind, JobStatus};
use crate::core::agent_session::TodoStatus;
use crate::gui::presentation::{RADIUS_LG, SPACE_LG, SPACE_SM, TEXT_LABEL, TEXT_META};

impl DesktopView {
    pub(super) fn agent_echo(&self, window: &Window) -> gpui_kit::AnyElement {
        let Some(agent) = &self.agent else {
            return div().into_any_element();
        };
        let jobs = agent
            .current
            .service
            .jobs()
            .rows()
            .into_iter()
            .filter(|job| job.status == JobStatus::Running && job.kind != JobKind::Collaborator)
            .collect::<Vec<_>>();
        let collaborators = agent.current.service.collaborators().rows();
        let task_count = jobs.len() + collaborators.len();
        let todos = agent
            .current
            .session
            .with_conversation(|conversation| conversation.todos.clone());
        let empty = task_count == 0 && todos.is_empty();
        let t = self.model.theme.theme;
        let card = || {
            div()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap(px(SPACE_SM))
        };
        let heading = |label: String| {
            div()
                .text_size(px(TEXT_LABEL))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(color(t.text_muted))
                .child(label)
        };
        div()
            .id("agent-echo")
            .debug_selector(|| "agent-echo".into())
            .w_full()
            .min_w_0()
            .p(px(SPACE_LG))
            .rounded(px(RADIUS_LG))
            .border_1()
            .border_color(color(t.ui_border_subtle))
            .bg(color(t.surface_overlay))
            .shadow_lg()
            .max_h(px(f32::from(window.viewport_size().height) * 0.4))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(SPACE_LG))
            .text_size(px(TEXT_META))
            .when(empty, |panel| {
                panel.child(div().text_color(color(t.text_muted)).child("暂无任务"))
            })
            .when(task_count > 0, |aside| {
                aside.child(
                    card()
                        .debug_selector(|| "agent-jobs-widget".into())
                        .child(heading(format!("后台任务 · {task_count}")))
                        .children(jobs.into_iter().enumerate().map(|(index, job)| {
                            div()
                                .debug_selector(move || format!("agent-job-{index}"))
                                .flex()
                                .items_start()
                                .gap(px(SPACE_SM))
                                .child(spinning_icon(
                                    format!("{}-widget-spinner", job.id),
                                    color(t.ui_action_ai),
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .text_color(color(t.text_secondary))
                                        .child(job.label),
                                )
                        }))
                        .children(collaborators.into_iter().enumerate().map(
                            |(index, collaborator)| {
                                let running = collaborator.status.is_active();
                                let tone = if running {
                                    t.ui_action_ai
                                } else {
                                    t.text_muted
                                };
                                div()
                                    .debug_selector(move || format!("agent-collaborator-{index}"))
                                    .flex()
                                    .items_start()
                                    .gap(px(SPACE_SM))
                                    .child(if running {
                                        spinning_icon(
                                            format!("{}-widget-spinner", collaborator.id),
                                            color(tone),
                                        )
                                    } else {
                                        icon(
                                            if matches!(
                                                collaborator.status,
                                                CollaboratorStatus::Failed
                                                    | CollaboratorStatus::Interrupted
                                            ) {
                                                "nole/alert"
                                            } else {
                                                "nole/agent"
                                            },
                                        )
                                        .size(px(14.))
                                        .text_color(color(tone))
                                        .into_any_element()
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .whitespace_normal()
                                            .text_color(color(t.text_secondary))
                                            .child(collaborator.name),
                                    )
                            },
                        )),
                )
            })
            .when(!todos.is_empty(), |aside| {
                let completed = todos
                    .iter()
                    .filter(|todo| todo.status == TodoStatus::Completed)
                    .count();
                aside.child(
                    card()
                        .debug_selector(|| "agent-todos-widget".into())
                        .child(heading(format!("Todo · {completed}/{}", todos.len())))
                        .children(todos.into_iter().enumerate().map(|(index, todo)| {
                            let (glyph, tone) = match todo.status {
                                TodoStatus::Pending => ("nole/todo", t.text_muted),
                                TodoStatus::InProgress => ("nole/spinner", t.ui_action_ai),
                                TodoStatus::Completed => ("nole/check", t.ui_success),
                            };
                            div()
                                .debug_selector(move || format!("agent-todo-{index}"))
                                .flex()
                                .items_start()
                                .gap(px(SPACE_SM))
                                .child(if todo.status == TodoStatus::InProgress {
                                    spinning_icon(
                                        format!("todo-{index}-widget-spinner"),
                                        color(tone),
                                    )
                                } else {
                                    icon(glyph)
                                        .size(px(14.))
                                        .text_color(color(tone))
                                        .into_any_element()
                                })
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .text_color(color(
                                            if todo.status == TodoStatus::Completed {
                                                t.text_muted
                                            } else {
                                                t.text_secondary
                                            },
                                        ))
                                        .child(todo.content),
                                )
                        })),
                )
            })
            .into_any_element()
    }
}
