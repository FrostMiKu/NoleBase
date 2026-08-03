//! Document browsing and actions: attachments.
//!
//! The attachment browser lists store metadata (display name, kind, size)
//! joined with the shared usage snapshot (how many distinct managed notes
//! reference each attachment), ordered by `imported_at` descending through the
//! store's list API. Opening an attachment opens the real application-managed
//! content file in place — no copy is ever materialized into the workspace.
//! Deletion is confirmed, refuses while the usage index is not ready or stale,
//! and always ends in the shared authoritative reference scan before the
//! atomic store trash.

use std::path::{Path, PathBuf};

use crate::attachment::{
    AttachmentMetadata, AttachmentQuery, AttachmentSortBy, AttachmentSortOrder, AttachmentUri,
};

use super::super::*;

impl App {
    pub fn open_attachments(&mut self) {
        self.close_dialog();
        self.activate_workspace_view(CenterView::Attachments);
    }

    /// Rebuild the browser rows from the store (query + imported_at descending)
    /// joined with the shared usage snapshot.
    pub(in crate::app) fn recompute_attachments(&mut self) {
        let query = AttachmentQuery {
            query: self.attachment_query.trim().to_string(),
            offset: 0,
            limit: u64::MAX,
            sort_by: AttachmentSortBy::ImportedAt,
            order: AttachmentSortOrder::Desc,
            ..AttachmentQuery::default()
        };
        let page = match self.attachment_store.list(&query) {
            Ok(page) => page,
            Err(error) => {
                self.attachment_entries.clear();
                self.attachment_index = 0;
                self.set_error(format!("Attachment list error: {error}"));
                return;
            }
        };
        let snapshot = self.attachment_usage.snapshot();
        self.attachment_entries = page
            .items
            .into_iter()
            .map(|metadata| {
                let name = attachment_display_name(&metadata);
                AttachmentEntry {
                    id: metadata.id,
                    kind: attachment_kind(&metadata, &name),
                    name: name.clone(),
                    size: metadata.size,
                    locations: snapshot
                        .references
                        .locations(&metadata.uri().to_string())
                        .len(),
                }
            })
            .collect();
        self.attachment_index = self
            .attachment_index
            .min(self.attachment_entries.len().saturating_sub(1));
        self.attachment_list_start = self
            .attachment_list_start
            .min(self.attachment_entries.len().saturating_sub(1));
    }

    /// Re-list the browser when attachment store files changed externally
    /// (content/metadata edits under `attachments/<id>/`). The usage snapshot
    /// is untouched: attachment store events never change note references.
    pub(crate) fn attachment_paths_changed(&mut self, _paths: &[PathBuf]) {
        if self.center_view == CenterView::Attachments {
            self.recompute_attachments();
        }
    }

    pub(in crate::app) fn move_attachment_selection(&mut self, delta: i32) {
        if !self.attachment_entries.is_empty() {
            self.attachment_index =
                move_index(self.attachment_index, delta, self.attachment_entries.len());
        }
    }

    /// Open the selected attachment with the system application. The returned
    /// path is the store's real application-managed content file, so external
    /// saves update the attachment in place; nothing is copied to the
    /// workspace or the cache.
    pub(in crate::app) fn open_attachment_at(&mut self, index: usize) -> Option<Command> {
        let entry = self.attachment_entries.get(index).cloned()?;
        match self.attachment_store.open(entry.id) {
            Ok(path) => Some(Command::OpenPath(path)),
            Err(error) => {
                self.set_error(format!("Attachment open error: {error}"));
                None
            }
        }
    }

    /// Begin the confirmed trash flow for the selected attachment. Refused
    /// before the usage index is ready, and refused up front when managed
    /// notes still reference the attachment, reporting the distinct locations.
    pub(in crate::app) fn request_delete_attachment(&mut self) {
        let Some(entry) = self.attachment_entries.get(self.attachment_index).cloned() else {
            self.set_status("No attachment selected");
            return;
        };
        let snapshot = self.attachment_usage.snapshot();
        if !snapshot.ready {
            self.set_status("Attachment index is still loading; try again");
            return;
        }
        let uri = AttachmentUri::from_id(entry.id).to_string();
        let locations = snapshot.references.locations(&uri);
        if !locations.is_empty() {
            self.set_error(format!(
                "{} is referenced by {} and cannot be moved to trash: {}",
                entry.name,
                locations_label(&locations),
                self.reference_names(&locations)
            ));
            return;
        }
        // Remember which usage snapshot the "unreferenced" decision came from;
        // the confirm handler refuses if it rotated in the meantime.
        self.pending_attachment = Some((entry.id, snapshot.revision));
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

    /// The referencing note paths as relative paths, comma separated.
    fn reference_names(&self, locations: &[PathBuf]) -> String {
        locations
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

/// "1 note" / "N notes" for the distinct managed notes in `locations`.
fn locations_label(locations: &[PathBuf]) -> String {
    match locations.len() {
        1 => "1 note".to_string(),
        count => format!("{count} notes"),
    }
}

/// The display name of an attachment: the application-managed display name
/// stored in metadata (never derived from the store path).
pub(in crate::app) fn attachment_display_name(metadata: &AttachmentMetadata) -> String {
    metadata.display_name.clone()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_and_kind_fall_back_gracefully() {
        let metadata = AttachmentMetadata {
            id: crate::attachment::AttachmentId::parse(&"550e8400-e29b-41d4-a716-446655440000")
                .unwrap(),
            size: 12,
            display_name: "report.pdf".to_string(),
            source: "archives/report.pdf".to_string(),
            mime_type: Some("application/pdf".to_string()),
            imported_at: chrono::Utc::now(),
        };
        assert_eq!(attachment_display_name(&metadata), "report.pdf");
        assert_eq!(attachment_kind(&metadata, "report.pdf"), "pdf");

        let plain = AttachmentMetadata {
            id: metadata.id,
            size: 3,
            display_name: "notes".to_string(),
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
    fn locations_label_reports_distinct_notes() {
        assert_eq!(locations_label(&[]), "0 notes");
        assert_eq!(locations_label(&[PathBuf::from("a")]), "1 note");
        assert_eq!(
            locations_label(&[PathBuf::from("a"), PathBuf::from("b")]),
            "2 notes"
        );
    }
}
