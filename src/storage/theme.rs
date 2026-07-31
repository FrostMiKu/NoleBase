use super::*;

impl Storage {
    pub fn select_theme(&self, selection: &str) -> Result<LoadedTheme> {
        let loaded = self.resolve_theme(selection, None)?;
        self.write_theme_selection(selection)?;
        Ok(loaded)
    }

    pub fn list_theme_names(&self) -> Result<Vec<String>> {
        Ok(self
            .theme_files()?
            .into_iter()
            .filter(|(name, _)| name != "default")
            .map(|(name, _)| name)
            .collect())
    }

    pub(super) fn resolve_theme(
        &self,
        requested: &str,
        previous_random_source: Option<&Path>,
    ) -> Result<LoadedTheme> {
        if requested == "default" {
            return self.load_default_theme(requested);
        }

        let files = self.theme_files()?;
        if requested == "random" {
            if let Some(previous) = previous_random_source {
                if let Some((name, path)) = files.iter().find(|(_, path)| path == previous) {
                    return self.load_theme_file(requested, name, path);
                }
            }

            let mut valid = Vec::new();
            for (name, path) in &files {
                if let Ok(theme) = self.parse_theme_file(path) {
                    valid.push((name, path, theme));
                }
            }
            if valid.is_empty() {
                return self.load_default_theme(requested);
            }
            let (name, path, theme) = valid.swap_remove(fastrand::usize(..valid.len()));
            return Ok(LoadedTheme {
                requested: requested.to_string(),
                active: name.clone(),
                source: Some(path.clone()),
                theme,
            });
        }

        match files.into_iter().find(|(name, _)| name == requested) {
            Some((name, path)) => self.load_theme_file(requested, &name, &path),
            None => self.load_default_theme(requested),
        }
    }

    fn load_default_theme(&self, requested: &str) -> Result<LoadedTheme> {
        let path = self.themes_dir.join("default.toml");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                self.load_theme_file(requested, "default", &path)
            }
            Ok(_) => Ok(LoadedTheme::built_in_default_for(requested)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(LoadedTheme::built_in_default_for(requested))
            }
            Err(error) => Err(error).with_context(|| format!("reading theme {}", path.display())),
        }
    }

    fn load_theme_file(&self, requested: &str, name: &str, path: &Path) -> Result<LoadedTheme> {
        Ok(LoadedTheme {
            requested: requested.to_string(),
            active: name.to_string(),
            source: Some(path.to_path_buf()),
            theme: self.parse_theme_file(path)?,
        })
    }

    fn parse_theme_file(&self, path: &Path) -> Result<Theme> {
        let source = fs::read_to_string(path)
            .with_context(|| format!("reading theme {}", path.display()))?;
        Theme::from_toml(&source).with_context(|| format!("loading theme {}", path.display()))
    }

    fn theme_files(&self) -> Result<Vec<(String, PathBuf)>> {
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
