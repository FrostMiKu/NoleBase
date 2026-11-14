use super::*;
use gpui_kit::component::sidebar::{SidebarFooter, SidebarHeader, SidebarItem, SidebarMenuItem};

impl DesktopView {
    pub(super) fn build_tree(
        model: &Desktop,
        sessions: &[SessionSummary],
        expanded: [bool; 3],
    ) -> Vec<FileListRow> {
        let mut rows = Vec::new();
        for (group, open) in [
            (FileGroup::Notes, expanded[0]),
            (FileGroup::Agents, expanded[2]),
            (FileGroup::Archives, expanded[1]),
        ] {
            rows.push(FileListRow::Group(group));
            if open {
                match group {
                    FileGroup::Notes => {
                        rows.extend(model.notes.iter().copied().map(FileListRow::File))
                    }
                    FileGroup::Archives => {
                        rows.extend(model.archives.iter().copied().map(FileListRow::File))
                    }
                    FileGroup::Agents => rows.extend(
                        sessions
                            .iter()
                            .map(|session| FileListRow::Agent(session.id)),
                    ),
                }
            }
        }
        rows
    }

    pub(super) fn rebuild_tree(&mut self) {
        self.tree_sessions = self.model.sessions.list();
        self.tree_rows = Self::build_tree(&self.model, &self.tree_sessions, self.tree_expanded);
    }

    fn tree_group(group: FileGroup) -> (usize, &'static str, &'static str, &'static str) {
        match group {
            FileGroup::Notes => (0, "Notes", "nole/folder", "tree-notes"),
            FileGroup::Archives => (1, "Archives", "nole/folder", "tree-archives"),
            FileGroup::Agents => (2, "Agents", "nole/folder", "tree-agents"),
        }
    }

    pub(super) fn open_collection(
        &mut self,
        collection: Collection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_home(window, cx);
        if self.page == Page::Home {
            self.home.switch(collection);
        }
        cx.notify();
    }

    pub(super) fn open_agent_session(
        &mut self,
        id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_page(Page::Agent, window, cx);
        self.expanded_thinking.clear();
        self.private_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.answer_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        if let Some(agent) = &mut self.agent {
            match agent.select(id) {
                Ok(()) => self.agent_list.reset(agent.rows.len()),
                Err(error) => self.status = error.to_string(),
            }
        }
        self.rebuild_tree();
        cx.notify();
    }

