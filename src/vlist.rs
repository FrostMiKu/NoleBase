//! Variable-height virtual list geometry.

use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VList {
    estimated_height: usize,
    /// Per-index estimate; defaults to [`Self::estimated_height`] on resize.
    estimates: Vec<usize>,
    measured: Vec<Option<usize>>,
    prefix: Vec<usize>,
    dirty: bool,
}

impl VList {
    pub fn new(estimated_height: usize) -> Self {
        Self {
            estimated_height: estimated_height.max(1),
            estimates: Vec::new(),
            measured: Vec::new(),
            prefix: vec![0],
            dirty: false,
        }
    }

    pub fn resize(&mut self, len: usize) {
        if self.measured.len() != len {
            self.measured.resize(len, None);
            self.estimates.resize(len, self.estimated_height);
            self.dirty = true;
        }
    }

    /// Sets the estimated height for an unmeasured item. Marks the prefix dirty only
    /// when the estimate actually changes, so the per-frame sync pass stays cheap.
    pub fn set_estimate(&mut self, index: usize, height: usize) {
        let height = height.max(1);
        let Some(estimate) = self.estimates.get_mut(index) else {
            return;
        };
        if *estimate != height {
            *estimate = height;
            self.dirty = true;
        }
    }

    pub fn invalidate(&mut self, index: usize) {
        if let Some(height) = self.measured.get_mut(index) {
            if height.take().is_some() {
                self.dirty = true;
            }
        }
    }

    pub fn set_height(&mut self, index: usize, height: usize) -> bool {
        let height = height.max(1);
        let Some(measured) = self.measured.get_mut(index) else {
            return false;
        };
        if *measured == Some(height) {
            return false;
        }
        *measured = Some(height);
        self.dirty = true;
        true
    }

    pub fn height(&self, index: usize) -> usize {
        self.measured
            .get(index)
            .and_then(|height| *height)
            .or_else(|| self.estimates.get(index).copied())
            .unwrap_or(self.estimated_height)
    }

    pub fn is_measured(&self, index: usize) -> bool {
        self.measured.get(index).is_some_and(Option::is_some)
    }

    pub fn total_height(&mut self) -> usize {
        self.rebuild_prefix();
        self.prefix.last().copied().unwrap_or(0)
    }

    pub fn item_top(&mut self, index: usize) -> usize {
        self.rebuild_prefix();
        self.prefix[index.min(self.measured.len())]
    }

    pub fn max_scroll(&mut self, viewport_height: usize) -> usize {
        self.total_height().saturating_sub(viewport_height)
    }

    /// Resolve a persisted scroll request against the current geometry.
    /// `follow_tail` and the `u16::MAX` sentinel both pin the view to the end;
    /// every other request is clamped to the available content range.
    pub fn scroll_offset(
        &mut self,
        requested: u16,
        follow_tail: bool,
        viewport_height: usize,
    ) -> usize {
        let maximum = self.max_scroll(viewport_height);
        if follow_tail || requested == u16::MAX {
            maximum
        } else {
            usize::from(requested).min(maximum)
        }
    }

    pub fn is_at_end(&mut self, scroll: usize, viewport_height: usize) -> bool {
        scroll >= self.max_scroll(viewport_height)
    }

    pub fn visible_range(&mut self, scroll: usize, viewport_height: usize) -> Range<usize> {
        self.rebuild_prefix();
        if self.measured.is_empty() || viewport_height == 0 {
            return 0..0;
        }
        let end_row = scroll.saturating_add(viewport_height);
        let start = self
            .prefix
            .partition_point(|top| *top <= scroll)
            .saturating_sub(1);
        let end = self
            .prefix
            .partition_point(|top| *top < end_row)
            .min(self.measured.len());
        start.min(self.measured.len())..end.max(start).min(self.measured.len())
    }

    fn rebuild_prefix(&mut self) {
        if !self.dirty {
            return;
        }
        self.prefix.clear();
        self.prefix.reserve(self.measured.len() + 1);
        self.prefix.push(0);
        let mut total = 0usize;
        for (index, height) in self.measured.iter().enumerate() {
            let height = height
                .as_ref()
                .copied()
                .or_else(|| self.estimates.get(index).copied())
                .unwrap_or(self.estimated_height);
            total = total.saturating_add(height);
            self.prefix.push(total);
        }
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_height_list_finds_visible_items_without_measuring_the_rest() {
        let mut list = VList::new(10);
        list.resize(100);
        list.set_height(0, 30);
        list.set_height(1, 5);

        assert_eq!(list.visible_range(0, 20), 0..1);
        assert_eq!(list.visible_range(30, 10), 1..3);
        assert!(list.is_measured(0));
        assert!(!list.is_measured(50));
        assert_eq!(list.total_height(), 1_015);
    }

    #[test]
    fn invalidation_restores_the_estimated_height() {
        let mut list = VList::new(8);
        list.resize(2);
        list.set_height(0, 40);
        assert_eq!(list.total_height(), 48);
        list.invalidate(0);
        assert_eq!(list.total_height(), 16);
    }

    #[test]
    fn scroll_offset_resolves_tail_and_clamps_persisted_position() {
        let mut list = VList::new(10);
        list.resize(10);
        assert_eq!(list.scroll_offset(3, false, 20), 3);
        assert_eq!(list.scroll_offset(u16::MAX, false, 20), 80);
        assert_eq!(list.scroll_offset(3, true, 20), 80);
        assert!(list.is_at_end(80, 20));
        assert!(!list.is_at_end(79, 20));
    }
}
