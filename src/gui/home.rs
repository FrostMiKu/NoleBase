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

/// Summary cards in as many columns as fit at a readable width.
#[derive(Clone, Copy)]
pub struct GridLayout {
    pub columns: u16,
    pub card_height: f32,
    pub gap: f32,
}
impl GridLayout {
    /// The narrowest card that keeps a summary line readable.
    const MIN_CARD_WIDTH: f32 = 240.;
    /// Two title lines, four summary lines and the date row.
    const CARD_HEIGHT: f32 = 188.;

    pub fn new(main_width: f32) -> Self {
        let available = (main_width - super::presentation::PAGE_INSET * 2.).max(1.);
        let gap = super::presentation::SPACE_LG;
        let columns = ((available + gap) / (Self::MIN_CARD_WIDTH + gap))
            .floor()
            .clamp(1., 6.) as u16;
        Self {
            columns,
            card_height: Self::CARD_HEIGHT,
            gap,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grid_columns_keep_cards_at_a_readable_width() {
        for width in [300., 820., 1112., 1600., 2400.] {
            let layout = GridLayout::new(width);
            let available = width - 48.;
            let card = (available - layout.gap * f32::from(layout.columns - 1))
                / f32::from(layout.columns);
            assert!(
                layout.columns == 1 || card >= GridLayout::MIN_CARD_WIDTH,
                "{width}"
            );
            let wider = (available - layout.gap * f32::from(layout.columns))
                / f32::from(layout.columns + 1);
            assert!(
                layout.columns == 6 || wider < GridLayout::MIN_CARD_WIDTH,
                "{width}"
            );
        }
    }

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
