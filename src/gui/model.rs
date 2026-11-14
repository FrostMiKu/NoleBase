//! Desktop state and persistence, through the shared workspace service.
use crate::core::{model::SearchHit, workspace_index::TagSummary};
use crate::core::{
    storage::Storage,
    theme::{self, LoadedTheme},
    workspace::Workspace,
};
use anyhow::{bail, Context, Result};
use gpui_kit::SharedString;
use std::{sync::Arc, time::SystemTime};

#[derive(Clone)]
pub struct Document {
    pub path: String,
    pub title: String,
    pub preview: Arc<Vec<super::markdown::PreviewBlock>>,
    stamp: SystemTime,
    pub archived: bool,
}

pub struct Desktop {
    pub workspace: Arc<Workspace>,
    pub sessions: Arc<crate::core::agent_sessions::AgentSessions>,
    pub todos: Vec<crate::core::model::TodoItem>,
    pub theme: LoadedTheme,
    pub documents: Vec<Document>,
    pub notes: Vec<usize>,
    pub archives: Vec<usize>,
    pub daily_notes: Vec<crate::core::model::DailyNote>,
    pub tags: Vec<TagSummary>,
    pub search_results: Vec<SearchResult>,
    pub query: String,
    pub tag_documents: Vec<String>,
    pub daily_blocks: Vec<DailyBlock>,
    pub generation: u64,
    pub selected: Option<String>,
    pub source: String,
    pub revision: u64,
}

impl Desktop {
    pub fn open(storage: Storage) -> Result<Self> {
        storage.ensure_files()?;
        let theme = theme::load_theme(&storage, None)?;
        let workspace = Arc::new(Workspace::open(storage)?);
        let sessions = crate::core::agent_sessions::AgentSessions::open(workspace.clone())?;
        let mut model = Self {
            workspace,
            sessions,
            todos: Vec::new(),
            theme,
            documents: Vec::new(),
            notes: Vec::new(),
            archives: Vec::new(),
            daily_notes: Vec::new(),
            tags: Vec::new(),
            search_results: Vec::new(),
            query: String::new(),
            tag_documents: Vec::new(),
            daily_blocks: Vec::new(),
            generation: 0,
            selected: None,
            source: String::new(),
            revision: 0,
        };
        model.refresh()?;
        Ok(model)
    }

    pub fn refresh(&mut self) -> Result<()> {
        self.todos = self.workspace.todos()?;
        let daily_notes = self.workspace.daily_notes()?;
        if daily_notes.len() != self.daily_notes.len()
            || daily_notes
                .iter()
                .zip(&self.daily_notes)
                .any(|(a, b)| a.date != b.date || a.body != b.body)
        {
            self.daily_blocks = daily_notes
                .iter()
                .flat_map(|note| {
                    let date = note.date.to_string();
                    let mut rows = vec![DailyBlock {
                        id: format!("{date}-header"),
                        date: Some(date.clone()),
                        text: SharedString::default(),
                    }];
                    rows.extend(
                        super::markdown::blocks(&note.body)
                            .into_iter()
                            .enumerate()
                            .map(|(index, text)| DailyBlock {
                                id: format!("{date}-{index}"),
                                date: None,
                                text,
                            }),
                    );
                    rows
                })
                .collect();
            self.daily_notes = daily_notes;
        }
        let previous: std::collections::HashMap<_, _> = std::mem::take(&mut self.documents)
            .into_iter()
            .map(|doc| (doc.path.clone(), doc))
            .collect();
        self.documents = self
            .workspace
            .note_files()?
            .into_iter()
            .map(|file| {
                let path = self
                    .workspace
                    .relative_path(&file.path)
                    .context("locating note")?;
                if let Some(cached) = previous.get(&path).filter(|doc| doc.stamp == file.modified) {
                    return Ok(cached.clone());
                }
                let title = file
                    .path
                    .file_stem()
                    .context("reading note name")?
                    .to_string_lossy()
                    .into_owned();
                let source = self.workspace.read_document(&path)?;
                Ok(Document {
                    path,
                    title,
                    preview: Arc::new(super::markdown::preview(&source)),
                    stamp: file.modified,
                    archived: file.archived,
                })
            })
            .collect::<Result<_>>()?;
        self.rebuild_collections();
        self.generation += 1;
        Ok(())
    }

    pub fn rebuild_collections(&mut self) {
        self.notes.clear();
        self.archives.clear();
        for (index, document) in self.documents.iter().enumerate() {
            if document.archived {
                self.archives.push(index);
            } else {
                self.notes.push(index);
            }
        }
    }

    pub fn open_document(&mut self, path: Option<String>) -> Result<()> {
        let source = match &path {
            Some(path) => self.workspace.read_document(path)?,
            None => String::new(),
        };
        self.selected = path;
        self.source = source;
        self.revision += 1;
        Ok(())
    }

    pub fn save(&mut self, draft: &str) -> Result<()> {
        if let Some(path) = &self.selected {
            if draft == self.source {
                return Ok(());
            }
            if draft != self.source {
                if self.workspace.read_document(path)? != self.source {
                    bail!("这篇笔记已在其他窗口更新，请保留当前文字并重新打开笔记。")
                }
                self.workspace.write_document(path, draft)?;
                self.source = draft.to_owned();
            }
        } else if !draft.trim().is_empty() {
            self.workspace.append_to_today(draft)?;
        } else {
            return Ok(());
        }
        self.refresh()
    }