    pub(super) fn new_agent_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_page(Page::Agent, window, cx);
        self.expanded_thinking.clear();
        self.private_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.answer_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        if let Some(agent) = &mut self.agent {
            match agent.new_session() {
                Ok(()) => self.agent_list.reset(0),
                Err(error) => self.status = error.to_string(),
            }
        }
        self.rebuild_tree();
        cx.notify();
    }

    pub(super) fn sidebar(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let view = cx.entity().downgrade();
        let tree = gpui_kit::uniform_list(
            "file-tree",
            self.tree_rows.len(),
            move |range, window, cx| {
                view.update(cx, |view, cx| {
                    range
                        .map(|index| view.tree_row(index, window, cx))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
            },
        )
        .track_scroll(&self.sidebar_scroll)
        .size_full();
        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(color(t.surface_panel))
            .border_r_1()
            .border_color(color(t.ui_border_subtle).opacity(0.45))
            .text_size(px(13.))
            .child(
                SidebarHeader::new()
                    .w_full()
                    .h(px(TOOLBAR_HEIGHT))
                    .px(px(14.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("NólëBase"),
                    )
                    .child(
                        self.icon_button("sidebar-new", "nole/plus", "新建笔记")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.begin_new_note(window, cx)),
                            ),
                    )
                    .child(
                        self.icon_button("collapse-sidebar", "nole/sidebar", "收起侧栏")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_sidebar(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .id("note-list")
                    .debug_selector(|| "note-list".into())
                    .relative()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .px(px(4.))
                    .overflow_hidden()
                    .pt(px(LIST_TOP))
                    .child(tree)
                    .child(Scrollbar::vertical(&self.sidebar_scroll)),
            )
            .child(
                SidebarFooter::new()
                    .w_full()
                    .h(px(44.))
                    .px(px(12.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        self.icon_button("daily-nav", "nole/daily", "日常记录")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.navigate(None, window, cx)),
                            ),
                    )
                    .child(
                        self.icon_button("search-nav", "nole/search", "搜索")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_page(Page::Search, window, cx)
                            })),
                    )
                    .child(
                        self.icon_button("tags-nav", "nole/tag", "Tags")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.active_tag = None;
                                this.open_page(Page::Tags, window, cx);
                            })),
                    )
                    .child(
                        self.icon_button("todo-nav", "nole/todo", "Todo")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_page(Page::Todo, window, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        self.icon_button("appearance", "nole/appearance", "外观")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.appearance = !this.appearance;
                                cx.notify();
                            })),
                    ),
            )
    }

    fn tree_row(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> gpui_kit::Div {
        #[cfg(test)]
        self.sidebar_rendered.set(self.sidebar_rendered.get() + 1);
        let row = div().w_full().h(px(NOTE_ROW_HEIGHT + 2.)).pb(px(2.));
        match self.tree_rows[index] {
            FileListRow::Group(group) => {
                let (slot, label, glyph, id) = Self::tree_group(group);
                let selected = match group {
                    FileGroup::Notes => {
                        self.page == Page::Home && self.home.collection == Collection::Notes
                    }
                    FileGroup::Archives => {
                        self.page == Page::Home && self.home.collection == Collection::Archives
                    }
                    FileGroup::Agents => {
                        self.page == Page::Agent
                            && self.agent.as_ref().is_some_and(|agent| {
                                !self
                                    .tree_sessions
                                    .iter()
                                    .any(|session| session.id == agent.current.summary().id)
                            })
                    }
                };
                let expanded = self.tree_expanded[slot];
                row.debug_selector(|| id.into()).child(
                    SidebarMenuItem::new(label)
                        .icon(icon(if self.tree_expanded[slot] {
                            "nole/folder-open"
                        } else {
                            glyph
                        }))
                        .active(selected)
                        .suffix(move |_, _| {
                            icon(if expanded {
                                "nole/chevron-down"
                            } else {
                                "nole/chevron-right"
                            })
                            .size(px(12.))
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.tree_expanded[slot] = !this.tree_expanded[slot];
                            match group {
                                FileGroup::Notes => {
                                    this.open_collection(Collection::Notes, window, cx)
                                }
                                FileGroup::Archives => {
                                    this.open_collection(Collection::Archives, window, cx)
                                }
                                FileGroup::Agents => this.open_page(Page::Agent, window, cx),
                            }
                            this.rebuild_tree();
                            cx.notify();
                        }))
                        .render(id, window, cx),
                )
            }
            FileListRow::File(file) => {
                let doc = &self.model.documents[file];
                let path = doc.path.clone();
                let selected =
                    self.page == Page::Document && self.model.selected.as_ref() == Some(&path);
                row.when(file == 0, |row| row.debug_selector(|| "first-note".into()))
                    .pl(px(20.))
                    .child(
                        SidebarMenuItem::new(doc.title.clone())
                            .icon(icon("nole/note"))
                            .active(selected)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.navigate(Some(path.clone()), window, cx)
                            }))
                            .render(
                                gpui_kit::SharedString::from(format!("file-{}", doc.path)),
                                window,
                                cx,
                            ),
                    )
            }
            FileListRow::Agent(id) => {
                let Some(session) = self.tree_sessions.iter().find(|session| session.id == id)
                else {
                    return row;
                };
                let selected = self.page == Page::Agent
                    && self
                        .agent
                        .as_ref()
                        .is_some_and(|agent| agent.current.summary().id == id);
                row.pl(px(20.)).child(
                    SidebarMenuItem::new(session.title.clone())
                        .icon(icon("nole/agent"))
                        .active(selected)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_agent_session(id, window, cx)
                        }))
                        .render(gpui_kit::SharedString::from(id.to_string()), window, cx),
                )
            }
        }
    }
}
