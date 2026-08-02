//! Document browsing and actions: attachments.
//!
//! The attachment browser lists store metadata (name, kind, size) joined with
//! the derived reference index (how many managed notes reference each
//! attachment). Opening an attachment materializes a copy under the agent
//! workspace and opens that with the system application, so physical object
//! paths never leave the store. Deletion is confirmed and refuses attachments
//! that are still referenced, reporting the referencing notes.

use std::fs;
use std::path::{Path, PathBuf};

use crate::attachment::AttachmentMetadata;

use super::super::*;

/// Where system-app open copies live, under the agent workspace root.
const OPEN_COPY_SUBDIR: &str = ".nole-open";

impl App {
    pub fn open_attachments(&mut self) {
        self.close_dialog();
        self.activate_workspace_view(CenterView::Attachments);
    }

    /// Rebuild the browser rows from the store and the reference index.
    pub(in crate::app) fn recompute_attachments(&mut self) {
        let query = self.attachment_query.trim().to_lowercase();
        let listed = match self.attachment_store.list() {
            Ok(listed) => listed,
            Err(error) => {
                self.attachment_entries.clear();
                self.attachment_index = 0;
                self.set_error(format!("Attachment list error: {error}"));
                return;
            }
        };
        self.attachment_entries = listed
            .into_iter()
            .map(|metadata| {
                let name = attachment_display_name(&metadata);
                AttachmentEntry {
                    id: metadata.id,
                    kind: attachment_kind(&metadata, &name),
                    name: name.clone(),
                    size: metadata.size,
                    references: self
                        .attachment_refs
                        .reference_count(&metadata.uri().to_string()),
                }
            })
            .filter(|entry| query.is_empty() || entry.name.to_lowercase().contains(&query))
            .collect();
        self.attachment_index = self
            .attachment_index
            .min(self.attachment_entries.len().saturating_sub(1));
        self.attachment_list_start = self
            .attachment_list_start
            .min(self.attachment_entries.len().saturating_sub(1));
    }

    pub(in crate::app) fn move_attachment_selection(&mut self, delta: i32) {
        if !self.attachment_entries.is_empty() {
            self.attachment_index =
                move_index(self.attachment_index, delta, self.attachment_entries.len());
        }
    }

    /// Open the selected attachment with the system application. Returns the
    /// open command; the resolved copy lives under the agent workspace so it
    /// never exposes the store's physical object path.
    pub(in crate::app) fn open_attachment_at(&mut self, index: usize) -> Option<Command> {
        let entry = self.attachment_entries.get(index).cloned()?;
        match self.attachment_store.read_object(entry.id) {
            Ok(bytes) => match self.write_open_copy(&entry, &bytes) {
                Ok(path) => Some(Command::OpenPath(path)),
                Err(error) => {
                    self.set_error(format!("Attachment open error: {error}"));
                    None
                }
            },
            Err(error) => {
                self.set_error(format!("Attachment read error: {error}"));
                None
            }
        }
    }

    /// Begin the confirmed trash flow for the selected attachment. Referenced
    /// attachments are refused up front and their locations are reported.
    pub(in crate::app) fn request_delete_attachment(&mut self) {
        let Some(entry) = self.attachment_entries.get(self.attachment_index).cloned() else {
            self.set_status("No attachment selected");
            return;
        };
        let uri = crate::attachment::AttachmentUri::from_id(entry.id).to_string();
        if let Some(locations) = self.referenced_locations(&uri) {
            self.set_error(format!(
                "{} is referenced by {} and cannot be moved to trash: {}",
                entry.name,
                locations,
                self.reference_names(&uri)
            ));
            return;
        }
        self.pending_attachment = Some(entry.id);
        self.open_dialog(DialogState::new(
            "Move attachment to trash",
            format!(
                "Move {} ({}) to trash? No notes reference it.",
                entry.name,
                human_size(entry.size)
            ),
            DialogMode::Confirm,
            DialogPurpose::DeleteAttachment,
            Vec::new(),
        ));
    }

