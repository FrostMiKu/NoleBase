use super::*;
use gpui_kit::component::toolbar::Toolbar;

impl DesktopView {
    fn toolbar(&self, window: &Window, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let title = match self.page {
            Page::Home => match self.home.collection {
                Collection::Notes => "Notes".to_owned(),
                Collection::Archives => "Archives".to_owned(),
            },
            Page::Daily => "日常记录".to_owned(),
            Page::Tags => self
                .active_tag
                .as_ref()
                .map(|tag| format!("# {tag}"))
                .unwrap_or_else(|| "Tags".into()),
            Page::Search => "搜索".to_owned(),
            Page::Todo => "Todo".to_owned(),
            Page::Agent => self
                .agent
                .as_ref()
                .map(|agent| agent.current.summary().title)
                .unwrap_or_else(|| "Agents".into()),
            Page::Document => {
                if self
                    .model
                    .selected
                    .as_ref()
                    .is_some_and(|path| path.starts_with("archives/"))
                {
                    "Archives".into()
                } else {
                    "Notes".into()
                }
            }
        };
        let search_field = self.page == Page::Search
            || (self.page == Page::Home && self.content_width(window) >= 640.);
        let mut toolbar = Toolbar::new("workspace-actions")
            .w_full()
            .small()
            .p_0()
            .when(!self.sidebar, |bar| {
                bar.child(
                    self.icon_button("expand-sidebar", "nole/sidebar", "展开文件树")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                )
            })
            .content(
                div()
                    .debug_selector(|| "page-title".into())
                    .flex_1()
                    .min_w(px(64.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(14.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(title),
            );
        if search_field {
            toolbar = toolbar.content(
                div().w(px(260.)).min_w(px(120.)).child(
                    Input::new(&self.search)
                        .prefix(icon("nole/search"))
                        .small()
                        .aria_label("搜索知识库"),
                ),
            );
        } else {
            toolbar = toolbar.child(
                self.icon_button("toolbar-search", "nole/search", "搜索")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_page(Page::Search, window, cx)),
                    ),
            );
        }
        toolbar = match self.page {
            Page::Home => toolbar.content(self.home_toolbar(cx)).child(
                self.button("new-note", "新建笔记")
                    .icon(icon("nole/plus"))
                    .on_click(cx.listener(|this, _, window, cx| this.begin_new_note(window, cx))),
            ),
            Page::Agent => toolbar.child(
                self.icon_button("new-session", "nole/plus", "新会话")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.new_agent_session(window, cx)),
                    ),
            ),
            Page::Document => toolbar
                .content(
                    div()
                        .text_size(px(11.))
                        .text_color(color(t.text_muted))
                        .child(if self.save_pending {
                            "编辑中…"
                        } else if self.status == "已保存" {
                            "已自动保存"
                        } else {
                            ""
                        }),
                )
                .child(
                    self.icon_button(
                        "preview",
                        if self.preview {
                            "nole/edit"
                        } else {
                            "nole/read"
                        },
                        "切换阅读与编辑",
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_preview(window, cx))),
                ),
            _ => toolbar,
        };
        div()
            .debug_selector(|| "workspace-toolbar".into())
            .w_full()
            .h(px(TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .px(px(PAGE_INSET))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(color(t.ui_border_subtle).opacity(0.35))
            .child(toolbar)
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
        div()
            .relative()
            .key_context("Desktop")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .bg(color(t.surface_canvas))
            .text_color(color(t.text_primary))
            .text_size(px(13.))
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
                            .px(px(PAGE_INSET))
                            .py(px(PAGE_INSET))
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
                    .when(
                        !self.new_note_open && !self.status.is_empty() && self.status != "已保存",
                        |body| {
                            body.child(
                                div()
                                    .w_full()
                                    .px(px(PAGE_INSET))
                                    .py(px(10.))
                                    .text_size(px(12.))
                                    .text_color(color(t.ui_error))
                                    .child(self.status.clone()),
                            )
                        },
                    ),
            )
            .when(self.appearance, |root| {
                root.child(self.appearance_panel(cx))
            })
    }
}
