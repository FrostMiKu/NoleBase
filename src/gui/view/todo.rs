use super::*;
use crate::gui::presentation::{RADIUS_MD, SPACE_MD, SPACE_XS, TEXT_META};

impl DesktopView {
    pub(super) fn todo_surface(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let view = cx.entity().downgrade();
        let hover = self.row_hover();
        let tasks =
            gpui_kit::uniform_list("todo-list", self.model.todos.len(), move |range, _, cx| {
                view.update(cx, |view, cx| {
                    range
                        .map(|index| {
                            let task = &view.model.todos[index];
                            div().w_full().h(px(48.)).pb(px(SPACE_XS)).child(
                                div()
                                    .id(("todo", index))
                                    .when(index == 0, |row| {
                                        row.debug_selector(|| "first-todo".into())
                                    })
                                    .size_full()
                                    .px(px(SPACE_MD))
                                    .flex()
                                    .items_center()
                                    .rounded(px(RADIUS_MD))
                                    .cursor_pointer()
                                    .hover(move |row| row.bg(hover))
                                    .when(task.checked, |row| row.text_color(color(t.text_muted)))
                                    .child(
                                        gpui_kit::component::checkbox::Checkbox::new((
                                            "todo-check",
                                            index,
                                        ))
                                        .checked(task.checked)
                                        .label(task.text.clone())
                                        .w_full()
                                        .on_change(
                                            cx.listener(move |this, _, _, cx| {
                                                this.toggle_task(index, cx)
                                            }),
                                        ),
                                    ),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
            })
            .track_scroll(&self.todo_scroll)
            .size_full();
        let open = self.model.todos.iter().filter(|todo| !todo.checked).count();
        let done = self.model.todos.len() - open;
        div()
            .debug_selector(|| "todo-page".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(self.model.todos.is_empty(), |panel| {
                panel.child(self.empty_state(
                    "todo-empty",
                    "nole/todo",
                    "没有待办",
                    "在日常记录中写下 - [ ] 任务，它会出现在这里，勾选后同步回原文。",
                ))
            })
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .pt(px(LIST_TOP))
                    .child(tasks)
                    .child(self.main_scrollbar(&self.todo_scroll, LIST_TOP)),
            )
            .when(!self.model.todos.is_empty(), |panel| {
                panel.child(
                    div()
                        .flex_shrink_0()
                        .pt(px(SPACE_MD))
                        .text_size(px(TEXT_META))
                        .text_color(color(t.text_muted))
                        .child(format!("未完成 {open} · 已完成 {done}")),
                )
            })
    }

    fn toggle_task(&mut self, index: usize, cx: &mut Context<Self>) {
        match self
            .model
            .workspace
            .toggle_todo_task(index)
            .and_then(|_| self.model.refresh())
        {
            Ok(()) => self.sync_daily_list(),
            Err(error) => self.status = format!("更新待办遇到问题：{error}"),
        }
        cx.notify();
    }
}
