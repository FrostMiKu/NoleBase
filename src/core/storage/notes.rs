use super::*;

impl Storage {
    pub fn load_daily_notes(&self) -> Result<Vec<DailyNote>> {
        self.daily_dates()?
            .into_iter()
            .map(|date| self.read_daily(date))
            .collect()
    }

    /// Append content to today's daily note, creating it on first send.
    pub fn append_to_today(&self, body: &str) -> Result<DailyNote> {
        self.append_to_today_tracked(body).map(|(note, _)| note)
    }

    pub(crate) fn append_to_today_tracked(&self, body: &str) -> Result<(DailyNote, AppendReceipt)> {
        let date = Local::now().date_naive();
        let receipt = self.append_daily_for_date(date, body)?;
        Ok((self.read_daily(date)?, receipt))
    }

    pub fn append_daily(&self, date: &str, body: &str) -> Result<DailyNote> {
        self.append_daily_tracked(date, body).map(|(note, _)| note)
    }

    pub(crate) fn append_daily_tracked(
        &self,
        date: &str,
        body: &str,
    ) -> Result<(DailyNote, AppendReceipt)> {
        let date = parse_daily_date(date)?;
        let receipt = self.append_daily_for_date(date, body)?;
        Ok((self.read_daily(date)?, receipt))
    }

