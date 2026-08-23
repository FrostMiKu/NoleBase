//! Agent sidebar section listing background jobs.

use crate::agent::{JobRow, JobStatus};

use super::*;

/// Rows the jobs section reserves when jobs exist (border + one blank spacer).
pub(super) const AGENT_JOBS_RESERVED_ROWS: u16 = 2;

pub(super) fn draw_agent_jobs(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 || area.height < AGENT_JOBS_RESERVED_ROWS {
        return;
    }
    let rows: Vec<JobRow> = app
        .agent_jobs
        .rows()
        .into_iter()
        .filter(|row| row.status == JobStatus::Running)
        .collect();
    if rows.is_empty() {
        return;
    }
    let list_height = (rows.len() as u16).saturating_add(AGENT_JOBS_RESERVED_ROWS);
    let area = Rect {
        height: area.height.min(list_height),
        ..area
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Jobs ")
        .border_style(Style::default().fg(app.theme.ui_border))
        .style(Style::default().bg(app.theme.surface_panel));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let spinner = spinner_frame(app.animation_tick);
    let label_budget = inner.width.saturating_sub(16) as usize;
    let lines: Vec<Line> = rows
        .iter()
        .map(|row| {
            let (marker, style) = match row.status {
                JobStatus::Running => (spinner, Style::default().fg(app.theme.ui_action_ai)),
                JobStatus::Done => ('✓', Style::default().fg(app.theme.ui_task_done)),
                JobStatus::Failed | JobStatus::Cancelled => {
                    ('✗', Style::default().fg(app.theme.ui_error))
                }
            };
            let marker = marker.to_string();
            Line::styled(
                format!(
                    "{marker} {} · {} · {}",
                    row.id,
                    truncate_job_label(&row.label, label_budget),
                    format_job_elapsed(row),
                ),
                style,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn format_job_elapsed(row: &JobRow) -> String {
    let seconds = row.elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    }
}

fn truncate_job_label(label: &str, width: usize) -> String {
    if UnicodeWidthStr::width(label) <= width {
        return label.to_string();
    }
    let mut taken = 0usize;
    let mut cut = None;
    for (index, ch) in label.char_indices() {
        taken += UnicodeWidthChar::width(ch).unwrap_or(0);
        if taken > width.saturating_sub(1) {
            cut = Some(index);
            break;
        }
    }
    match cut {
        Some(0) | None => label.to_string(),
        Some(index) => format!("{}…", &label[..index]),
    }
}
