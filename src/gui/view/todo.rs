use super::*;

impl DesktopView {
    pub(super) fn todo_surface(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let view = cx.entity().downgrade();
        let tasks =
            gpui_kit::uniform_list("todo-list", self.model.todos.len(), move |range, _, cx| {
                view.update(cx, |view, cx| {
                    range
                        .map(|index| {
                            let task = &view.model.todos[index];
                            div().w_full().h(px(56.)).pb(px(4.)).child(
                                div()
                                    .id(("todo", index))
                                    .when(index == 0, |row| {
                                        row.debug_selector(|| "first-todo".into())
                                    })
                                    .w_full()
                                    .h(px(52.))
                                    .px(px(10.))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .rounded(px(5.))
                                    .cursor_pointer()
                                    .hover(move |row| {
                                        row.bg(color(t.selection_background_inactive))
                                    })
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
        div()
            .debug_selector(|| "todo-page".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(self.model.todos.is_empty(), |panel| {
                panel.child(
                    div()
                        .pt(px(LIST_TOP))
                        .text_size(px(12.))
                        .text_color(color(t.text_muted))
                        .child("日常记录中的待办会显示在这里。"),
                )
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
    }
    fn toggle_task(&mut self, index: usize, cx: &mut Context<Self>) {
        match self
            .model
            .workspace
            .toggle_todo_task(index)
            .and_then(|_| self.model.refresh())
        {
            Ok(()) => self.sync_daily_list(),
            Err(error) => self.status = format!("更新 Todo 遇到问题：{error}"),
        }
        cx.notify();
    }
}
