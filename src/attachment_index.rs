//! Derived index of canonical attachment URIs referenced by managed notes.
//!
//! The index parses each managed Markdown/MBDown file (daily/, data/,
//! archives/) with the same MBDown parser the renderer uses and records the
//! canonical attachment URIs (`nole://attachment/<id>`) emitted for
//! clickable/renderable reference targets: Markdown link destinations
//! (including autolinks and reference-style links), image destinations,
//! MBDown embeds (`![[...]]`), and `[link=...]` tag targets. URI strings in
//! prose, fenced or inline code, HTML comments, escaped text, and wiki-link
//! bodies remain ordinary content because the renderer classifies those
//! regions as text.
//!
//! Per URI the index preserves the total occurrence count and the distinct
//! set of referencing managed notes. The shared document indexer rebuilds at
//! startup (reusing its validated cache when possible), then refreshes from
//! watcher events and publishes snapshots with the last applied revision.
//!
//! The index stores URIs as plain strings keyed on the canonical form produced
//! by [`AttachmentUri`]'s `Display`: the attachment store owns the typed
//! URI/identity, and keeping this module string-keyed avoids a second
//! attachment abstraction while still giving the browser and delete guard
//! exact canonical URIs to compare against.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use mbdown::{Container, Event, InlineTag, Node, SpannedEvent};

use crate::attachment::AttachmentUri;
use crate::document_index::{aggregate_references, DocumentIndex, ReferenceEntry};
use crate::markdown::visit_markdown;
use crate::storage::Storage;

/// Derived reference index: canonical attachment URI -> referencing notes.
#[derive(Clone, Debug, Default)]
pub struct AttachmentReferenceIndex {
    /// Canonical URI -> aggregate reference data.
    references: HashMap<String, ReferenceEntry>,
}

impl AttachmentReferenceIndex {
    pub(crate) fn from_documents(documents: &DocumentIndex) -> Self {
        let files = documents
            .iter()
            .map(|(path, document)| (path.clone(), document.attachment_uris.clone()))
            .collect();
        let references = aggregate_references(&files);
        Self { references }
    }
    /// Scan every managed Markdown/MBDown file and index its attachment URIs.
    #[cfg(test)]
    pub fn build(storage: &Storage) -> Self {
        Self::from_documents(&DocumentIndex::build(storage))
    }

    pub(crate) fn build_checked(storage: &Storage) -> Result<Self> {
        Ok(Self::from_documents(&DocumentIndex::build_checked(
            storage,
        )?))
    }

    #[cfg(test)]
    /// Total occurrences of the canonical URI across all managed files.
    pub fn reference_count(&self, uri: &str) -> usize {
        self.references
            .get(uri)
            .map(|entry| entry.count)
            .unwrap_or(0)
    }

