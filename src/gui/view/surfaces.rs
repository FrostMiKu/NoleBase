use super::widgets::{daily_label, relative_time};
use super::*;
use crate::gui::presentation::{
    BODY_SIZE, RADIUS_LG, RADIUS_MD, RADIUS_SM, READING_WIDTH, SPACE_2XL, SPACE_LG, SPACE_MD,
    SPACE_SM, SPACE_XL, SPACE_XS, TEXT_META, TEXT_NAV, TEXT_UI,
};
use gpui_kit::TestSupportExt;

impl DesktopView {
    pub(super) fn home_surface(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
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
        .track_scroll(&self.home_scroll[collection]);
        let empty =
            if self.home.collection == Collection::Notes {
                self.empty_state(
                    "home-empty",
                    "nole/note",
                    "从第一篇笔记开始",
                    "笔记是长期保存的 Markdown 文件，可以用 [[双链]] 与 #标签 互相关联。",
                )
                .child(div().mt(px(SPACE_SM)).child(
                    self.primary_button("empty-new", "新建笔记").on_click(
                        cx.listener(|this, _, window, cx| this.begin_new_note(window, cx)),
                    ),
                ))
            } else {
                self.empty_state(
                    "home-empty",
                    "nole/archive",
                    "暂无归档",
                    "归档的笔记会保存在这里，随时可以恢复。",
                )
            };
        div()
            .id("home-scroll")
            .debug_selector(|| "home-scroll".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(count == 0, |surface| surface.child(empty))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .pt(px(LIST_TOP))
                    .child(grid.size_full())
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
        let serif = crate::gui::presentation::fonts(cx).serif;
        let hover_border = color(if selected { t.ui_action } else { t.ui_border });
        let mut meta = div()
            .debug_selector(move || format!("card-meta-{index}"))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(SPACE_SM))
            .overflow_hidden()
            .text_size(px(TEXT_META))
            .text_color(color(t.text_muted))
            .child(relative_time(doc.modified, std::time::SystemTime::now()));
        for tag in doc.tags.iter().take(2) {
            meta = meta.child(self.tag_pill(tag));
        }
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
            .p(px(SPACE_LG + 2.))
            .rounded(px(RADIUS_LG))
            .border_1()
            .border_color(color(if selected {
                t.ui_action
            } else {
                t.ui_border_subtle
            }))
            .bg(color(if selected {
                t.selection_background
            } else {
                t.surface_raised
            }))
            .cursor_pointer()
            .relative()
            .flex()
            .flex_col()
            .gap(px(SPACE_SM))
            .overflow_hidden()
            .hover(move |card| card.border_color(hover_border).shadow_sm())
            .child(
                div()
                    .debug_selector(move || format!("card-title-{index}"))
                    .w_full()
                    .flex_shrink_0()
                    .pr(px(if self.home.managing { 28. } else { 0. }))
                    .font_family(serif)
                    .text_size(px(BODY_SIZE))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .line_height(px(24.))
                    .text_color(color(t.text_primary))
                    .line_clamp(2)
                    .text_ellipsis()
                    .child(doc.title.clone()),
            )
            .child(
                div()
                    .debug_selector(move || format!("card-summary-{index}"))
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .text_size(px(TEXT_NAV))
                    .line_height(px(21.))
                    .text_color(color(t.text_secondary))
                    .line_clamp(4)
                    .text_ellipsis()
                    .child(doc.summary.clone()),
            )
            .child(meta)
            .when(self.home.managing, |card| {
                card.child(
                    div()
                        .absolute()
                        .right(px(SPACE_MD))
                        .top(px(SPACE_MD))
                        .size(px(20.))
                        .rounded(px(RADIUS_SM + 1.))
                        .border_1()
                        .border_color(color(if selected { t.ui_action } else { t.ui_border }))
                        .bg(color(if selected {
                            t.ui_action
                        } else {
                            t.surface_raised
                        }))
                        .text_color(color(t.text_on_accent))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(selected, |check| {
                            check.child(icon("nole/check").size(px(13.)))
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
        let today = chrono::Local::now().date_naive();
        let serif = crate::gui::presentation::fonts(cx).serif;
        let stream = gpui_kit::list(self.daily_list.clone(), move |index, window, cx| {
            view.update(cx, |view, cx| {
                let block = &view.model.daily_blocks[index];
                let content = match block.date {
                    Some(date) => div()
                        .pt(px(if index == 0 { 0. } else { SPACE_2XL }))
                        .pb(px(SPACE_LG))
                        .flex()
                        .items_center()
                        .gap(px(SPACE_MD))
                        .child(
                            div()
                                .id(gpui_kit::SharedString::from(format!("daily-date-{date}")))
                                .debug_selector(move || format!("daily-date-{date}"))
                                .test_support()
                                .role(gpui_kit::Role::Button)
                                .aria_label(daily_label(date, today))
                                .cursor_pointer()
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.open_daily_document(date, window, cx);
                                }))
                                .flex_shrink_0()
                                .font_family(serif.clone())
                                .text_size(px(TEXT_UI + 1.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(color(t.text_primary))
                                .child(daily_label(date, today)),
                        )
                        .child(div().flex_1().h(px(1.)).bg(color(t.ui_border_subtle)))
                        .into_any_element(),
                    None => div()
                        .pb(px(SPACE_MD))
                        .child(view.markdown(block.id.clone(), &block.text, window, cx))
                        .into_any_element(),
                };
                reading_row(content).into_any_element()
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
                    .when(self.model.daily_blocks.is_empty(), |body| {
                        body.child(self.empty_state(
                            "daily-empty",
                            "nole/daily",
                            "今天想记下什么？",
                            "随手写下想法、会议要点或待办，Enter 保存到今天的日常记录。",
                        ))
                    })
                    .child(stream)
                    .child(self.main_scrollbar(&self.daily_list, 0.)),
            )
            .child(
                reading_row(
                    div().pt(px(SPACE_MD)).child(
                        self.composer(
                            "daily-composer",
                            super::persistence::Compose::Daily,
                            96.,
                            "Enter 记录 · Shift+Enter 换行",
                            self.primary_button("capture", "记录")
                                .child(icon("nole/arrow").size(px(14.)))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.save(window, cx);
                                })),
                            cx,
                        ),
                    ),
                )
                .flex_shrink_0(),
            )
    }

    /// The raised writing card shared by Daily capture and the Agent prompt.
    pub(super) fn composer(
        &self,
        id: &'static str,
        kind: super::persistence::Compose,
        height: f32,
        hint: &'static str,
        action: Button,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        div()
            .debug_selector(|| id.into())
            .w_full()
            .rounded(px(RADIUS_LG))
            .border_1()
            .border_color(color(t.ui_border))
            .bg(color(t.surface_raised))
            .shadow_sm()
            .px(px(SPACE_LG))
            .pt(px(SPACE_MD))
            .pb(px(SPACE_MD - 2.))
            .child(self.compose_input(kind, height, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mt(px(SPACE_SM))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_SM))
                            .when(matches!(kind, super::persistence::Compose::Agent), |row| {
                                row.child(self.agent_permission_control(cx))
                            })
                            .child(
                                div()
                                    .text_size(px(TEXT_META))
                                    .text_color(color(t.text_muted))
                                    .child(hint),
                            ),
                    )
                    .child(action),
            )
    }

    pub(super) fn appearance_panel(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        div()
            .debug_selector(|| "appearance-panel".into())
            .absolute()
            .left(px(SPACE_MD))
            .bottom(px(52.))
            .w(px(260.))
            .p(px(SPACE_LG))
            .rounded(px(RADIUS_LG))
            .shadow_lg()
            .border_1()
            .border_color(color(t.ui_border_subtle))
            .bg(color(t.surface_overlay))
            .flex()
            .flex_col()
            .gap(px(SPACE_LG))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(TEXT_UI))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("外观")
                    .child(
                        self.icon_button("close-appearance", "nole/close", "关闭")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.appearance = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .h(px(56.))
                    .rounded(px(RADIUS_MD))
                    .border_1()
                    .border_color(color(t.ui_border_subtle))
                    .bg(color(t.surface_canvas))
                    .p(px(SPACE_SM + 2.))
                    .flex()
                    .flex_col()
                    .justify_between()
                    .child(
                        div().flex().gap(px(SPACE_XS)).children(
                            [
                                t.ui_action,
                                t.ui_action_ai,
                                t.ui_success,
                                t.ui_warning,
                                t.ui_error,
                            ]
                            .map(|tone| div().size(px(14.)).rounded(px(7.)).bg(color(tone))),
                        ),
                    )
                    .child(
                        div().flex().gap(px(SPACE_XS)).children(
                            [t.text_primary, t.text_secondary, t.text_muted].map(|tone| {
                                div().h(px(4.)).w(px(48.)).rounded(px(2.)).bg(color(tone))
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(TEXT_META))
                                    .text_color(color(t.text_muted))
                                    .child("当前主题"),
                            )
                            .child(
                                div()
                                    .text_size(px(TEXT_NAV))
                                    .text_color(color(t.text_primary))
                                    .child(self.model.theme.active.clone()),
                            ),
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

/// Center `content` on the shared reading measure.
pub(super) fn reading_row(content: impl IntoElement) -> gpui_kit::Div {
    div().w_full().flex().justify_center().child(
        div()
            .w_full()
            .max_w(px(READING_WIDTH + SPACE_XL * 2.))
            .px(px(SPACE_XL))
            .child(content),
    )
}
