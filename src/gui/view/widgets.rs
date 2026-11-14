//! Shared pieces of the desktop's visual language: buttons, labels, pills,
//! empty states, the status banner and the date wording, so every page draws
//! them the same way.
use super::*;
use crate::gui::presentation::{
    RADIUS_MD, RADIUS_PILL, SPACE_LG, SPACE_MD, SPACE_SM, SPACE_XS, TEXT_LABEL, TEXT_META,
    TEXT_NAV, TEXT_UI,
};
use chrono::{Datelike, Local, NaiveDate};
use std::time::SystemTime;

impl DesktopView {
    /// The filled call to action of a surface: create, send, record, approve.
    pub(super) fn primary_button(
        &self,
        id: &'static str,
        label: impl Into<gpui_kit::SharedString>,
    ) -> Button {
        let label = label.into();
        Button::new(id)
            .primary()
            .small()
            .debug_selector(|| id.to_owned())
            .accessibility_label(label.clone())
            .child(div().debug_selector(|| format!("{id}-label")).child(label))
    }

    /// A small uppercase-style heading over a group of rows.
    pub(super) fn section_label(&self, text: impl Into<gpui_kit::SharedString>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        div()
            .px(px(SPACE_SM + 2.))
            .pt(px(SPACE_LG + SPACE_XS))
            .pb(px(SPACE_SM))
            .text_size(px(TEXT_LABEL))
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(color(t.text_muted))
            .child(text.into())
    }

    /// A tag reference in accent ink on its soft wash.
    pub(super) fn tag_pill(&self, tag: &str) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        div()
            .flex_shrink_0()
            .px(px(SPACE_SM + 2.))
            .py(px(1.))
            .rounded(px(RADIUS_PILL))
            .bg(color(t.selection_background))
            .text_color(color(t.markdown_hashtag))
            .text_size(px(TEXT_META))
            .child(format!("#{tag}"))
    }

    /// Background of a list row under the pointer.
    pub(super) fn row_hover(&self) -> gpui_kit::Hsla {
        color(self.model.theme.theme.selection_background_inactive)
    }

    /// A centered placeholder for a surface with nothing to show yet.
    pub(super) fn empty_state(
        &self,
        id: &'static str,
        glyph: &'static str,
        title: &'static str,
        hint: &'static str,
    ) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        div()
            .debug_selector(|| id.into())
            .w_full()
            .py(px(SPACE_LG * 4.))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SPACE_SM))
            .child(
                div()
                    .size(px(48.))
                    .mb(px(SPACE_XS))
                    .rounded(px(RADIUS_PILL))
                    .bg(color(t.surface_sunken))
                    .text_color(color(t.text_muted))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(glyph).size(px(22.))),
            )
            .child(
                div()
                    .text_size(px(TEXT_UI))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(color(t.text_primary))
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(360.))
                    .text_size(px(TEXT_NAV))
                    .text_color(color(t.text_muted))
                    .text_center()
                    .child(hint),
            )
    }

    /// The problem the last action ran into, shown under the page.
    pub(super) fn status_banner(&self) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let error = color(t.ui_error);
        div()
            .debug_selector(|| "status-banner".into())
            .mx(px(PAGE_INSET))
            .mb(px(SPACE_MD))
            .px(px(SPACE_MD))
            .py(px(SPACE_SM))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(error.opacity(0.35))
            .bg(error.opacity(0.08))
            .flex()
            .items_center()
            .gap(px(SPACE_SM))
            .text_size(px(TEXT_NAV))
            .text_color(error)
            .child(icon("nole/alert").size(px(16.)))
            .child(div().flex_1().min_w_0().child(self.status.clone()))
    }
}

/// When a note last changed, worded relative to `now`.
pub(super) fn relative_time(modified: SystemTime, now: SystemTime) -> String {
    let seconds = now
        .duration_since(modified)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    match seconds {
        0..60 => "刚刚".into(),
        60..3_600 => format!("{} 分钟前", seconds / 60),
        3_600..86_400 => format!("{} 小时前", seconds / 3_600),
        86_400..604_800 => format!("{} 天前", seconds / 86_400),
        _ => {
            let date = chrono::DateTime::<Local>::from(modified).date_naive();
            let today = chrono::DateTime::<Local>::from(now).date_naive();
            calendar_date(date, today)
        }
    }
}

/// `11 月 14 日`, with the year when it differs from `today`'s.
pub(super) fn calendar_date(date: NaiveDate, today: NaiveDate) -> String {
    if date.year() == today.year() {
        format!("{} 月 {} 日", date.month(), date.day())
    } else {
        format!("{} 年 {} 月 {} 日", date.year(), date.month(), date.day())
    }
}

/// The heading of one day in Daily: `今天 · 11 月 14 日 星期六`.
pub(super) fn daily_label(date: NaiveDate, today: NaiveDate) -> String {
    const WEEKDAYS: [&str; 7] = [
        "星期一",
        "星期二",
        "星期三",
        "星期四",
        "星期五",
        "星期六",
        "星期日",
    ];
    let weekday = WEEKDAYS[date.weekday().num_days_from_monday() as usize];
    let day = calendar_date(date, today);
    match (today - date).num_days() {
        0 => format!("今天 · {day} {weekday}"),
        1 => format!("昨天 · {day} {weekday}"),
        _ => format!("{day} {weekday}"),
    }
}

/// Shared running indicator for transcript activity and task widgets.
pub(super) fn spinning_icon(
    id: impl Into<gpui_kit::SharedString>,
    tone: gpui_kit::Hsla,
) -> gpui_kit::AnyElement {
    icon("nole/spinner")
        .size(px(14.))
        .text_color(tone)
        .with_animation(
            id.into(),
            Animation::new(Duration::from_millis(1200)).repeat(),
            |icon, delta| icon.rotate(gpui_kit::radians(delta * std::f32::consts::TAU)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn relative_time_counts_up_to_a_week_then_names_the_date() {
        let now = SystemTime::now();
        assert_eq!(relative_time(now, now), "刚刚");
        assert_eq!(
            relative_time(now - Duration::from_secs(5 * 60), now),
            "5 分钟前"
        );
        assert_eq!(
            relative_time(now - Duration::from_secs(2 * 3_600), now),
            "2 小时前"
        );
        assert_eq!(
            relative_time(now - Duration::from_secs(3 * 86_400), now),
            "3 天前"
        );
        assert!(relative_time(now - Duration::from_secs(30 * 86_400), now).ends_with(" 日"));
        assert_eq!(relative_time(now + Duration::from_secs(60), now), "刚刚");
    }

    #[test]
    fn daily_labels_name_recent_days_and_distant_years() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        assert_eq!(daily_label(today, today), "今天 · 11 月 14 日 星期六");
        assert_eq!(
            daily_label(today.pred_opt().unwrap(), today),
            "昨天 · 11 月 13 日 星期五"
        );
        assert_eq!(
            daily_label(NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(), today),
            "2025 年 1 月 2 日 星期四"
        );
    }
}
