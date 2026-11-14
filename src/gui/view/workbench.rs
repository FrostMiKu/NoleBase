use super::*;
use crate::gui::presentation::{RADIUS_MD, SPACE_MD, SPACE_XS, TEXT_META, TEXT_NAV, TEXT_UI};

impl DesktopView {
    pub(super) fn main_scrollbar(
        &self,
        handle: &(impl gpui_kit::component::scroll::ScrollbarHandle + Clone),
        top: f32,
    ) -> gpui_kit::Div {
        div()
            .debug_selector(|| "main-scrollbar-viewport".into())
            .absolute()
            .top(px(top))
            .bottom_0()
            .left_0()
            .right(-px(PAGE_INSET))
            .child(Scrollbar::vertical(handle).viewport_from_layout())
    }

    /// The section a page belongs to and the page's own name.
    fn page_title(&self) -> (Option<&'static str>, String) {
        match self.page {
            Page::Home => (
                None,
                match self.home.collection {
                    Collection::Notes => "笔记".into(),
                    Collection::Archives => "归档".into(),
                },
            ),
            Page::Daily => (None, "日常记录".into()),
            Page::Tags => match &self.active_tag {
                Some(tag) => (Some("标签"), format!("#{tag}")),
                None => (None, "标签".into()),
            },
            Page::Search => (None, "搜索".into()),
            Page::Todo => (None, "待办".into()),
            Page::Agent => match &self.agent {
                Some(agent) => (Some("Agent"), agent.current.summary().title),
                None => (None, "Agent".into()),
            },
            Page::Document => {
                let path = self.model.selected.as_deref().unwrap_or_default();
                let section = if path.starts_with("archives/") {
                    "归档"
                } else {
                    "笔记"
                };
                let title = self
                    .model
                    .documents
                    .iter()
                    .find(|doc| doc.path == path)
                    .map(|doc| doc.title.clone())
                    .unwrap_or_default();
                (Some(section), title)
            }
        }
    }

    fn toolbar(&self, _window: &Window, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let (section, title) = self.page_title();
        let mut bar = div()
            .debug_selector(|| "workspace-toolbar".into())
            .w_full()
            .h(px(TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .px(px(PAGE_INSET))
            .flex()
            .items_center()
            .gap(px(SPACE_MD))
            .border_b_1()
            .border_color(color(t.ui_border_subtle))
            .when(!self.sidebar, |bar| {
                bar.child(
                    self.icon_button("expand-sidebar", "nole/sidebar", "展开侧栏")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                )
            })
            .child(
                div()
                    .debug_selector(|| "page-title".into())
                    .flex_1()
                    .min_w(px(64.))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS + 2.))
                    .overflow_hidden()
                    .text_size(px(TEXT_NAV))
                    .when_some(section, |crumb, section| {
                        crumb
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(color(t.text_muted))
                                    .child(section),
                            )
                            .child(div().text_color(color(t.text_disabled)).child("/"))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(TEXT_UI))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(color(t.text_primary))
                            .child(title),
                    ),
            );
        bar = match self.page {
            Page::Home => bar.child(self.home_toolbar(cx)).child(
                self.primary_button("new-note", "新建笔记")
                    .icon(icon("nole/plus").size(px(14.)))
                    .on_click(cx.listener(|this, _, window, cx| this.begin_new_note(window, cx))),
            ),
            Page::Agent => bar.child(
                self.icon_button("new-session", "nole/plus", "新会话")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.new_agent_session(window, cx)),
                    ),
            ),
            Page::Document => bar
                .child(self.save_state())
                .child(self.mode_switch(cx))
                .child(
                    self.button("ask-agent", "问 Agent")
                        .icon(icon("nole/agent").size(px(14.)))
                        .bg(color(t.ui_action_ai).opacity(0.12))
                        .text_color(color(t.ui_action_ai))
                        .on_click(cx.listener(|this, _, window, cx| this.ask_agent(window, cx))),
                ),
            _ => bar,
        };
        bar
    }

    fn save_state(&self) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let saved = !self.save_pending && self.status == "已保存";
        div()
            .debug_selector(|| "save-state".into())
            .flex()
            .items_center()
            .gap(px(SPACE_XS + 2.))
            .text_size(px(TEXT_META))
            .text_color(color(t.text_muted))
            .when(saved, |state| {
                state
                    .child(
                        div()
                            .text_color(color(t.ui_success))
                            .child(icon("nole/check").size(px(14.))),
                    )
                    .child("已自动保存")
            })
            .when(self.save_pending, |state| state.child("编辑中…"))
    }

    /// The reading and editing segments of the document toolbar.
    fn mode_switch(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let segment = |id: &'static str, label: &'static str, active: bool| {
            div()
                .id(id)
                .debug_selector(|| id.into())
                .px(px(SPACE_MD))
                .py(px(3.))
                .rounded(px(RADIUS_MD - 2.))
                .text_size(px(TEXT_META))
                .cursor_pointer()
                .when(active, |segment| {
                    segment
                        .bg(color(t.surface_raised))
                        .shadow_xs()
                        .text_color(color(t.text_primary))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                })
                .when(!active, |segment| {
                    segment.text_color(color(t.text_secondary))
                })
                .child(label)
        };
        div()
            .debug_selector(|| "mode-switch".into())
            .flex()
            .p(px(2.))
            .rounded(px(RADIUS_MD))
            .bg(color(t.surface_sunken))
            .child(
                segment("read-mode", "阅读", self.preview).on_click(cx.listener(
                    |this, _, window, cx| {
                        if !this.preview {
                            this.toggle_preview(window, cx)
                        }
                    },
                )),
            )
            .child(
                segment("edit-mode", "编辑", !self.preview).on_click(cx.listener(
                    |this, _, window, cx| {
                        if this.preview {
                            this.toggle_preview(window, cx)
                        }
                    },
                )),
            )
    }

    /// Start a conversation about the open note, referencing it by wiki link.
    fn ask_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self
            .model
            .selected
            .as_ref()
            .and_then(|path| self.model.documents.iter().find(|doc| &doc.path == path))
            .map(|doc| doc.title.clone())
        else {
            return;
        };
        self.open_page(Page::Agent, window, cx);
        if self.page == Page::Agent {
            self.agent_input.update(cx, |input, cx| {
                input.insert(format!("[[{title}]] "), window, cx)
            });
        }
    }
}

