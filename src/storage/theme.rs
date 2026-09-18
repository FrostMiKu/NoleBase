//! Theme file discovery. Storage lists theme TOMLs and persists the theme
//! selection string; resolving a selection into a parsed theme lives in the
//! TUI theme module.

use super::*;

impl Storage {
    pub fn list_theme_names(&self) -> Result<Vec<String>> {
        Ok(self
            .theme_files()?
            .into_iter()
            .filter(|(name, _)| name != "default")
            .map(|(name, _)| name)
            .collect())
    }

    pub(crate) fn theme_files(&self) -> Result<Vec<(String, PathBuf)>> {
        let mut themes = Vec::new();
        for entry in fs::read_dir(&self.themes_dir)
            .with_context(|| format!("reading {}", self.themes_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let is_toml = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"));
            let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if is_toml && !name.is_empty() && name != "random" {
                themes.push((name.to_string(), path));
            }
        }
        themes.sort_by(|(left, _), (right, _)| left.cmp(right));
        Ok(themes)
    }
}
