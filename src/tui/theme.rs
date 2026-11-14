//! Terminal adapter for the shared semantic theme.
use crate::core::{
    storage::Storage,
    theme::{self, ThemeTarget},
};
use anyhow::Result;
use ratatui::style::Color;
use std::path::Path;

pub type Theme = theme::Theme<Color>;
pub type LoadedTheme = theme::LoadedTheme<Color>;

impl From<theme::Color> for Color {
    fn from(value: theme::Color) -> Self {
        match value {
            theme::Color::Reset => Self::Reset,
            theme::Color::Rgb(r, g, b) => Self::Rgb(r, g, b),
        }
    }
}

pub fn load_theme(storage: &Storage, previous: Option<&Path>) -> Result<LoadedTheme> {
    theme::load_theme(storage, ThemeTarget::Terminal, previous)
}

pub fn select_theme(storage: &Storage, selection: &str) -> Result<LoadedTheme> {
    theme::select_theme(storage, ThemeTarget::Terminal, selection)
}
