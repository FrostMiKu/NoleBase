use super::{
    home::{Collection, GridLayout, Home},
    icons::icon,
    model::Desktop,
    presentation::PAGE_INSET,
    theme::{self, color},
};
use crate::core::{
    agent_sessions::SessionSummary,
    model::{FileGroup, FileListRow},
};
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    scroll::Scrollbar,
    Sizable,
};
use gpui_kit::{
    div, prelude::*, px, Animation, AnimationExt, Context, Entity, IntoElement, ListAlignment,
    ListState, Render, Subscription, Task, UniformListScrollHandle, Window,
};
use std::time::Duration;

mod agent;
mod agent_entry;
mod discovery;
mod home;
mod persistence;
mod preview;
mod runtime;
mod sidebar;
mod surfaces;
mod todo;
mod workbench;

const SIDEBAR_WIDTH: f32 = 232.;
const NOTE_ROW_HEIGHT: f32 = 32.;
const LIST_TOP: f32 = 12.;
const TOOLBAR_HEIGHT: f32 = 48.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Daily,
    Document,
    Tags,
    Search,
    Agent,
    Todo,
}

gpui_kit::actions!(
    desktop,
    [
        TogglePreview,
        ToggleSidebar,
        OpenSearch,
        OpenAgent,
        NewNote,
        ToggleTodo
    ]
);

pub struct DesktopView {
    focus: gpui_kit::FocusHandle,
    model: Desktop,
    tree_rows: Vec<FileListRow>,
    tree_sessions: Vec<SessionSummary>,
    tree_expanded: [bool; 3],
    todo_scroll: UniformListScrollHandle,
    autosave_task: Option<Task<()>>,
    save_pending: bool,
    editor: Entity<TextareaState>,
    draft: gpui_kit::SharedString,
    draft_chars: usize,
    draft_count_task: Option<Task<()>>,
    search_pending: bool,
    search: Entity<InputState>,
    preview: bool,
    page: Page,
    active_tag: Option<String>,
    agent: Option<super::agent::AgentClient>,
    expanded_thinking: std::collections::HashSet<String>,
    agent_input: Entity<TextareaState>,
    answer_input: Entity<InputState>,
    private_input: Entity<InputState>,
    home_scroll: [UniformListScrollHandle; 2],
    home: Home,
    new_note_open: bool,
    new_note_template: bool,
    new_note_name: Entity<InputState>,
    sidebar_scroll: UniformListScrollHandle,
    results_scroll: UniformListScrollHandle,
    tags_scroll: UniformListScrollHandle,
    daily_list: ListState,
    agent_list: ListState,
    grid_columns: u16,
    list_generation: u64,
    watcher: Option<crate::core::workspace_watcher::WorkspaceWatcher>,
    _poll_task: Task<()>,
    search_task: Option<Task<()>>,
    #[cfg(test)]
    home_rendered: std::rc::Rc<std::cell::Cell<usize>>,
    #[cfg(test)]
    sidebar_rendered: std::rc::Rc<std::cell::Cell<usize>>,
    sidebar: bool,
    sidebar_motion: super::motion::SidebarMotion,
    appearance: bool,
    status: String,
    _subscriptions: Vec<Subscription>,
}

