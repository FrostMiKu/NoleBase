//! Keyboard and mouse input: links.

use super::super::*;

impl App {
    pub(in crate::app) fn activate_link(&mut self, target: LinkTarget) -> Option<Command> {
        match target {
            LinkTarget::External(target) => Some(Command::OpenLink(target)),
            LinkTarget::EmbeddedFile(target) => {
                match self.storage.validate_embedded_file(&target) {
                    Ok(path) => Some(Command::OpenPath(path)),
                    Err(error) => {
                        self.set_error(format!("Embed error: {error}"));
                        None
                    }
                }
            }
            LinkTarget::WikiLink(target) => {
                let requested = target.trim().to_string();
                let mut candidates = self
                    .storage
                    .list_daily_file_paths()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|path| wiki_name_matches(path, &requested))
                    .map(|path| WikiLinkCandidate {
                        path,
                        location: WikiLinkLocation::Daily,
                    })
                    .collect::<Vec<_>>();
                candidates.extend(
                    self.storage
                        .list_note_files()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|note| wiki_name_matches(&note.path, &requested))
                        .map(|note| WikiLinkCandidate {
                            path: note.path,
                            location: WikiLinkLocation::Notes,
                        }),
                );
                candidates.extend(
                    self.storage
                        .list_archived_note_files()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|note| wiki_name_matches(&note.path, &requested))
                        .map(|note| WikiLinkCandidate {
                            path: note.path,
                            location: WikiLinkLocation::Archives,
                        }),
                );
                if candidates.is_empty() {
                    match self.storage.create_named_file(&requested) {
                        Ok(path) => {
                            self.reload_files();
                            self.open_file_document(&path, DocumentReturn::Daily);
                            self.set_status(format!("Created note {}", path.display()));
                        }
                        Err(error) => self.set_error(format!("Wiki note error: {error}")),
                    }
                } else if candidates.len() == 1 {
                    self.open_wiki_candidate(&candidates[0]);
                } else {
                    self.wiki_link_target = Some(requested);
                    self.wiki_link_candidates = candidates;
                    self.wiki_link_index = 0;
                    self.set_overlay(Overlay::WikiLinkChoice);
                }
                None
            }
        }
    }

    pub(in crate::app) fn open_wiki_candidate(&mut self, candidate: &WikiLinkCandidate) {
        let source = match candidate.location {
            WikiLinkLocation::Daily => self.storage.read_document_file(&candidate.path),
            WikiLinkLocation::Notes => self.storage.read_note_file(&candidate.path),
            WikiLinkLocation::Archives => self.storage.read_archived_note_file(&candidate.path),
        };
        match source {
            Ok(source) => {
                let title = candidate
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Document".to_string());
                self.show_document(
                    DocumentKind::File(candidate.path.clone()),
                    title,
                    source,
                    DocumentReturn::Daily,
                );
                self.center_view = CenterView::Document;
                self.focus = Focus::Center;
                self.overlay = None;
                self.dialog = None;
                self.wiki_link_target = None;
                self.wiki_link_candidates.clear();
                self.wiki_link_index = 0;
            }
            Err(error) => self.set_error(format!("Wiki note error: {error}")),
        }
    }
}
