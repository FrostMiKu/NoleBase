//! Shared task appearance for Markdown and the ToDo page.
use crate::tui::theme::Theme;
use ratatui::{
    style::{Modifier, Style},
    text::Span,
};

pub(crate) fn marker(checked: bool, selected: bool, theme: Theme) -> Span<'static> {
    let style = if selected {
        Style::default()
            .fg(theme.selection_foreground)
            .add_modifier(Modifier::BOLD)
    } else if checked {
        Style::default()
            .fg(theme.ui_task_done)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.ui_task_open)
    };
    Span::styled(if checked { "[x]" } else { "[ ]" }, style)
}

pub(crate) fn text_style(checked: bool, selected: bool, theme: Theme) -> Style {
    let style = if checked {
        Style::default().add_modifier(Modifier::CROSSED_OUT)
    } else {
        Style::default()
    };
    if selected {
        style
            .fg(theme.selection_foreground)
            .bg(theme.selection_background)
    } else {
        style
    }
}