    /// Materialize a new file under `workspace/main/.nole-open/` holding the
    /// object bytes, for the system application to open. Overwriting the same
    /// attachment reuses its stable file name.
    fn write_open_copy(&self, entry: &AttachmentEntry, bytes: &[u8]) -> anyhow::Result<PathBuf> {
        let directory = self.storage.agent_workspace_dir().join(OPEN_COPY_SUBDIR);
        fs::create_dir_all(&directory)?;
        let name = sanitize_open_name(&entry.name);
        let path = directory.join(format!("{}-{name}", &entry.id.to_hex()[..12]));
        fs::write(&path, bytes)?;
        Ok(path)
    }

    /// The distinct managed notes referencing the URI as "N note(s)", or None
    /// when nothing references it.
    fn referenced_locations(&self, uri: &str) -> Option<String> {
        let count = self.attachment_refs.reference_count(uri);
        (count > 0).then(|| match count {
            1 => "1 note".to_string(),
            n => format!("{n} notes"),
        })
    }

    /// The referencing note paths as relative paths, comma separated.
    fn reference_names(&self, uri: &str) -> String {
        self.attachment_refs
            .locations(uri)
            .iter()
            .map(|path| self.relative_note_path(path))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn relative_note_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.storage.root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }
}

/// The display name of an attachment: the file portion of its import source,
/// falling back to the raw source string.
pub(in crate::app) fn attachment_display_name(metadata: &AttachmentMetadata) -> String {
    Path::new(&metadata.source)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| metadata.source.clone())
}

/// A short display type: the recognizable media type (for example `pdf` or
/// `image/png`) or the source extension, falling back to `file`.
fn attachment_kind(metadata: &AttachmentMetadata, name: &str) -> String {
    if let Some(mime_type) = &metadata.mime_type {
        if let Some(short) = mime_type
            .split('/')
            .next_back()
            .filter(|short| *short != "octet-stream")
        {
            return short.to_string();
        }
    }
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
        .unwrap_or("file")
        .to_string()
}

/// Human-readable byte size, e.g. `1.2 MB`.
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// File names usable in the open-copy directory: keep the extension and the
/// stem, dropping characters that are unsafe in file names.
fn sanitize_open_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return "attachment".to_string();
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension))
            if !stem.is_empty()
                && !extension.is_empty()
                && extension
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()) =>
        {
            (stem, Some(extension))
        }
        _ => (name, None),
    };
    let clean = |part: &str| {
        let cleaned = part
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ' ' | '.') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        cleaned.trim().trim_end_matches('.').to_string()
    };
    let stem = clean(stem);
    let stem = if stem.is_empty() {
        "attachment".to_string()
    } else {
        stem
    };
    match extension {
        Some(extension) => format!("{stem}.{extension}"),
        None => stem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_and_kind_fall_back_gracefully() {
        let metadata = AttachmentMetadata {
            id: crate::attachment::AttachmentId::from_hex(&"a".repeat(64)).unwrap(),
            size: 12,
            source: "archives/report.pdf".to_string(),
            mime_type: Some("application/pdf".to_string()),
            imported_at: chrono::Utc::now(),
        };
        assert_eq!(attachment_display_name(&metadata), "report.pdf");
        assert_eq!(attachment_kind(&metadata, "report.pdf"), "pdf");

        let plain = AttachmentMetadata {
            id: metadata.id,
            size: 3,
            source: "notes".to_string(),
            mime_type: None,
            imported_at: metadata.imported_at,
        };
        assert_eq!(attachment_display_name(&plain), "notes");
        assert_eq!(attachment_kind(&plain, "notes"), "file");
    }

    #[test]
    fn human_size_formats_bytes_and_orders_of_magnitude() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1_024), "1.0 KB");
        assert_eq!(human_size(1_234_567), "1.2 MB");
    }

    #[test]
    fn open_copy_names_are_safe_and_stable() {
        assert_eq!(sanitize_open_name("report.pdf"), "report.pdf");
        assert_eq!(sanitize_open_name("a/b:report v2.pdf"), "a_b_report v2.pdf");
        assert_eq!(sanitize_open_name(""), "attachment");
        assert_eq!(sanitize_open_name(".."), "attachment");
    }
}
