use super::*;
use crate::gui::presentation::{
    RADIUS_MD, SPACE_LG, SPACE_MD, SPACE_SM, SPACE_XS, TEXT_LABEL, TEXT_META, TEXT_NAV,
};
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::TestSupportExt;

/// A destination in the sidebar navigation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Nav {
    Daily,
    Notes,
    Tags,
    Todo,
    Agent,
    Archives,
}

impl Nav {
    const ALL: [Nav; 6] = [
        Nav::Daily,
        Nav::Notes,
        Nav::Tags,
        Nav::Todo,
        Nav::Agent,
        Nav::Archives,
    ];

    fn id(self) -> &'static str {
        match self {
            Nav::Daily => "daily-nav",
            Nav::Notes => "notes-nav",
            Nav::Tags => "tags-nav",
            Nav::Todo => "todo-nav",
            Nav::Agent => "agent-nav",
            Nav::Archives => "archives-nav",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Nav::Daily => "日常记录",
            Nav::Notes => "笔记",
            Nav::Tags => "标签",
            Nav::Todo => "待办",
            Nav::Agent => "Agent",
            Nav::Archives => "归档",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Nav::Daily => "nole/daily",
            Nav::Notes => "nole/note",
            Nav::Tags => "nole/tag",
            Nav::Todo => "nole/todo",
            Nav::Agent => "nole/agent",
            Nav::Archives => "nole/archive",
        }
    }
}

/// The collection listed under the navigation. It follows the navigation:
/// Agent lists sessions, Archives lists archived notes, and every other page
/// lists the active notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SidebarList {
    Notes,
    Archives,
    Sessions,
}

#[derive(Clone, Copy)]
enum SidebarRow {
    Note(usize),
    Session(usize),
}

impl DesktopView {
    pub(super) fn refresh_sessions(&mut self) {
        self.sessions = self.model.sessions.list();
    }

    fn viewing_archive(&self) -> bool {
        match self.page {
            Page::Home => self.home.collection == Collection::Archives,
            Page::Document => self
                .model
                .selected
                .as_ref()
                .is_some_and(|path| path.starts_with("archives/")),
            _ => false,
        }
    }

    pub(super) fn active_nav(&self) -> Option<Nav> {
        Some(match self.page {
            Page::Daily => Nav::Daily,
            Page::Tags => Nav::Tags,
            Page::Todo => Nav::Todo,
            Page::Agent => Nav::Agent,
            Page::Search => return None,
            Page::Home | Page::Document if self.viewing_archive() => Nav::Archives,
            Page::Home | Page::Document => Nav::Notes,
        })
    }

    pub(super) fn sidebar_list(&self) -> SidebarList {
        match self.active_nav() {
            Some(Nav::Agent) => SidebarList::Sessions,
            Some(Nav::Archives) => SidebarList::Archives,
            _ => SidebarList::Notes,
        }
    }

    fn sidebar_rows(&self) -> Vec<SidebarRow> {
        match self.sidebar_list() {
            SidebarList::Notes => self
                .model
                .notes
                .iter()
                .copied()
                .map(SidebarRow::Note)
                .collect(),
            SidebarList::Archives => self
                .model
                .archives
                .iter()
                .copied()
                .map(SidebarRow::Note)
                .collect(),
            SidebarList::Sessions => (0..self.sessions.len()).map(SidebarRow::Session).collect(),
        }
    }

