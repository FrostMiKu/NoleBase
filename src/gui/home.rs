//! Home collection and selection state. Document paths remain stable across sorting.
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Collection {
    #[default]
    Notes,
    Archives,
}
impl Collection {
    pub fn index(self) -> usize {
        match self {
            Self::Notes => 0,
            Self::Archives => 1,
        }
    }
}

#[derive(Default)]
pub struct Home {
    pub collection: Collection,
    pub managing: bool,
    pub selected: HashSet<String>,
    anchor: Option<String>,
}

impl Home {
    pub fn switch(&mut self, collection: Collection) {
        self.collection = collection;
        self.finish_selection();
    }
    pub fn finish_selection(&mut self) {
        self.managing = false;
        self.selected.clear();
        self.anchor = None;
    }
    pub fn toggle(&mut self, path: &str, visible: &[String], extend: bool) {
        self.managing = true;
        if extend {
            if let Some((start, end)) = self
                .anchor
                .as_ref()
                .and_then(|anchor| visible.iter().position(|p| p == anchor))
                .zip(visible.iter().position(|p| p == path))
            {
                self.selected
                    .extend(visible[start.min(end)..=start.max(end)].iter().cloned());
                return;
            }
        }
        if !self.selected.remove(path) {
            self.selected.insert(path.to_owned());
        }
        self.anchor = Some(path.to_owned());
    }
}

/// Adapted from quiet-md's MIT-licensed `render_home_grid` sizing algorithm.
/// Copyright (c) 2026 s1dashu. See assets/licenses/quiet-md.txt.
#[derive(Clone, Copy)]
pub struct GridLayout {
    pub columns: u16,
    pub card_height: f32,
    pub gap: f32,
}
impl GridLayout {
    pub fn new(main_width: f32) -> Self {
        let available = (main_width - 48.).max(1.);
        let gap = 16.;
        let wide_columns = ((available + gap) / (200. + gap)).floor().max(1.) as u16;
        let columns = if wide_columns >= 4 {
            ((available + gap) / (240. + gap)).floor().max(4.) as u16
        } else {
            ((available + gap) / (160. + gap)).floor().clamp(1., 3.) as u16
        };
        let card_height =
            ((available - gap * f32::from(columns - 1)) / f32::from(columns)) * (264. / 236.);
        Self {
            columns,
            card_height,
            gap,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multi_selection_extends_from_its_anchor_and_resets_on_collection_change() {
        let paths = ["a", "b", "c", "d"].map(str::to_owned);
        let mut home = Home::default();
        home.toggle("b", &paths, false);
        home.toggle("d", &paths, true);
        assert_eq!(home.selected, ["b", "c", "d"].map(str::to_owned).into());
        home.toggle("c", &paths, false);
        assert_eq!(home.selected, ["b", "d"].map(str::to_owned).into());
        home.switch(Collection::Archives);
        assert!(!home.managing && home.selected.is_empty());
    }
}
