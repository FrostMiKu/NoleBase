//! Virtual-list and per-card render caches for the daily and agent panels.

use std::sync::Arc;

use chrono::NaiveDate;

use crate::agent_session::AgentPanelEntry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DailyCardRenderCache {
    pub width: usize,
    pub date: NaiveDate,
    pub date_label: String,
    pub body: String,
    pub lines: Vec<ratatui::text::Line<'static>>,
    pub links: Vec<crate::markdown::RenderedLink>,
    pub tags: Vec<crate::markdown::RenderedTag>,
    pub images: Vec<mbtui::ImagePlacement>,
    pub button_line: usize,
    pub button_start: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentEntryRenderCache {
    pub width: usize,
    /// Shared with `App::agent_panel` so the sync pass can compare by `Arc::ptr_eq`
    /// instead of deep-comparing the entry text every frame.
    pub entry: Arc<AgentPanelEntry>,
    pub lines: Vec<ratatui::text::Line<'static>>,
    pub links: Vec<crate::markdown::RenderedLink>,
    pub images: Vec<mbtui::ImagePlacement>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AgentEntryRenderStyle {
    #[default]
    Panel,
    Cards,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DailyVirtualItem {
    pub date: NaiveDate,
    pub cache: Option<DailyCardRenderCache>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DailyVirtualList {
    pub width: usize,
    pub geometry: crate::vlist::VList,
    pub items: Vec<DailyVirtualItem>,
}

impl Default for DailyVirtualList {
    fn default() -> Self {
        Self {
            width: 0,
            geometry: crate::vlist::VList::new(12),
            items: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentVirtualList {
    pub width: usize,
    pub style: AgentEntryRenderStyle,
    pub geometry: crate::vlist::VList,
    pub caches: Vec<Option<AgentEntryRenderCache>>,
}

impl Default for AgentVirtualList {
    fn default() -> Self {
        Self {
            width: 0,
            style: AgentEntryRenderStyle::Panel,
            geometry: crate::vlist::VList::new(4),
            caches: Vec::new(),
        }
    }
}
