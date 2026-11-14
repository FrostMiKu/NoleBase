use super::*;
use gpui_kit::component::{
    dialog::{Dialog, DialogEntrance},
    menu::{PopupMenu, PopupMenuItem},
    WindowExt,
};

#[derive(Clone)]
pub(super) enum NoteAction {
    Rename(String),
    Delete(String),
}

impl DesktopView {
    pub(super) fn note_context_menu(
        &self,
        path: String,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = cx.entity().downgrade();
        move |menu, _, _| {
            let rename_view = view.clone();
            let rename_path = path.clone();
            let move_view = view.clone();
            let move_path = path.clone();
            let delete_view = view.clone();
            let delete_path = path.clone();
            menu.item(PopupMenuItem::new("重命名").on_click(move |_, window, cx| {
                let _ = rename_view.update(cx, |view, cx| {
                    view.begin_note_action(NoteAction::Rename(rename_path.clone()), window, cx);
                });
            }))
            .item(
                PopupMenuItem::new(if path.starts_with("archives/") {
                    "恢复"
                } else {
                    "归档"
                })
                .on_click(move |_, window, cx| {
                    let _ = move_view.update(cx, |view, cx| {
                        view.move_note(&move_path, window, cx);
                    });
                }),
            )
            .separator()
            .item(PopupMenuItem::new("删除").on_click(move |_, window, cx| {
                let _ = delete_view.update(cx, |view, cx| {
                    view.begin_note_action(NoteAction::Delete(delete_path.clone()), window, cx);
                });
            }))
        }
    }

    fn begin_note_action(
        &mut self,
        action: NoteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.flush_save(window, cx) {
            return;
        }
        if let NoteAction::Rename(path) = &action {
            let name = std::path::Path::new(path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            self.note_action_name
                .update(cx, |input, cx| input.set_value(name, window, cx));
        }
        self.note_action = Some(action);
        self.status.clear();
        let view = cx.entity().downgrade();
        let initial_focus = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            view.update(cx, |view, cx| {
                if initial_focus.replace(false)
                    && matches!(view.note_action, Some(NoteAction::Rename(_)))
                {
                    view.note_action_name
                        .update(cx, |input, cx| input.focus(window, cx));
                }
                view.note_action_dialog(dialog, cx)
            })
            .unwrap_or_else(|_| Dialog::new(cx))
        });
        cx.notify();
    }

    fn note_action_dialog(&self, dialog: Dialog, cx: &mut Context<Self>) -> Dialog {
        let rename = matches!(self.note_action, Some(NoteAction::Rename(_)));
        let mut dialog = dialog
            .title(if rename {
                "重命名笔记"
            } else {
                "删除笔记"
            })
            .centered()
            .entrance(DialogEntrance::FadeSlide)
            .w(px(420.))
            .on_close(cx.listener(|view, _, _, cx| {
                view.note_action = None;
                cx.notify();
            }));
        if rename {
            dialog = dialog.child(Input::new(&self.note_action_name).aria_label("笔记名称"));
        } else if let Some(NoteAction::Delete(path)) = &self.note_action {
            let name = std::path::Path::new(path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            dialog = dialog.child(format!("将永久删除「{name}」。"));
        }
        dialog
            .when(!self.status.is_empty(), |dialog| {
                dialog.child(
                    div()
                        .mt(px(12.))
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
                        self.button("cancel-note-action", "取消")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.note_action = None;
                                window.close_dialog(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        self.button(
                            "confirm-note-action",
                            if rename { "重命名" } else { "删除" },
                        )
                        .primary()
                        .on_click(
                            cx.listener(|view, _, window, cx| view.confirm_note_action(window, cx)),
                        ),
                    ),
            )
    }

    pub(super) fn confirm_note_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.note_action.clone() else {
            return;
        };
        if !self.flush_save(window, cx) {
            return;
        }
        let result = (|| -> anyhow::Result<()> {
            let (path, destination) = match &action {
                NoteAction::Rename(path) => {
                    let from = self.model.workspace.document_path(path)?;
                    let name = self.note_action_name.read(cx).value();
                    let to = if path.starts_with("archives/") {
                        self.model
                            .workspace
                            .rename_archived_note(&from, name.trim())?
                    } else {
                        self.model.workspace.rename_note(&from, name.trim())?
                    };
                    (
                        path,
                        Some(
                            self.model
                                .workspace
                                .relative_path(&to)
                                .ok_or_else(|| anyhow::anyhow!("笔记路径超出了知识库范围"))?,
                        ),
                    )
                }
                NoteAction::Delete(path) => {
                    self.model
                        .workspace
                        .delete_note(&self.model.workspace.document_path(path)?)?;
                    (path, None)
                }
            };
            self.finish_note_action(path, destination, window, cx)
        })();
        match result {
            Ok(()) => {
                self.note_action = None;
                window.close_dialog(cx);
            }
            Err(error) => self.status = format!("操作遇到问题：{error}"),
        }
        cx.notify();
    }

    fn move_note(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.flush_save(window, cx) {
            return;
        }
        let result = (|| -> anyhow::Result<()> {
            let from = self.model.workspace.document_path(path)?;
            let to = if path.starts_with("archives/") {
                self.model.workspace.restore_note(&from)?
            } else {
                self.model.workspace.archive_note(&from)?
            };
            let destination = self
                .model
                .workspace
                .relative_path(&to)
                .ok_or_else(|| anyhow::anyhow!("笔记路径超出了知识库范围"))?;
            self.finish_note_action(path, Some(destination), window, cx)
        })();
        self.status = result
            .err()
            .map(|error| format!("操作遇到问题：{error}"))
            .unwrap_or_default();
        cx.notify();
    }

    fn finish_note_action(
        &mut self,
        path: &str,
        destination: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if self.model.selected.as_deref() == Some(path) {
            self.model.selected = destination;
            if self.model.selected.is_none() {
                self.page = Page::Home;
            }
        }
        self.home.selected.remove(path);
        if let Some(selected) = self.model.selected.clone() {
            self.model.open_document(Some(selected))?;
        } else if self.page == Page::Home {
            self.model.source.clear();
        }
        self.set_editor(window, cx);
        self.model.refresh()?;
        self.sync_daily_list();
        self.status.clear();
        Ok(())
    }
}
