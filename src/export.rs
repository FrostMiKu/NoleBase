//! Safe, offline publication formats shared by the UI and Agent.
//!
//! Renderers return a [`RenderedExport`] carrying the artifact bytes plus
//! explicit diagnostics, so degraded resources (unavailable images) are
//! surfaced as warnings.

mod assets;
mod highlight;
mod html;
mod katex;

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use anyhow::{bail, Context, Result};

use crate::attachment::AttachmentStore;

/// Upper bound for the UTF-8 source of a rendered export. Rendered formats
/// hold the whole source in memory, so pathological files receive a size error
/// during preparation and rendering remains bounded.
pub(crate) const MAX_RENDER_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// Upper bound for the produced artifact (HTML) of a single export.
pub(crate) const MAX_EXPORT_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;

/// Severity of an [`ExportDiagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportDiagnosticSeverity {
    /// The export succeeded but a resource or engine step degraded.
    Warning,
}

/// A diagnostic surfaced by an export instead of being silently swallowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportDiagnostic {
    pub severity: ExportDiagnosticSeverity,
    pub message: String,
}

impl ExportDiagnostic {
    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: ExportDiagnosticSeverity::Warning,
            message: message.into(),
        }
    }
}

impl fmt::Display for ExportDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {}",
            match self.severity {
                ExportDiagnosticSeverity::Warning => "warning",
            },
            self.message
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Original,
    Html,
}

impl ExportFormat {
    pub const ALL: [Self; 2] = [Self::Original, Self::Html];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Original => "Original",
            Self::Html => "HTML",
        }
    }

    pub const fn agent_value(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Html => "html",
        }
    }

    pub const fn hint(self) -> &'static str {
        match self {
            Self::Original => "Exact source bytes",
            Self::Html => "Safe standalone .html",
        }
    }

    pub const fn required_suffix(self) -> Option<&'static str> {
        match self {
            Self::Original => None,
            Self::Html => Some("html"),
        }
    }

    pub fn validate_destination(self, path: &Path) -> Result<()> {
        if let Some(required) = self.required_suffix() {
            let actual = path.extension().and_then(|value| value.to_str());
            if !actual.is_some_and(|value| value.eq_ignore_ascii_case(required)) {
                bail!(
                    "{} export destination must end in .{required}",
                    self.label()
                );
            }
        }
        Ok(())
    }
}

impl fmt::Display for ExportFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

impl FromStr for ExportFormat {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "original" => Ok(Self::Original),
            "html" => Ok(Self::Html),
            _ => bail!("format must be one of: original, html"),
        }
    }
}
/// A rendered export artifact plus the diagnostics the renderer wants to
/// surface (engine warnings, degraded resources).
#[derive(Debug)]
pub(crate) struct RenderedExport {
    pub bytes: Vec<u8>,
    pub diagnostics: Vec<ExportDiagnostic>,
}

pub(crate) fn render_html(
    source: &str,
    title: &str,
    root: &Path,
    note: &Path,
    attachments: &AttachmentStore,
) -> Result<RenderedExport> {
    let rendered = html::render(source, title, root, note, attachments)?;
    let bytes = rendered
        .assets
        .materialize_data_uris(&rendered.html)
        .into_bytes();
    let size = u64::try_from(bytes.len()).context("export is too large")?;
    if size > MAX_EXPORT_OUTPUT_BYTES {
        bail!(
            "HTML export exceeds the {}-byte output limit",
            MAX_EXPORT_OUTPUT_BYTES
        );
    }
    let asset_bytes = rendered.assets.total_bytes();
    if asset_bytes > assets::MAX_TOTAL_ASSET_BYTES {
        bail!(
            "HTML export images exceed the {}-byte total limit",
            assets::MAX_TOTAL_ASSET_BYTES
        );
    }
    Ok(RenderedExport {
        bytes,
        diagnostics: convert_render_diagnostics(rendered.diagnostics),
    })
}

