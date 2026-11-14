use super::*;
use gpui_kit::component::{
    checkbox::Checkbox,
    dialog::{Dialog, DialogEntrance},
    WindowExt,
};

impl DesktopView {
    pub(super) fn home_indices(&self) -> &[usize] {
        match self.home.collection {
            Collection::Notes => &self.model.notes,
            Collection::Archives => &self.model.archives,
        }
    }

    pub(super) fn home_toolbar(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let mut bar = div()
            .h(px(30.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(6.));
        if self.home.managing {
            bar = bar
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(color(t.text_muted))
                        .child(format!("已选 {} 篇", self.home.selected.len())),
                )
                .child(
                    self.button("select-all-notes", "全选")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let paths = this
                                .home_indices()
                                .iter()
                                .map(|index| this.model.documents[*index].path.clone())
                                .collect();
                            this.home.selected = paths;
                            cx.notify();
                        })),
                )
                .child(
                    self.button(
                        "archive-selection",
                        if self.home.collection == Collection::Notes {
                            "归档"
                        } else {
                            "恢复"
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.move_home_selection(cx))),
                )
                .child(
                    self.button("finish-selection", "完成")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.home.finish_selection();
                            cx.notify();
                        })),
                );
        } else {
            bar = bar
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(color(t.text_muted))
                        .mr(px(8.))
                        .child(format!("{} 篇", self.home_indices().len())),
                )
                .child(self.button("manage-notes", "多选").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.home.managing = true;
                        cx.notify();
                    },
                )));
        }
        bar
    }

    pub(super) fn click_note(
        &mut self,
        path: &str,
        modifiers: gpui_kit::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.home.managing || modifiers.secondary(cx) || modifiers.shift {
            let paths: Vec<_> = self
                .home_indices()
                .iter()
                .map(|index| self.model.documents[*index].path.clone())
                .collect();
            if !paths.iter().any(|visible| visible == path) {
                return;
            }
            self.home.toggle(path, &paths, modifiers.shift);
            cx.notify();
        } else {
            self.navigate(Some(path.to_owned()), window, cx);
        }
    }

    fn move_home_selection(&mut self, cx: &mut Context<Self>) {
        let mut selected: Vec<_> = self.home.selected.iter().cloned().collect();
        selected.sort();
        let mut moved = 0;
        let mut errors = Vec::new();
        for relative in selected {
            let result = self
                .model
                .workspace
                .document_path(&relative)
                .and_then(|path| {
                    if self.home.collection == Collection::Notes {
                        self.model.workspace.archive_note(&path)
                    } else {
                        self.model.workspace.restore_note(&path)
                    }
                });
            match result {
                Ok(_) => {
                    self.home.selected.remove(&relative);
                    moved += 1;
                }
                Err(error) => errors.push(format!("{relative}：{error}")),
            }
        }
        if let Err(error) = self.model.refresh() {
            errors.push(error.to_string());
        }
        self.sync_daily_list();
        let action = if self.home.collection == Collection::Notes {
            "归档"
        } else {
            "恢复"
        };
        self.status = if errors.is_empty() {
            String::new()
        } else {
            format!("已{action} {moved} 篇笔记。{}", errors.join("；"))
        };
        cx.notify();
    }

    pub(super) fn begin_new_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.flush_save(window, cx) {
            return;
        }
        self.appearance = false;
        self.new_note_open = true;
        self.new_note_template = false;
        self.new_note_name
            .update(cx, |input, cx| input.set_value("", window, cx));
        let view = cx.entity().downgrade();
        let initial_focus = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |view, cx| {
                    if initial_focus.replace(false) {
                        view.new_note_name
                            .update(cx, |input, cx| input.focus(window, cx));
                    }
                    view.new_note_dialog(dialog, cx)
                })
            } else {
                dialog
            }
        });
        cx.notify();
    }

    pub(super) fn confirm_new_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.new_note_name.read(cx).value();
        match self.model.create_note(name.trim(), self.new_note_template) {
            Ok(()) => {
                self.set_editor(window, cx);
                self.new_note_open = false;
                window.close_dialog(cx);
                self.page = Page::Document;
                self.preview = false;
                self.status.clear();
                self.editor
                    .update(cx, |editor, cx| editor.focus(window, cx));
            }
            Err(error) => self.status = format!("创建遇到问题：{error}"),
        }
        cx.notify();
    }

    fn new_note_dialog(&self, dialog: Dialog, cx: &mut Context<Self>) -> Dialog {
        let t = self.model.theme.theme;
        dialog
            .title("新建笔记")
            .centered()
            .entrance(DialogEntrance::FadeSlide)
            .w(px(420.))
            .on_close(cx.listener(|this, _, _, cx| {
                this.new_note_open = false;
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(Input::new(&self.new_note_name).aria_label("笔记名称"))
                    .child(
                        Checkbox::new("use-template")
                            .label("使用笔记模板")
                            .checked(self.new_note_template)
                            .on_change(cx.listener(|this, checked, _, cx| {
                                this.new_note_template = *checked;
                                cx.notify();
                            })),
                    )
                    .when(
                        !self.status.is_empty() && self.status != "已保存",
                        |form| {
                            form.child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(color(t.ui_error))
                                    .child(self.status.clone()),
                            )
                        },
                    ),
            )
            .footer(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(self.button("cancel-new-note", "取消").on_click(cx.listener(
                        |this, _, window, cx| {
                            this.new_note_open = false;
                            window.close_dialog(cx);
                            cx.notify();
                        },
                    )))
                    .child(self.button("confirm-new-note", "创建").primary().on_click(
                        cx.listener(|this, _, window, cx| this.confirm_new_note(window, cx)),
                    )),
            )
    }
}