impl DesktopView {
    pub fn new(model: Desktop, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let editor = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("写下此刻的想法……")
                .default_value(model.source.clone())
        });
        let new_note_name = cx.new(|cx| InputState::new(window, cx).placeholder("笔记名称"));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索知识库"));
        let agent_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("让 Agent 帮你整理知识、查找资料或完成任务……")
        });
        let answer_input = cx.new(|cx| InputState::new(window, cx).placeholder("输入回答"));
        let private_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("输入后提交")
        });
        let subscriptions = vec![
            cx.subscribe_in(&new_note_name, window, |view, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    view.confirm_new_note(window, cx);
                }
            }),
            cx.subscribe_in(&search, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.queue_search(window, cx);
                }
            }),
            cx.subscribe_in(&editor, window, |view, input, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    view.queue_autosave(window, cx);
                    let text = input.read(cx).text().clone();
                    view.draft_count_task = Some(cx.spawn(async move |view, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(120))
                            .await;
                        let count = cx
                            .background_spawn(async move { text.chars().count() })
                            .await;
                        let _ = view.update(cx, |view, cx| {
                            view.draft_chars = count;
                            cx.notify();
                        });
                    }));
                    cx.notify();
                }
            }),
        ];
        let watcher =
            crate::core::workspace_watcher::WorkspaceWatcher::new(model.workspace.storage()).ok();
        let poll_task = cx.spawn(async move |view, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(40))
                .await;
            if view.update(cx, |view, cx| view.poll_runtime(cx)).is_err() {
                break;
            }
        });
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            view.update(cx, |this, cx| this.flush_save(window, cx))
                .unwrap_or(true)
        });
        let tree_sessions = model.sessions.list();
        let tree_rows = Self::build_tree(&model, &tree_sessions, [true; 3]);
        Self {
            focus,
            tree_rows,
            tree_sessions,
            tree_expanded: [true; 3],
            todo_scroll: UniformListScrollHandle::new(),
            autosave_task: None,
            save_pending: false,
            draft: model.source.clone().into(),
            draft_chars: model.source.chars().count(),
            search_pending: false,
            draft_count_task: None,
            editor,
            search,
            preview: false,
            page: Page::Home,
            active_tag: None,
            agent: None,
            expanded_thinking: Default::default(),
            agent_input,
            answer_input,
            private_input,
            home_scroll: std::array::from_fn(|_| UniformListScrollHandle::new()),
            home: Home::default(),
            new_note_open: false,
            new_note_template: false,
            new_note_name,
            sidebar_scroll: UniformListScrollHandle::new(),
            results_scroll: UniformListScrollHandle::new(),
            tags_scroll: UniformListScrollHandle::new(),
            daily_list: ListState::new(model.daily_blocks.len(), ListAlignment::Top, px(200.)),
            agent_list: ListState::new(0, ListAlignment::Bottom, px(200.)),
            grid_columns: 0,
            list_generation: model.generation,
            watcher,
            _poll_task: poll_task,
            search_task: None,
            #[cfg(test)]
            home_rendered: Default::default(),
            #[cfg(test)]
            sidebar_rendered: Default::default(),
            sidebar: true,
            sidebar_motion: super::motion::SidebarMotion::new(SIDEBAR_WIDTH),
            appearance: false,
            status: String::new(),
            _subscriptions: subscriptions,
            model,
        }
    }

    #[cfg(feature = "gui-test")]
    pub(super) fn review_document(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Some(path), window, cx);
        self.preview = true;
        cx.notify();
    }

    fn set_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.model.source.clone();
        self.draft = source.clone().into();
        self.draft_chars = source.chars().count();
        self.editor
            .update(cx, |editor, cx| editor.set_value(source, window, cx));
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.save_pending = false;
        let draft = self.editor.read(cx).value();
        match self.model.save(&draft) {
            Ok(()) => {
                if self.model.selected.is_none() {
                    self.editor
                        .update(cx, |editor, cx| editor.set_value("", window, cx));
                }
                self.sync_daily_list();
                self.status = "已保存".into();
                cx.notify();
                true
            }
            Err(error) => {
                self.status = format!("保存遇到问题：{error}");
                cx.notify();
                false
            }
        }
    }

    fn navigate(&mut self, path: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.flush_save(window, cx) {
            return;
        }
        match self.model.open_document(path) {
            Ok(()) => {
                self.set_editor(window, cx);
                self.preview = self.model.selected.is_some();
                self.page = if self.model.selected.is_some() {
                    Page::Document
                } else {
                    Page::Daily
                };
                if self.preview {
                    self.focus.focus(window, cx);
                } else {
                    self.editor
                        .update(cx, |editor, cx| editor.focus(window, cx));
                }
                self.status.clear();
            }
            Err(error) => self.status = format!("打开遇到问题：{error}"),
        }
        cx.notify();
    }

    fn open_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(None, window, cx);
        if self.model.selected.is_none() && self.editor.read(cx).value().is_empty() {
            self.page = Page::Home;
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar = !self.sidebar;
        self.sidebar_motion.retarget(
            if self.sidebar { SIDEBAR_WIDTH } else { 0. },
            std::time::Instant::now(),
        );
        cx.notify();
    }

    fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = !self.preview;
        if self.preview {
            self.draft = self.editor.read(cx).value();
            self.focus.focus(window, cx);
        } else {
            self.editor
                .update(cx, |editor, cx| editor.focus(window, cx));
        }
        cx.notify();
    }

    fn button(&self, id: &'static str, label: impl Into<gpui_kit::SharedString>) -> Button {
        let label = label.into();
        Button::new(id)
            .ghost()
            .small()
            .debug_selector(|| id.to_owned())
            .accessibility_label(label.clone())
            .child(div().debug_selector(|| format!("{id}-label")).child(label))
    }

    fn icon_button(&self, id: &'static str, name: &'static str, tooltip: &'static str) -> Button {
        Button::new(id)
            .ghost()
            .small()
            .debug_selector(|| id.to_owned())
            .icon(icon(name))
            .tooltip(tooltip)
    }
}
#[cfg(all(test, feature = "gui-test"))]
mod tests;
