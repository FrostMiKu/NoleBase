//! A compact completion window shared by the desktop writing surfaces.
use super::*;
use crate::core::{
    completion::{
        byte_offset, filter_wiki_completions, open_wiki_span, selection_viewport_start, stem_label,
    },
    model::{WikiLinkCandidate, WikiLinkLocation},
};
use crate::gui::presentation::{RADIUS_MD, SPACE_SM, TEXT_NAV};
use gpui_kit::{EntityInputHandler, Focusable};
use std::cell::Cell;

const WINDOW: usize = 8;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Owner {
    Editor,
    Agent,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Wiki,
    Tag,
}
#[derive(Clone)]
pub(super) struct Completion {
    owner: Owner,
    kind: Kind,
    start: usize,
    end: usize,
    query: String,
    candidates: Vec<String>,
    pub selected: usize,
    pub scroll: Cell<usize>,
}

impl DesktopView {
    fn completion_input(&self, owner: Owner) -> Entity<TextareaState> {
        match owner {
            Owner::Editor => self.editor.clone(),
            Owner::Agent => self.agent_input.clone(),
        }
    }

    pub(super) fn refresh_completion(
        &mut self,
        owner: Owner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.completion_input(owner);
        if !input.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        if input.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            if self.completion.take().is_some() {
                cx.notify();
            }
            return;
        }
        if !input.read(cx).selected_range().is_empty() {
            if self.completion.take().is_some() {
                cx.notify();
            }
            return;
        }
        let text = input.read(cx).value();
        let cursor = input.read(cx).cursor().min(text.len());
        let before = &text[..cursor];
        let span = open_wiki_span(&text, before.chars().count())
            .map(|span| {
                let start = byte_offset(&text, span.start);
                (Kind::Wiki, start, &text[start + 2..cursor])
            })
            .or_else(|| {
                let start = before.rfind('#')?;
                let boundary = start == 0 || before[..start].chars().next_back()?.is_whitespace();
                let query = &before[start + 1..];
                (boundary
                    && query
                        .chars()
                        .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '/')))
                .then_some((Kind::Tag, start, query))
            });
        let Some((kind, start, query)) = span else {
            let changed = self.completion.take().is_some();
            self.completion_dismissed = None;
            if changed {
                cx.notify();
            }
            return;
        };
        let literal = text[..start]
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'\\')
            .count()
            % 2
            == 1
            || crate::core::markup::parse(&text).iter().any(|item| {
                matches!(
                    &item.event,
                    crate::core::markup::Event::Markdown(
                        pulldown_cmark::Event::Code(_)
                            | pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(_))
                            | pulldown_cmark::Event::Html(_)
                            | pulldown_cmark::Event::InlineHtml(_)
                    )
                ) && item.span.contains(&start)
            });
        if literal {
            if self.completion.take().is_some() {
                cx.notify();
            }
            return;
        }
        if self.completion_dismissed.as_ref() == Some(&(owner, start, query.to_owned())) {
            return;
        }
        self.completion_dismissed = None;
        if self.completion.as_ref().is_some_and(|state| {
            state.owner == owner
                && state.kind == kind
                && state.start == start
                && state.end == cursor
                && state.query == query
        }) {
            return;
        }
        let mut candidates = match kind {
            Kind::Wiki => {
                let mut notes = self
                    .model
                    .documents
                    .iter()
                    .map(|note| WikiLinkCandidate {
                        path: note.path.clone().into(),
                        location: if note.archived {
                            WikiLinkLocation::Archives
                        } else {
                            WikiLinkLocation::Notes
                        },
                    })
                    .collect::<Vec<_>>();
                notes.extend(self.model.daily_notes.iter().map(|note| WikiLinkCandidate {
                    path: crate::core::storage::Storage::daily_file_name(note.date),
                    location: WikiLinkLocation::Daily,
                }));
                filter_wiki_completions(&notes, query)
                    .iter()
                    .filter_map(stem_label)
                    .collect::<Vec<_>>()
            }
            Kind::Tag => {
                let query = query.to_lowercase();
                let mut tags = self
                    .model
                    .tags
                    .iter()
                    .filter(|tag| tag.name.to_lowercase().contains(&query))
                    .map(|tag| tag.name.clone())
                    .collect::<Vec<_>>();
                tags.sort_by_key(|tag| (!tag.to_lowercase().starts_with(&query), tag.clone()));
                tags
            }
        };
        candidates.dedup();
        let changed = self.completion.is_some() || !candidates.is_empty();
        self.completion = (!candidates.is_empty()).then_some(Completion {
            owner,
            kind,
            start,
            end: cursor,
            query: query.into(),
            candidates,
            selected: 0,
            scroll: Cell::new(0),
        });
        if changed {
            cx.notify();
        }
    }

    pub(super) fn completion_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(state) = &self.completion else {
            return false;
        };
        let input = self.completion_input(state.owner);
        if !input.read(cx).focus_handle(cx).is_focused(window) {
            return false;
        }
        if input.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            return false;
        }
        match key {
            "up" => {
                let state = self.completion.as_mut().unwrap();
                state.selected = state.selected.saturating_sub(1);
            }
            "down" => {
                let state = self.completion.as_mut().unwrap();
                state.selected = (state.selected + 1).min(state.candidates.len() - 1);
            }
            "enter" | "tab" => self.accept_completion(window, cx),
            "escape" => {
                let state = self.completion.take().unwrap();
                self.completion_dismissed = Some((state.owner, state.start, state.query));
            }
            _ => return false,
        }
        cx.stop_propagation();
        cx.notify();
        true
    }

    pub(super) fn refresh_focused_completion(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for owner in [Owner::Editor, Owner::Agent] {
            if self
                .completion_input(owner)
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
            {
                self.refresh_completion(owner, window, cx);
                return;
            }
        }
        self.completion = None;
        cx.notify();
    }

    fn accept_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.completion.take() else {
            return;
        };
        let input = self.completion_input(state.owner);
        let text = input.read(cx).value();
        let mut end = state.end;
        let label = &state.candidates[state.selected];
        let value = match state.kind {
            Kind::Wiki => {
                if text[end..].starts_with("]]") {
                    end += 2;
                }
                format!("[[{label}]]")
            }
            Kind::Tag => format!("#{label} "),
        };
        let range = text[..state.start].encode_utf16().count()..text[..end].encode_utf16().count();
        input.update(cx, |input, cx| {
            input.replace_text_in_range(Some(range), &value, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn completion_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let state = self.completion.as_ref()?;
        let input = self.completion_input(state.owner);
        if !input.read(cx).focus_handle(cx).is_focused(window) {
            return None;
        }
        let cursor = input.read(cx).cursor_layout()?.0;
        let start = selection_viewport_start(
            state.scroll.get(),
            state.selected,
            WINDOW,
            state.candidates.len(),
        );
        state.scroll.set(start);
        let t = self.model.theme.theme;
        let popup = div()
            .debug_selector(|| "completion-popup".into())
            .w(px(300.).min(window.viewport_size().width - px(32.)))
            .py(px(4.))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(color(t.ui_border_subtle))
            .bg(color(t.surface_overlay))
            .shadow_lg()
            .text_size(px(TEXT_NAV))
            .children(
                state
                    .candidates
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(WINDOW)
                    .map(|(index, label)| {
                        let selected = index == state.selected;
                        div()
                            .id(("completion-option", index))
                            .debug_selector(move || format!("completion-option-{index}"))
                            .h(px(30.))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .when(selected, |row| row.bg(color(t.selection_background)))
                            .child(
                                div()
                                    .debug_selector(move || {
                                        if selected {
                                            "completion-selection".into()
                                        } else {
                                            format!("completion-gutter-{index}")
                                        }
                                    })
                                    .w(px(2.))
                                    .h_full()
                                    .when(selected, |mark| mark.bg(color(t.selection_indicator))),
                            )
                            .child(
                                div()
                                    .debug_selector(move || format!("completion-label-{index}"))
                                    .px(px(SPACE_SM))
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(color(t.text_primary))
                                    .child(label.clone()),
                            )
                            .on_mouse_down(
                                gpui_kit::MouseButton::Left,
                                cx.listener(move |view, _, window, cx| {
                                    if let Some(state) = &mut view.completion {
                                        state.selected = index;
                                    }
                                    view.accept_completion(window, cx);
                                    cx.stop_propagation();
                                }),
                            )
                    }),
            );
        Some(
            gpui_kit::deferred(
                gpui_kit::anchored()
                    .position(gpui_kit::point(cursor.left(), cursor.top() - px(4.)))
                    .anchor(gpui_kit::Anchor::BottomLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(popup),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}