    /// Distinct managed files referencing the canonical URI, sorted by path.
    pub fn locations(&self, uri: &str) -> Vec<PathBuf> {
        self.references
            .get(uri)
            .map(|entry| entry.locations.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    /// Whether any managed file references the canonical URI.
    pub fn is_referenced(&self, uri: &str) -> bool {
        self.references.contains_key(uri)
    }
}

/// Every canonical attachment URI in `text` that the MBDown renderer would
/// render as an attachment reference, in source order, including duplicates:
/// Markdown link destinations (including autolinks and reference-style links),
/// image destinations, MBDown embeds, and `[link=...]` tag targets. URI
/// strings inside code, HTML comments, escaped text, or plain prose remain
/// content because MBDown emits attachment events for rendered targets.
#[cfg(test)]
pub fn find_attachment_uris(text: &str) -> Vec<String> {
    let Ok(document) = mbdown::parse(text) else {
        return Vec::new();
    };
    collect_attachment_uris(document.nodes())
}

pub(crate) fn collect_attachment_uris(nodes: &[Node<'_>]) -> Vec<String> {
    let mut uris = Vec::new();
    collect_node_attachment_uris(nodes, &mut uris);
    uris
}

fn collect_node_attachment_uris(nodes: &[Node<'_>], uris: &mut Vec<String>) {
    visit_markdown(nodes, &mut |markdown| {
        collect_event_attachment_uris(markdown.events(), uris)
    });
}

fn collect_event_attachment_uris(events: &[SpannedEvent<'_>], uris: &mut Vec<String>) {
    for item in events {
        match &item.event {
            Event::Start(Container::Link { target, .. })
            | Event::Start(Container::Image { target, .. }) => {
                push_canonical_attachment_uri(target, uris);
            }
            Event::Embed(target) => push_canonical_attachment_uri(target, uris),
            Event::InlineTag(InlineTag {
                name,
                value: Some(target),
                closing: false,
                ..
            }) if name == "link" => push_canonical_attachment_uri(target, uris),
            _ => {}
        }
    }
}

/// Push `target` when it strictly parses as a canonical attachment URI.
/// Strict parsing keeps the index free of id-format and digest assumptions
/// and guarantees every key is the canonical `nole://attachment/<id>` form
/// [`AttachmentUri`]'s `Display` produces, so lookups from typed attachment
/// ids always match.
fn push_canonical_attachment_uri(target: &str, uris: &mut Vec<String>) {
    if let Ok(uri) = AttachmentUri::parse(target) {
        uris.push(uri.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn storage_with(directory: &tempfile::TempDir) -> Storage {
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        storage
    }

    /// A distinct, strictly canonical `nole://attachment/<uuid>` per seed.
    fn uri(seed: u8) -> String {
        format!("nole://attachment/{seed:08x}-0000-4000-8000-000000000000")
    }

    #[test]
    fn scanner_indexes_links_images_embeds_and_link_tags_only() {
        let a = uri(1);
        let b = uri(2);
        let text = format!(
            "see [report]({a}) and ![img]({b}) or ![[{a}]] and [link={b}]tag[/link]\n\
             autolink <{a}>\n\
             ref [ref][refdef]\n\
             \n\
             [refdef]: {b}\n\
             \n\
             ```text\n\
             {a} in a fence\n\
             ```\n\
             \n\
             inline `{a}` and <!-- {b} --> comment\n\
             escaped \\[x]({b})\n\
             bare {a} in prose\n\
             wikilink [[{b}]]\n"
        );
        let found = find_attachment_uris(&text);
        assert_eq!(
            found,
            vec![a.clone(), b.clone(), a.clone(), b.clone(), a, b]
        );
    }

    #[test]
    fn scanner_rejects_non_canonical_uris() {
        // The value has no UUID-shaped attachment id.
        let not_uuid = "nole://attachment/0000000000000000000000000000000000000000";
        assert!(find_attachment_uris(&format!("[x]({not_uuid})")).is_empty());
        // Uppercase ids use a non-canonical spelling.
        let upper = "nole://attachment/123e4567-e89b-42d3-a456-426614174000".to_uppercase();
        assert!(find_attachment_uris(&format!("[x]({upper})")).is_empty());
        // The legacy digest scheme uses a separate URI format.
        assert!(
            find_attachment_uris(&format!("[x](nole-attachment://sha256/{})", "a".repeat(64)))
                .is_empty()
        );
    }

    #[test]
    fn index_counts_shared_references_across_managed_groups() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(1);
        let b = uri(2);
        fs::write(
            storage.daily_dir.join("2026-07-28.md"),
            format!("[a]({a})\n"),
        )
        .unwrap();
        fs::write(
            storage.data_dir.join("Note.mb"),
            format!("[a]({a}) and [b]({b})\n"),
        )
        .unwrap();
        fs::write(
            storage.archives_dir.join("Old.md"),
            format!("[b]({b}) again\n"),
        )
        .unwrap();

        let index = AttachmentReferenceIndex::build(&storage);
        assert_eq!(index.reference_count(&a), 2);
        assert_eq!(index.locations(&a).len(), 2);
        assert_eq!(index.reference_count(&b), 2);
        assert_eq!(index.locations(&b).len(), 2);
        assert!(index.is_referenced(&a));
        assert!(!index.is_referenced(&uri(3)));
    }

    #[test]
    fn count_tracks_occurrences_and_locations_track_distinct_notes() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(1);
        fs::write(
            storage.data_dir.join("Note.md"),
            format!("[a]({a}) twice [a]({a})\n"),
        )
        .unwrap();
        fs::write(
            storage.daily_dir.join("2026-07-28.md"),
            format!("[a]({a})\n"),
        )
        .unwrap();

        let index = AttachmentReferenceIndex::build(&storage);
        assert_eq!(index.reference_count(&a), 3, "total occurrence count");
        assert_eq!(index.locations(&a).len(), 2, "distinct notes");
    }

    #[test]
    fn code_samples_comments_and_escaped_text_are_not_references() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(1);
        fs::write(
            storage.data_dir.join("Note.md"),
            format!("```text\n{a}\n```\n<!-- {a} -->\n`{a}`\n\\[{a}](x)\nbare {a}\n"),
        )
        .unwrap();
        let index = AttachmentReferenceIndex::build(&storage);
        assert!(!index.is_referenced(&a));
        assert_eq!(index.reference_count(&a), 0);
    }

    #[test]
    fn references_inside_mbdown_containers_are_indexed() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(1);
        fs::write(
            storage.data_dir.join("Note.mb"),
            format!("[box]\n[link={a}]open[/link]\n[/box]\n"),
        )
        .unwrap();
        let index = AttachmentReferenceIndex::build(&storage);
        assert_eq!(index.reference_count(&a), 1);
        assert_eq!(index.locations(&a).len(), 1);
    }
}
