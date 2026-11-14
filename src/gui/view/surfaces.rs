use super::*;

impl DesktopView {
    pub(super) fn home_surface(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let t = self.model.theme.theme;
        let available = self.content_width(window);
        let layout = GridLayout::new(available);
        let columns = layout.columns;
        let collection = self.home.collection.index();
        if self.grid_columns != columns {
            self.home_scroll[collection] = UniformListScrollHandle::new();
            self.grid_columns = columns;
        }
        let count = self.home_indices().len();
        let view = cx.entity().downgrade();
        let grid = gpui_kit::uniform_list(
            "home-grid",
            count.div_ceil(usize::from(columns)),
            move |range, _, cx| {
                view.update(cx, |view, cx| {
                    range
                        .map(|row| {
                            let start = row * usize::from(columns);
                            let indices = view.home_indices();
                            div()
                                .w_full()
                                .h(px(layout.card_height + layout.gap))
                                .pb(px(layout.gap))
                                .grid()
                                .grid_cols(columns)
                                .gap(px(layout.gap))
                                .children(
                                    indices
                                        [start..(start + usize::from(columns)).min(indices.len())]
                                        .iter()
                                        .map(|index| {
                                            view.note_card(*index, layout.card_height, cx)
                                        }),
                                )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
            },
        )
        .track_scroll(&self.home_scroll[collection])
        .size_full();
        div()
            .id("home-scroll")
            .debug_selector(|| "home-scroll".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(count == 0, |surface| {
                surface.child(
                    div()
                        .h(px(240.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(12.))
                        .text_color(color(t.text_muted))
                        .child(icon("nole/note").size(px(32.)))
                        .child(if self.home.collection == Collection::Notes {
                            "从第一篇笔记开始"
                        } else {
                            "归档的笔记会显示在这里"
                        })
                        .child(self.button("empty-new", "新建笔记").on_click(
                            cx.listener(|this, _, window, cx| this.begin_new_note(window, cx)),
                        )),
                )
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .pt(px(LIST_TOP))
                    .child(grid)
                    .child(self.main_scrollbar(&self.home_scroll[collection], LIST_TOP)),
            )
    }

    fn note_card(
        &self,
        index: usize,
        card_height: f32,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        #[cfg(test)]
        self.home_rendered.set(self.home_rendered.get() + 1);
        let doc = &self.model.documents[index];
        let t = self.model.theme.theme;
        let selected = self.home.selected.contains(&doc.path);
        let path = doc.path.clone();
        div()
            .id(gpui_kit::SharedString::from(path.clone()))
            .when(index == 0, |card| {
                card.debug_selector(|| "first-card".into())
            })
            .when(index == 1, |card| {
                card.debug_selector(|| "second-card".into())
            })
            .w_full()
            .min_w_0()
            .h(px(card_height))
            .p(px(super::super::presentation::CARD_FRAME))
            .rounded(px(18.))
            .bg(color(t.surface_panel))
            .cursor_pointer()
            .relative()
            .when(selected, |card| {
                card.bg(color(t.selection_indicator).opacity(0.55))
            })
            .flex()
            .flex_col()
            .overflow_hidden()
            .hover(move |card| {
                card.bg(color(if selected {
                    t.selection_indicator
                } else {
                    t.ui_border_subtle
                })
                .opacity(0.65))
            })
            .child(
                div()
                    .debug_selector(move || format!("card-page-{index}").into())
                    .size_full()
                    .min_w_0()
                    .p(px(super::super::presentation::CARD_INSET))
                    .rounded(px(14.))
                    .bg(color(t.surface_canvas))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(
                        div()
                            .debug_selector(move || format!("card-title-{index}").into())
                            .w_full()
                            .flex_shrink_0()
                            .text_size(px(14.))
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .line_height(px(20.))
                            .mb(px(8.))
                            .child(doc.title.clone()),
                    )
                    .child(self.note_preview(&doc.preview)),
            )
            .when(self.home.managing, |card| {
                card.child(
                    div()
                        .absolute()
                        .right(px(12.))
                        .bottom(px(12.))
                        .size(px(20.))
                        .rounded(px(5.))
                        .border_1()
                        .border_color(color(t.ui_border_subtle))
                        .bg(color(t.surface_canvas))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(12.))
                        .when(selected, |check| {
                            check
                                .bg(color(t.selection_indicator))
                                .text_color(color(t.text_on_accent))
                                .child("✓")
                        }),
                )
            })
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    this.click_note(&path, event.modifiers(), window, cx)
                }),
            )
    }

    pub(super) fn daily_surface(
        &self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let t = self.model.theme.theme;
        let view = cx.entity().downgrade();
        let stream = gpui_kit::list(self.daily_list.clone(), move |index, window, cx| {
            view.update(cx, |view, cx| {
                let block = &view.model.daily_blocks[index];
                let row = div().w_full();
                match &block.date {
                    Some(date) => row
                        .pt(px(if index == 0 { 0. } else { 24. }))
                        .pb(px(16.))
                        .text_size(px(12.))
                        .text_color(color(t.text_muted))
                        .child(date.clone())
                        .into_any_element(),
                    None => row
                        .pb(px(12.))
                        .text_size(px(15.))
                        .line_height(gpui_kit::relative(1.65))
                        .child(crate::gui::rich_text::markdown(
                            block.id.clone(),
                            block.text.clone(),
                            false,
                            t,
                            window,
                            cx,
                        ))
                        .into_any_element(),
                }
            })
            .unwrap_or_else(|_| div().into_any_element())
        })
        .size_full();
        div()
            .id("daily-surface")
            .debug_selector(|| "daily-surface".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(stream)
                    .child(self.main_scrollbar(&self.daily_list, 0.)),
            )
            .child(
                div().w_full().pt(px(12.)).flex_shrink_0().child(
                    div()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(color(t.ui_border_subtle).opacity(0.65))
                        .bg(color(t.surface_message_agent))
                        .p(px(14.))
                        .child(self.compose_input(super::persistence::Compose::Daily, 96., cx))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .mt(px(8.))
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(color(t.text_muted))
                                        .child("Enter 记录 · Shift+Enter 换行"),
                                )
                                .child(
                                    self.button("capture", "记录")
                                        .bg(color(t.ui_action))
                                        .text_color(color(t.text_on_accent))
                                        .child(icon("nole/arrow").size(px(14.)))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.save(window, cx);
                                        })),
                                ),
                        ),
                ),
            )
    }

    pub(super) fn document_surface(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let t = self.model.theme.theme;
        let draft = self.draft.clone();
        let inset =
            super::super::presentation::document_inset(self.content_width(window)) + PAGE_INSET;
        if self.preview {
            div()
                .id("document-scroll")
                .debug_selector(|| "document-reading".into())
                .w_full()
                .flex_1()
                .min_h_0()
                .w_full()
                .relative()
                .overflow_hidden()
                .text_size(px(super::super::presentation::BODY_SIZE))
                .line_height(px(super::super::presentation::BODY_LINE))
                .child(
                    div()
                        .id("document-reading-scroll")
                        .debug_selector(|| "document-viewport".into())
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.document_scroll)
                        .px(px(inset))
                        .py(px(PAGE_INSET))
                        .child(
                            div()
                                .debug_selector(|| "document-column".into())
                                .w_full()
                                .child(crate::gui::rich_text::markdown(
                                    "document-preview",
                                    draft,
                                    false,
                                    t,
                                    window,
                                    cx,
                                )),
                        ),
                )
                .child(Scrollbar::vertical(&self.document_scroll))
        } else {
            let scrollbar = self.document_editor_scrollbar(cx);
            div()
                .id("document-editor-surface")
                .w_full()
                .flex_1()
                .min_h_0()
                .w_full()
                .py(px(PAGE_INSET))
                .flex()
                .flex_col()
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .text_size(px(super::super::presentation::BODY_SIZE))
                        .line_height(px(super::super::presentation::BODY_LINE))
                        .child(
                            div().size_full().px(px(inset)).child(
                                div()
                                    .debug_selector(|| "document-editor".into())
                                    .size_full()
                                    .child(Textarea::new(&self.editor).appearance(false).h_full()),
                            ),
                        )
                        .child(scrollbar),
                )
                .child(
                    div()
                        .h(px(22.))
                        .px(px(inset))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_end()
                        .text_size(px(11.))
                        .text_color(color(t.text_muted))
                        .child(format!("{} 字", self.draft_chars)),
                )
        }
    }
    pub(super) fn appearance_panel(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        div()
            .absolute()
            .left(px(12.))
            .bottom(px(52.))
            .w(px(248.))
            .p(px(16.))
            .rounded(px(10.))
            .shadow_lg()
            .border_1()
            .border_color(color(t.ui_border_subtle))
            .bg(color(t.surface_overlay))
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(13.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("外观")
                    .child(
                        self.icon_button("close-appearance", "nole/close", "关闭")
                            .size(px(24.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.appearance = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div().flex().gap(px(6.)).children(
                    t.animation_gradient
                        .into_iter()
                        .map(|tone| div().flex_1().h(px(24.)).rounded(px(4.)).bg(color(tone))),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(color(t.text_secondary))
                            .child(self.model.theme.active.clone()),
                    )
                    .child(
                        self.button("change-theme", "切换主题")
                            .on_click(cx.listener(|this, _, _, cx| {
                                match this.model.cycle_theme() {
                                    Ok(()) => theme::apply(this.model.theme.theme, cx),
                                    Err(error) => this.status = format!("主题遇到问题：{error}"),
                                }
                                cx.notify();
                            })),
                    ),
            )
    }
}