    fn open_nav(&mut self, nav: Nav, window: &mut Window, cx: &mut Context<Self>) {
        match nav {
            Nav::Daily => self.navigate(None, window, cx),
            Nav::Notes => self.open_collection(Collection::Notes, window, cx),
            Nav::Archives => self.open_collection(Collection::Archives, window, cx),
            Nav::Tags => {
                self.active_tag = None;
                self.open_page(Page::Tags, window, cx);
            }
            Nav::Todo => self.open_page(Page::Todo, window, cx),
            Nav::Agent => self.open_page(Page::Agent, window, cx),
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
        self.refresh_sessions();
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
        self.refresh_sessions();
        cx.notify();
    }

    pub(super) fn sidebar(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let rows = std::rc::Rc::new(self.sidebar_rows());
        let count = rows.len();
        let view = cx.entity().downgrade();
        let list = gpui_kit::uniform_list("sidebar-list", count, move |range, window, cx| {
            let rows = rows.clone();
            view.update(cx, |view, cx| {
                range
                    .map(|index| view.sidebar_row(index, rows[index], window, cx))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        })
        .track_scroll(&self.sidebar_scroll)
        .size_full();
        let (list_label, empty_hint) = match self.sidebar_list() {
            SidebarList::Notes => ("笔记", "新建的笔记会列在这里"),
            SidebarList::Archives => ("已归档", "归档的笔记会列在这里"),
            SidebarList::Sessions => ("会话", "与 Agent 的对话会列在这里"),
        };
        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(color(t.surface_panel))
            .border_r_1()
            .border_color(color(t.ui_border_subtle))
            .text_size(px(TEXT_NAV))
            .child(
                div()
                    .debug_selector(|| "sidebar-header".into())
                    .w_full()
                    .h(px(TOOLBAR_HEIGHT))
                    .pl(px(SPACE_LG + SPACE_XS))
                    .pr(px(SPACE_MD))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(15.))
                            .font_family(crate::gui::presentation::fonts(cx).serif)
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("NólëBase"),
                    )
                    .child(
                        self.icon_button("collapse-sidebar", "nole/sidebar", "收起侧栏")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                    ),
            )
            .child(self.sidebar_search(cx))
            .child(
                div()
                    .debug_selector(|| "sidebar-nav".into())
                    .px(px(SPACE_MD))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(Nav::ALL.map(|nav| self.nav_row(nav, cx))),
            )
            .child(
                div()
                    .px(px(SPACE_MD))
                    .flex()
                    .items_center()
                    .child(self.section_label(list_label))
                    .child(div().flex_1())
                    .child(
                        div()
                            .pt(px(SPACE_LG + SPACE_XS))
                            .pb(px(SPACE_SM))
                            .px(px(SPACE_SM + 2.))
                            .text_size(px(TEXT_LABEL))
                            .text_color(color(t.text_muted))
                            .child(count.to_string()),
                    ),
            )
            .child(
                div()
                    .id("sidebar-list-viewport")
                    .debug_selector(|| "note-list".into())
                    .relative()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .px(px(SPACE_MD))
                    .overflow_hidden()
                    .when(count == 0, |list| {
                        list.child(
                            div()
                                .px(px(SPACE_SM + 2.))
                                .text_size(px(TEXT_META))
                                .text_color(color(t.text_muted))
                                .child(empty_hint),
                        )
                    })
                    .child(list)
                    .child(Scrollbar::vertical(&self.sidebar_scroll)),
            )
            .child(self.sidebar_footer(cx))
    }

    fn sidebar_search(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.model.theme.theme;
        let active = self.page == Page::Search;
        let shortcut = if cfg!(target_os = "macos") {
            "⌘F"
        } else {
            "Ctrl F"
        };
        div()
            .id("search-nav")
            .debug_selector(|| "search-nav".into())
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label("搜索知识库")
            .mx(px(SPACE_MD))
            .mb(px(SPACE_LG))
            .h(px(32.))
            .px(px(SPACE_MD - 2.))
            .flex()
            .items_center()
            .gap(px(SPACE_SM))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(color(if active {
                t.ui_focus_border
            } else {
                t.ui_border_subtle
            }))
            .bg(color(t.surface_raised))
            .text_color(color(t.text_muted))
            .cursor_pointer()
            .hover(move |row| row.border_color(color(t.ui_border)))
            .child(icon("nole/search").size(px(15.)))
            .child(div().flex_1().child("搜索知识库"))
            .child(
                div()
                    .px(px(5.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(color(t.ui_border_subtle))
                    .text_size(px(TEXT_LABEL))
                    .child(shortcut),
            )
            .on_click(cx.listener(|this, _, window, cx| this.open_page(Page::Search, window, cx)))
    }

    fn nav_row(&self, nav: Nav, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.model.theme.theme;
        let active = self.active_nav() == Some(nav);
        let count = match nav {
            Nav::Notes => Some(self.model.notes.len()),
            Nav::Archives => Some(self.model.archives.len()),
            Nav::Todo => Some(self.model.todos.iter().filter(|todo| !todo.checked).count()),
            Nav::Daily | Nav::Tags | Nav::Agent => None,
        };
        let running = nav == Nav::Agent && self.sessions.iter().any(|session| session.running);
        let id = nav.id();
        div()
            .id(id)
            .debug_selector(|| id.into())
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(nav.label())
            .aria_selected(active)
            .h(px(32.))
            .px(px(SPACE_MD - 2.))
            .flex()
            .items_center()
            .gap(px(SPACE_MD - 2.))
            .rounded(px(RADIUS_MD))
            .border_1()
            .cursor_pointer()
            .when(active, |row| {
                row.bg(color(t.surface_raised))
                    .border_color(color(t.ui_border_subtle))
                    .shadow_xs()
                    .text_color(color(t.text_primary))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            })
            .when(!active, |row| {
                let hover = self.row_hover();
                row.border_color(color(t.surface_panel).opacity(0.))
                    .text_color(color(t.text_secondary))
                    .hover(move |row| row.bg(hover))
            })
            .child(
                div()
                    .text_color(color(if active { t.ui_action } else { t.text_muted }))
                    .child(icon(nav.glyph()).size(px(16.))),
            )
            .child(div().flex_1().child(nav.label()))
            .when(running, |row| row.child(self.activity_dot(t.ui_action_ai)))
            .when_some(count.filter(|count| *count > 0), |row, count| {
                row.child(
                    div()
                        .text_size(px(TEXT_META))
                        .font_weight(gpui_kit::FontWeight::NORMAL)
                        .text_color(color(t.text_muted))
                        .child(count.to_string()),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| this.open_nav(nav, window, cx)))
    }

    fn activity_dot(&self, tone: crate::core::theme::Color) -> gpui_kit::Div {
        div()
            .size(px(7.))
            .flex_shrink_0()
            .rounded(px(4.))
            .bg(color(tone))
    }

    fn sidebar_row(
        &self,
        index: usize,
        row: SidebarRow,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        #[cfg(test)]
        self.sidebar_rendered.set(self.sidebar_rendered.get() + 1);
        let t = self.model.theme.theme;
        let slot = div().w_full().h(px(NOTE_ROW_HEIGHT + 2.)).pb(px(2.));
        let hover = self.row_hover();
        let item = |id: gpui_kit::SharedString, label: String, active: bool| {
            div()
                .id(id.clone())
                .debug_selector(move || id.to_string())
                .test_support()
                .role(gpui_kit::Role::TreeItem)
                .aria_label(label.clone())
                .aria_selected(active)
                .size_full()
                .px(px(SPACE_MD - 2.))
                .flex()
                .items_center()
                .gap(px(SPACE_SM))
                .rounded(px(RADIUS_MD - 2.))
                .cursor_pointer()
                .when(active, |row| {
                    row.bg(color(t.selection_background))
                        .text_color(color(t.ui_action))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                })
                .when(!active, |row| {
                    row.text_color(color(t.text_secondary))
                        .hover(move |row| row.bg(hover))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(label),
                )
        };
        match row {
            SidebarRow::Note(file) => {
                let doc = &self.model.documents[file];
                let path = doc.path.clone();
                let active =
                    self.page == Page::Document && self.model.selected.as_ref() == Some(&path);
                let open = path.clone();
                slot.when(index == 0, |slot| {
                    slot.debug_selector(|| "first-note".into())
                })
                .child(
                    item(format!("file-{path}").into(), doc.title.clone(), active)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.navigate(Some(open.clone()), window, cx)
                        }))
                        .context_menu(self.note_context_menu(path, cx)),
                )
                .into_any_element()
            }
            SidebarRow::Session(position) => {
                let session = &self.sessions[position];
                let id = session.id;
                let active = self.page == Page::Agent
                    && self
                        .agent
                        .as_ref()
                        .is_some_and(|agent| agent.current.summary().id == id);
                let marker = if session.running {
                    Some(t.ui_action_ai)
                } else if session.unread {
                    Some(t.ui_action)
                } else {
                    None
                };
                slot.child(
                    item(id.to_string().into(), session.title.clone(), active)
                        .when_some(marker, |row, tone| row.child(self.activity_dot(tone)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_agent_session(id, window, cx)
                        })),
                )
                .into_any_element()
            }
        }
    }

    fn sidebar_footer(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let root = self.model.workspace.storage().root.clone();
        let root = dirs::home_dir()
            .and_then(|home| root.strip_prefix(home).ok().map(|rest| rest.to_path_buf()))
            .map(|rest| format!("~/{}", rest.display()))
            .unwrap_or_else(|| root.display().to_string());
        div()
            .w_full()
            .h(px(44.))
            .px(px(SPACE_LG + SPACE_XS))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(SPACE_SM))
            .border_t_1()
            .border_color(color(t.ui_border_subtle))
            .text_size(px(TEXT_META))
            .text_color(color(t.text_muted))
            .child(self.activity_dot(t.ui_success))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(format!("本地工作区 · {root}")),
            )
            .child(
                self.icon_button("appearance", "nole/appearance", "外观")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.appearance = !this.appearance;
                        cx.notify();
                    })),
            )
    }
}