    fn append_daily_for_date(&self, date: NaiveDate, body: &str) -> Result<AppendReceipt> {
        if body.trim().is_empty() {
            bail!("daily content must not be empty");
        }
        fs::create_dir_all(&self.daily_dir)?;
        let path = self.daily_path(date);
        let content = if path.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            format!("\n{body}\n")
        } else {
            format!("{body}\n")
        };
        append_text_tracked(&path, &content)
    }

    pub fn read_daily_by_date(&self, date: &str) -> Result<DailyNote> {
        self.read_daily(parse_daily_date(date)?)
    }

    fn read_daily(&self, date: NaiveDate) -> Result<DailyNote> {
        let path = self.daily_path(date);
        let mut body = fs::read_to_string(&path)
            .with_context(|| format!("reading daily note {}", path.display()))?;
        if body.ends_with('\n') {
            body.pop();
        }
        Ok(DailyNote { date, body })
    }

    pub fn remove_daily(&self, date: &str) -> Result<bool> {
        let path = self.daily_file_path(date)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| format!("deleting {}", path.display())),
        }
    }

    /// Scan task-list items from every daily file, newest day first.
    pub fn load_todo_tasks(&self) -> Result<Vec<TodoItem>> {
        let mut items: Vec<TodoItem> = Vec::new();
        let mut dates = self.daily_dates()?;
        dates.reverse();
        for date in dates {
            let path = self.daily_path(date);
            let text = fs::read_to_string(&path)
                .with_context(|| format!("reading todo tasks from {}", path.display()))?;
            let mut active_task = None;
            for line in text.lines() {
                if let Some((checked, body)) = parse_task_line(line) {
                    items.push(TodoItem {
                        checked,
                        text: body.to_string(),
                    });
                    active_task = Some(items.len() - 1);
                } else if !line.trim().is_empty()
                    && (line.starts_with(' ') || line.starts_with('\t'))
                {
                    if let Some(item) = active_task.and_then(|index| items.get_mut(index)) {
                        item.text.push('\n');
                        item.text.push_str(line.trim());
                    }
                } else if !line.trim().is_empty() {
                    active_task = None;
                }
            }
        }
        Ok(items)
    }

    /// Flip the completion state of the indexed task across all daily files.
    /// Returns `true` if a task at that index was toggled.
    pub fn toggle_todo_task(&self, index: usize) -> Result<bool> {
        let mut dates = self.daily_dates()?;
        dates.reverse();
        let mut count = 0usize;
        for date in dates {
            let path = self.daily_path(date);
            let text = fs::read_to_string(&path)?;
            let mut out = String::with_capacity(text.len());
            let mut toggled = false;
            for line in text.split_inclusive('\n') {
                let is_task = parse_task_line(line.trim_end_matches('\n')).is_some();
                if is_task && count == index {
                    if let Some(flip) = flip_task_line(line) {
                        out.push_str(&flip);
                        toggled = true;
                    } else {
                        out.push_str(line);
                    }
                } else {
                    out.push_str(line);
                }
                if is_task {
                    count += 1;
                }
            }
            if toggled {
                fs::write(path, out)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Append a DailyNote to a managed note, then remove its daily file.
    pub fn move_to_markdown(&self, target: &Path, note: &DailyNote) -> Result<String> {
        let safe = self.validate_target(target)?;
        let content = append_markdown_section(&safe, note)?;
        let removed = self.remove_daily(&note.date.to_string())?;
        debug_assert!(removed, "moved DailyNote not found after append");
        Ok(content)
    }

    /// Restore a deleted or filed DailyNote.
    pub fn restore_daily(&self, note: &DailyNote) -> Result<()> {
        let path = self.daily_path(note.date);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(note.body.as_bytes())?;
        if !note.body.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        Ok(())
    }

    /// Remove the first occurrence of `needle` from `path`. Returns `true` if
    /// found and removed. Used by undo to clean up a filed copy.
    pub fn remove_first_occurrence(&self, path: &Path, needle: &str) -> Result<bool> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading {} for undo", path.display()))?;
        if let Some(idx) = text.find(needle) {
            let mut out = String::with_capacity(text.len() - needle.len());
            out.push_str(&text[..idx]);
            out.push_str(&text[idx + needle.len()..]);
            fs::write(path, out)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn daily_path(&self, date: NaiveDate) -> PathBuf {
        self.daily_dir.join(date_file_name(date))
    }

    /// The `YYYY-MM-DD.md` file name for a daily date.
    pub fn daily_file_name(date: NaiveDate) -> PathBuf {
        PathBuf::from(date_file_name(date))
    }

    /// Return the physical path for a validated `YYYY-MM-DD` daily date.
    pub fn daily_file_path(&self, date: &str) -> Result<PathBuf> {
        Ok(self.daily_path(parse_daily_date(date)?))
    }

    pub fn daily_date_for_path(&self, path: &Path) -> Option<NaiveDate> {
        (path.parent() == Some(self.daily_dir.as_path()))
            .then(|| date_from_path(path))
            .flatten()
    }

    fn daily_dates(&self) -> Result<Vec<NaiveDate>> {
        let mut dates = Vec::new();
        for entry in fs::read_dir(&self.daily_dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_file() && !file_type.is_symlink() {
                if let Some(date) = date_from_path(&entry.path()) {
                    dates.push(date);
                }
            }
        }
        dates.sort_unstable();
        Ok(dates)
    }

    /// Create a new note in `data/` from a user-entered name, returning its path.
    /// Existing filesystem entries are never overwritten.
    pub fn create_named_file(&self, name: &str) -> Result<PathBuf> {
        let file_name = normalize_new_name(name)?;
        self.create_named_file_with_content(&file_name, &format!("# {}\n\n", stem(&file_name)))
    }

    pub fn create_named_file_from_template(&self, name: &str) -> Result<PathBuf> {
        let file_name = normalize_new_name(name)?;
        let template = fs::read_to_string(&self.template_path)
            .with_context(|| format!("reading {}", self.template_path.display()))?;
        self.create_named_file_with_content(&file_name, &template)
    }

    fn create_named_file_with_content(&self, file_name: &str, content: &str) -> Result<PathBuf> {
        fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating {}", self.data_dir.display()))?;
        let path = self.data_dir.join(file_name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("creating new note file {}", path.display()))?;
        file.write_all(content.as_bytes())?;
        Ok(path)
    }

    /// Rename a managed data note to `new_name` (normalized), returning the
    /// new path. Refuses protected names and never overwrites an existing entry.
    pub fn rename_file(&self, from: &Path, new_name: &str) -> Result<PathBuf> {
        let from = self.validate_target(from)?;

        let name = normalize_new_name(new_name)?;
        let to = self.data_dir.join(&name);
        if to.file_name() == from.file_name() {
            return Ok(from);
        }
        match fs::symlink_metadata(&to) {
            Ok(_) => {
                bail!("a file named {name} already exists");
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", to.display()));
            }
        }
        fs::rename(&from, &to)
            .with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
        Ok(to)
    }

    /// Rename a direct archived note without overwriting an existing entry.
    pub fn rename_archived_file(&self, from: &Path, new_name: &str) -> Result<PathBuf> {
        let from = self.validate_archived_target(from)?;
        let name = normalize_new_name(new_name)?;
        let to = self.archives_dir.join(&name);
        rename_without_overwrite(&from, &to, &name)
    }

    /// Move a data note into `archives/` without overwriting an existing entry.
    pub fn archive_note(&self, path: &Path) -> Result<PathBuf> {
        let source = self.validate_target(path)?;
        let name = source.file_name().context("note has no file name")?;
        let destination = self.archives_dir.join(name);
        move_without_overwrite(&source, &destination)?;
        Ok(destination)
    }

    /// Restore an archived note into `data/` without overwriting an existing entry.
    pub fn restore_archived_note(&self, path: &Path) -> Result<PathBuf> {
        let source = self.validate_archived_target(path)?;
        let name = source
            .file_name()
            .context("archived note has no file name")?;
        let destination = self.data_dir.join(name);
        move_without_overwrite(&source, &destination)?;
        Ok(destination)
    }

    /// Delete a managed data note. Protected files cannot be deleted.
    pub fn delete_file(&self, path: &Path) -> Result<()> {
        let path = self.validate_target(path)?;
        fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
        Ok(())
    }

    pub fn delete_archived_file(&self, path: &Path) -> Result<()> {
        let path = self.validate_archived_target(path)?;
        fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
        Ok(())
    }

    pub fn load_skills(&self) -> Result<crate::core::skill::SkillCatalog> {
        Ok(crate::core::skill::load_default_skill_catalog(
            &self.skills_dir,
        ))
    }

    pub fn read_skill(&self, path: &Path) -> Result<crate::core::skill::Skill> {
        let (_, path) = self.locate_skill(path)?;
        crate::core::skill::load_skill(&path)
    }

    pub fn rename_skill(&self, from: &Path, new_id: &str) -> Result<PathBuf> {
        let (root, _) = self.locate_skill(from)?;
        crate::core::skill::rename_skill(&root, from, new_id)
    }

    pub fn delete_skill(&self, path: &Path) -> Result<()> {
        let (root, _) = self.locate_skill(path)?;
        crate::core::skill::delete_skill(&root, path)
    }

    fn locate_skill(&self, path: &Path) -> Result<(PathBuf, PathBuf)> {
        let roots = crate::core::skill::default_skill_roots(&self.skills_dir);
        crate::core::skill::locate_managed_skill(&roots, path)
    }

    /// Read a managed note after applying the same path checks used by
    /// mutating operations.
    pub fn read_note_file(&self, path: &Path) -> Result<String> {
        let path = self.validate_target(path)?;
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    }

    /// Append to an article in either `data/` or `archives/` and return the
    /// receipt required to undo that exact write.
    pub(crate) fn append_document_tracked(&self, path: &Path, body: &str) -> Result<AppendReceipt> {
        if body.trim().is_empty() {
            bail!("note content must not be empty");
        }
        self.read_document_file(path)?;
        let content = if path.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            format!("\n{body}\n")
        } else {
            format!("{body}\n")
        };
        append_text_tracked(path, &content)
    }

    pub(crate) fn undo_append(&self, receipt: &AppendReceipt) -> Result<()> {
        let current_len = receipt
            .path
            .metadata()
            .with_context(|| format!("reading metadata for {}", receipt.path.display()))?
            .len();
        let expected_len = receipt.original_len + receipt.appended.len() as u64;
        if current_len != expected_len {
            bail!("file changed after the append");
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&receipt.path)
            .with_context(|| format!("opening {}", receipt.path.display()))?;
        file.seek(SeekFrom::Start(receipt.original_len))?;
        let mut appended = Vec::with_capacity(receipt.appended.len());
        file.read_to_end(&mut appended)?;
        if appended != receipt.appended {
            bail!("file changed after the append");
        }

        drop(file);
        if receipt.created {
            fs::remove_file(&receipt.path)
                .with_context(|| format!("deleting {}", receipt.path.display()))?;
        } else {
            OpenOptions::new()
                .write(true)
                .open(&receipt.path)?
                .set_len(receipt.original_len)?;
        }
        Ok(())
    }

    /// List daily Markdown files, newest date first.
    pub fn list_daily_file_paths(&self) -> Result<Vec<PathBuf>> {
        let mut dates = self.daily_dates()?;
        dates.reverse();
        Ok(dates
            .into_iter()
            .map(|date| self.daily_path(date))
            .collect())
    }

    /// List flat `.md` notes under `data/`, most recently modified first.
    pub fn list_note_files(&self) -> Result<Vec<NoteFile>> {
        self.list_note_files_in(&self.data_dir, false)
    }

    /// List flat `.md` files under `archives/`, most recently modified first.
    pub fn list_archived_note_files(&self) -> Result<Vec<NoteFile>> {
        self.list_note_files_in(&self.archives_dir, true)
    }

    fn list_note_files_in(&self, directory: &Path, archived: bool) -> Result<Vec<NoteFile>> {
        let mut files = Vec::new();
        fs::create_dir_all(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        for entry in fs::read_dir(directory)? {
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() || file_type.is_symlink() {
                continue;
            }

            let path = entry.path();
            if is_note_path(&path) {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                files.push(NoteFile {
                    path,
                    modified,
                    archived,
                });
            }
        }
        files.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
        });
        Ok(files)
    }

    /// Read a regular note from `archives/` after rejecting symlinks and paths
    /// outside that directory.
    pub fn read_archived_note_file(&self, path: &Path) -> Result<String> {
        let canonical = self.validate_archived_target(path)?;
        fs::read_to_string(canonical).context("reading archived note")
    }

    pub fn validate_archived_target(&self, target: &Path) -> Result<PathBuf> {
        validate_direct_note(&self.archives_dir, target, "archives")
    }

    /// Validate that `path` is a regular, non-symlink managed note directly
    /// inside one of the wiki-managed directories (daily/, data/, archives/).
    /// The path is resolved in full before the containment check, so a managed
    /// directory swapped for a symlink can never redirect the caller to a file
    /// outside the Nole root. Returns the canonical path.
    pub(crate) fn validate_wiki_note(&self, path: &Path) -> Result<PathBuf> {
        if !is_note_path(path) {
            bail!("wiki note must have a .md extension: {}", path.display());
        }
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("checking wiki note {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            bail!("wiki note must be a regular file: {}", path.display());
        }
        let canonical_root = fs::canonicalize(&self.root)
            .with_context(|| format!("resolving Nole root {}", self.root.display()))?;
        let canonical = fs::canonicalize(path)
            .with_context(|| format!("resolving wiki note {}", path.display()))?;
        if !canonical.starts_with(&canonical_root) {
            bail!("wiki note escapes the Nole root: {}", path.display());
        }
        let managed = [&self.daily_dir, &self.data_dir, &self.archives_dir]
            .into_iter()
            .any(|directory| {
                fs::canonicalize(directory).is_ok_and(|canonical_directory| {
                    canonical.parent() == Some(canonical_directory.as_path())
                })
            });
        if !managed {
            bail!(
                "wiki note must live directly in daily/, data/, or archives/: {}",
                path.display()
            );
        }
        Ok(canonical)
    }

    /// Read an open Markdown article anywhere within the Nole workspace.
    pub fn read_document_file(&self, path: &Path) -> Result<String> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("checking document {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            bail!("document must be a regular file: {}", path.display());
        }
        let canonical_root = fs::canonicalize(&self.root)
            .with_context(|| format!("resolving Nole root {}", self.root.display()))?;
        let canonical = fs::canonicalize(path)
            .with_context(|| format!("resolving document {}", path.display()))?;
        let config = fs::canonicalize(&self.config_dir).unwrap_or_else(|_| self.config_dir.clone());
        if !canonical.starts_with(&canonical_root)
            || canonical.starts_with(config)
            || !is_note_path(&canonical)
        {
            bail!("document must be a managed .md file: {}", path.display());
        }
        fs::read_to_string(canonical).context("reading document")
    }

    /// Resolve an existing regular file referenced by a local Markdown link.
    pub fn validate_local_file(&self, path: &Path) -> Result<PathBuf> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("checking local file {}", path.display()))?;
        if !metadata.is_file() {
            bail!("local target must be a regular file: {}", path.display());
        }
        fs::canonicalize(path).with_context(|| format!("resolving local file {}", path.display()))
    }

    /// Ensure a target is a flat data note.
    /// Existing targets are canonicalized in full; symlinks are always rejected.
    pub fn validate_target(&self, target: &Path) -> Result<PathBuf> {
        if !is_note_path(target) {
            bail!("target must have a .md extension: {}", target.display());
        }

        let canonical_data = fs::canonicalize(&self.data_dir).with_context(|| {
            format!("resolving note data directory {}", self.data_dir.display())
        })?;
        match fs::symlink_metadata(target) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    bail!("symlink note targets are not allowed: {}", target.display());
                }
                if !metadata.file_type().is_file() {
                    bail!("note target is not a regular file: {}", target.display());
                }
                let canonical_target = fs::canonicalize(target)
                    .with_context(|| format!("resolving note target {}", target.display()))?;
                let parent = canonical_target.parent();
                let is_data_note = parent == Some(canonical_data.as_path());
                if !is_data_note {
                    bail!(
                        "target must be a direct child of {}: {}",
                        self.data_dir.display(),
                        target.display()
                    );
                }
                Ok(canonical_target)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = target
                    .parent()
                    .context("note target has no parent directory")?;
                let canonical_parent = fs::canonicalize(parent)
                    .with_context(|| format!("resolving target parent {}", parent.display()))?;
                let is_data_note = canonical_parent == canonical_data;
                if !is_data_note {
                    bail!(
                        "target must be a direct child of {}: {}",
                        self.data_dir.display(),
                        target.display()
                    );
                }
                Ok(target.to_path_buf())
            }
            Err(error) => {
                Err(error).with_context(|| format!("checking note target {}", target.display()))
            }
        }
    }
}