/// Map the renderer's rich internal degradations onto the public diagnostic
/// shape carried by `ExportOutcome`.
fn convert_render_diagnostics(diagnostics: Vec<html::RenderDiagnostic>) -> Vec<ExportDiagnostic> {
    diagnostics
        .into_iter()
        .map(|diagnostic| {
            ExportDiagnostic::warning(match diagnostic.kind {
                html::RenderDiagnosticKind::Image => format!(
                    "image '{}' could not be embedded: {}",
                    diagnostic.target, diagnostic.reason
                ),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_parse_and_enforce_rendered_suffixes() {
        assert_eq!(
            "original".parse::<ExportFormat>().unwrap(),
            ExportFormat::Original
        );
        assert_eq!("html".parse::<ExportFormat>().unwrap(), ExportFormat::Html);
        assert!(ExportFormat::Html
            .validate_destination(Path::new("x.HTML"))
            .is_ok());
        assert!("pdf".parse::<ExportFormat>().is_err());
    }

    fn renderer_fixture() -> (tempfile::TempDir, std::path::PathBuf, AttachmentStore) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("data/nested")).unwrap();
        let store = AttachmentStore::new(root.join("attachments"));
        store.ensure_layout().unwrap();
        (directory, root, store)
    }

    #[test]
    fn html_export_preserves_safe_semantics_and_visible_text() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let local = root.join("data/nested/中文.png");
        image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
            .save(&local)
            .unwrap();
        let png = std::fs::read(&local).unwrap();
        let managed = store.import_bytes(&png, Some("managed.png")).unwrap();
        let uri = crate::attachment::AttachmentUri::from_id(managed.id).to_string();
        std::fs::write(root.join("data/nested/broken.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let source = format!(
            "## Heading\n\n<script>alert(1)</script> [unsafe](javascript:alert(1))\n\n![chart](中文.png)\n\n![Missing alt](missing.png)\n\n![Broken alt](broken.png)\n\n![[{uri}]]\n\n[box width=full border=none bg=17]\n[color=bright-cyan]Bright[/color]\n\n| Left | Center | Right |\n|:-----|:------:|------:|\n| 1 | 2 | 3 |\n[/box]\n\n[link=https://example.test]safe[/link] [link=javascript:alert(1)]plain[/link]\n\n- [x] done\n\n```mermaid\ngraph LR; A[Start] --> B[End]\n```\n"
        );
        std::fs::write(&note, &source).unwrap();
        let rendered = html::render(&source, "note.md", &root, &note, &store).unwrap();
        assert!(!rendered.html.contains("中文.png"));
        assert!(!rendered.html.contains(&format!("src=\"{uri}\"")));
        assert!(rendered.html.contains("Image Missing alt"));
        assert!(rendered.html.contains("Image Broken alt"));
        assert!(rendered.html.contains("☑"));
        assert_eq!(rendered.diagnostics.len(), 2);
        assert!(rendered
            .diagnostics
            .iter()
            .all(|d| d.kind == html::RenderDiagnosticKind::Image));
        assert!(rendered
            .diagnostics
            .iter()
            .any(|d| d.target == "missing.png"));
        assert!(rendered
            .diagnostics
            .iter()
            .any(|d| d.target == "broken.png"));
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<h2>Heading</h2>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("href=\"javascript:"));
        assert_eq!(html.matches("src=\"data:image/png;base64,").count(), 2);
        assert!(html.contains("alt=\"chart\""));
        assert!(html.contains("Image Missing alt"));
        assert!(html.contains("Image Broken alt"));
        assert!(html.contains("class=\"nole-box nole-bg-dark\" style=\"border:none;"));
        assert!(html.contains("background:#00005f"));
        assert!(html.contains("color:#00ffff"));
        assert!(html.contains("<th style=\"text-align:left\">Left</th>"));
        assert!(html.contains("<th style=\"text-align:center\">Center</th>"));
        assert!(html.contains("<th style=\"text-align:right\">Right</th>"));
        assert!(html.contains("href=\"https://example.test\""));
        assert!(!html.contains("[link="));
        // The fenced mermaid block becomes an escaped-source container with the
        // inlined runtime and hashed CSP; the hostile `<script>` in the source
        // stays inert escaped text.
        assert!(html.contains("<pre class=\"mermaid\">graph LR; A[Start] --&gt; B[End]"));
        assert!(html.contains("script-src 'sha256-"));
        assert!(!html.contains("<script>alert"));
    }

    #[test]
    fn render_html_surfaces_degraded_images_as_public_warnings() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        std::fs::write(root.join("data/nested/broken.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let source = "![Broken](broken.png)\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render_html(source, "note.md", &root, &note, &store).unwrap();
        assert_eq!(rendered.diagnostics.len(), 1);
        let diagnostic = &rendered.diagnostics[0];
        assert_eq!(diagnostic.severity, ExportDiagnosticSeverity::Warning);
        assert!(diagnostic.message.contains("broken.png"));
        assert_eq!(
            diagnostic.to_string(),
            format!("warning: {}", diagnostic.message)
        );
    }
}
