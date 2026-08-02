//! Keyboard and mouse input: links.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;

use crate::attachment::{AttachmentStore, AttachmentUri};
use crate::storage::Storage;

use super::super::*;

impl App {
    pub(in crate::app) fn activate_link(&mut self, target: LinkTarget) -> Option<Command> {
        match target {
            LinkTarget::External(target) => Some(Command::OpenLink(target)),
            LinkTarget::Attachment(uri) => {
                let uri = match AttachmentUri::parse(&uri) {
                    Ok(uri) => uri,
                    Err(error) => {
                        self.set_error(format!("Attachment error: {error}"));
                        return None;
                    }
                };
                match materialize_attachment(&self.storage, uri) {
                    Ok(path) => Some(Command::OpenPath(path)),
                    Err(error) => {
                        self.set_error(format!("Attachment error: {error}"));
                        None
                    }
                }
            }
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

/// Materialize an attachment as a file the system application can open,
/// writing the bytes under the Agent workspace (`workspace/main`). The digest
/// in the file name makes it content-addressed, so opening the same attachment
/// again reuses the existing file instead of churning the workspace. Physical
/// object paths never leave the attachment store.
fn materialize_attachment(storage: &Storage, uri: AttachmentUri) -> anyhow::Result<PathBuf> {
    let store = AttachmentStore::new(storage.attachments_dir.clone());
    let metadata = store.metadata(uri.id())?;
    let bytes = store.read_object(uri.id())?;
    let workspace = storage.agent_workspace_dir();
    fs::create_dir_all(&workspace).with_context(|| format!("creating {}", workspace.display()))?;
    let path = workspace.join(attachment_open_name(&metadata.source, uri));
    if fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
        fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(path)
}

/// A stable, content-addressed file name for opening an attachment with the
/// system application. The original extension is preserved so the OS picks the
/// right application; the digest prefix keeps names unique per attachment.
fn attachment_open_name(source: &str, uri: AttachmentUri) -> String {
    let path = Path::new(source);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("attachment");
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty());
    let id = &uri.id().to_hex()[..12];
    match extension {
        Some(extension) => format!("{stem}-{id}.{extension}"),
        None => format!("{stem}-{id}"),
    }
}