impl Render for DesktopView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.model.theme.theme;
        let surface = match self.page {
            Page::Home => self.home_surface(window, cx).into_any_element(),
            Page::Daily => self.daily_surface(window, cx).into_any_element(),
            Page::Document => self.document_surface(window, cx).into_any_element(),
            Page::Tags => self.tags_surface(cx).into_any_element(),
            Page::Search => self.search_surface(cx).into_any_element(),
            Page::Agent => self.agent_surface(window, cx).into_any_element(),
            Page::Todo => self.todo_surface(cx).into_any_element(),
        };
        let now = std::time::Instant::now();
        if self.sidebar_motion.active(now) {
            window.request_animation_frame();
        }
        let sidebar = div()
            .h_full()
            .w(px(self.sidebar_motion.value(now)))
            .flex_shrink_0()
            .overflow_hidden()
            .child(self.sidebar(cx));
        let transition_key = gpui_kit::SharedString::from(format!(
            "page/{}/{:?}/{}/{:?}/{}",
            self.page as u8,
            self.model.selected,
            self.preview,
            self.active_tag,
            self.home.collection.index(),
        ));
        let problem = !self.new_note_open && !self.status.is_empty() && self.status != "已保存";
        div()
            .relative()
            .key_context("Desktop")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .bg(color(t.surface_canvas))
            .text_color(color(t.text_primary))
            .text_size(px(TEXT_UI))
            .on_action(cx.listener(|this, _: &NewNote, window, cx| this.begin_new_note(window, cx)))
            .on_action(cx.listener(|this, _: &OpenSearch, window, cx| {
                this.open_page(Page::Search, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenAgent, window, cx| {
                this.open_page(Page::Agent, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &TogglePreview, window, cx| this.toggle_preview(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.toggle_sidebar(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleTodo, window, cx| {
                this.open_page(Page::Todo, window, cx);
            }))
            .child(sidebar)
            .child(
                div()
                    .debug_selector(|| "main-workspace".into())
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.toolbar(window, cx))
                    .child(
                        div()
                            .debug_selector(|| "page-content".into())
                            .when(self.page != Page::Document, |page| page.p(px(PAGE_INSET)))
                            .w_full()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(surface)
                            .with_animation(
                                transition_key,
                                Animation::new(super::super::motion::PAGE_DURATION)
                                    .with_easing(gpui_kit::ease_out_quint()),
                                |element, progress| element.opacity(0.6 + 0.4 * progress),
                            ),
                    )
                    .when(problem, |body| body.child(self.status_banner())),
            )
            .when(self.appearance, |root| {
                root.child(self.appearance_panel(cx))
            })
    }
}