    pub fn create_note(&mut self, name: &str, use_template: bool) -> Result<()> {
        let path = if use_template {
            self.workspace.create_note_from_template(name)?
        } else {
            self.workspace.create_note(name)?
        };
        self.refresh()?;
        let relative = self
            .workspace
            .relative_path(&path)
            .context("locating new note")?;
        self.open_document(Some(relative))
    }

    pub fn refresh_index(&mut self) -> bool {
        if !self.workspace.poll_index_update() {
            return false;
        }
        self.tags = self.workspace.tags().unwrap_or_default();
        self.search_results = self.search(&self.query);
        true
    }

    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        search_workspace(&self.workspace, query)
    }

    pub fn select_tag(&mut self, tag: &str) {
        self.tag_documents = self
            .workspace
            .tag_documents(tag)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|doc| self.workspace.relative_path(&doc.path))
            .collect();
    }

    pub fn cycle_theme(&mut self) -> Result<()> {
        let mut names = vec!["default".to_owned()];
        names.extend(self.workspace.storage().list_theme_names()?);
        let next = names
            .iter()
            .position(|name| name == &self.theme.active)
            .map(|index| (index + 1) % names.len())
            .unwrap_or(0);
        self.theme = theme::select_theme(self.workspace.storage(), &names[next])?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct DailyBlock {
    pub id: String,
    pub date: Option<String>,
    pub text: SharedString,
}

#[derive(Clone)]
pub struct SearchResult {
    pub path: String,
    pub line_no: usize,
    pub text: SharedString,
}

pub fn search_workspace(workspace: &Workspace, query: &str) -> Vec<SearchResult> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    workspace
        .search(query)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|hit| match hit {
            SearchHit::FileLine {
                path,
                line_no,
                text,
            } => Some(SearchResult {
                path: workspace.relative_path(&path)?,
                line_no,
                text: text.into(),
            }),
            SearchHit::DocumentLine { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_writes_are_visible_to_the_shared_workspace() {
        let root = tempfile::tempdir().unwrap();
        let mut desktop = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
        desktop.create_note("Test note", false).unwrap();
        desktop.save("# Hello\n\nShared note").unwrap();
        let path = desktop.selected.clone().unwrap();
        assert_eq!(
            desktop.workspace.read_document(&path).unwrap(),
            "# Hello\n\nShared note"
        );
        desktop.open_document(None).unwrap();
        desktop.save("Daily capture").unwrap();
        assert!(desktop
            .workspace
            .daily_notes()
            .unwrap()
            .iter()
            .any(|note| note.body.contains("Daily capture")));
    }

    #[test]
    fn save_conflict_preserves_external_content() {
        let root = tempfile::tempdir().unwrap();
        let mut desktop = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
        desktop.create_note("Test note", false).unwrap();
        let path = desktop.selected.clone().unwrap();
        desktop
            .workspace
            .write_document(&path, "External edit")
            .unwrap();
        assert!(desktop.save("Desktop edit").is_err());
        assert_eq!(
            desktop.workspace.read_document(&path).unwrap(),
            "External edit"
        );
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    #[test]
    fn full_text_search_and_tags_use_the_shared_index_across_document_groups() {
        let root = tempfile::tempdir().unwrap();
        let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
        let file = model.workspace.create_note("Deep note").unwrap();
        let path = model.workspace.relative_path(&file).unwrap();
        model
            .workspace
            .write_document(
                &path,
                &format!(
                    "# Deep note\n\n{}needle #research",
                    ("Long introduction. ".repeat(100) + "\n\n").repeat(9)
                ),
            )
            .unwrap();
        model
            .workspace
            .append_to_today("Daily needle #research")
            .unwrap();
        let archive = model.workspace.create_note("Archive").unwrap();
        let archive_path = model.workspace.relative_path(&archive).unwrap();
        model
            .workspace
            .write_document(&archive_path, "Archived needle #research")
            .unwrap();
        model.workspace.archive_note(&archive).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            model.refresh_index();
            if model.search("needle").len() == 3
                && model
                    .tags
                    .iter()
                    .any(|tag| tag.name == "research" && tag.documents == 3)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "index did not publish all document groups"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        model.refresh().unwrap();
        assert!(!model
            .documents
            .iter()
            .find(|doc| doc.path == path)
            .unwrap()
            .preview
            .iter()
            .any(|block| block.text.contains("needle")));
        let hits = model.search("needle");
        assert!(hits
            .iter()
            .any(|hit| hit.path.starts_with("data/") && hit.line_no == 21));
        assert!(hits.iter().any(|hit| hit.path.starts_with("daily/")));
        assert!(hits.iter().any(|hit| hit.path.starts_with("archives/")));
        model.select_tag("research");
        assert_eq!(model.tag_documents.len(), 3);
        for path in model.tag_documents.clone() {
            model.open_document(Some(path)).unwrap();
            assert!(model.source.contains("needle"));
        }
    }
}
