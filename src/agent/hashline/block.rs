//! Dependency-free syntactic block resolution for hashline block anchors.
//!
//! Upstream hashline resolves `PUT N*` / `CUT N*` / `PUT >N*` anchors with a
//! tree-sitter grammar. This module is the native replacement: a conservative
//! line-oriented heuristic resolver for Markdown headings, lists, and fenced
//! code, brace-delimited languages, and indentation-based languages, so the
//! editor stays free of tree-sitter and generated grammars.
//!
//! All spans are 1-based and inclusive. A span with `start == end` denotes a
//! bare single line; callers decide whether that is an error for their
//! operation.

use std::path::Path;

/// Lexical families the block resolver understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Syntax {
    /// Markdown documents: ATX headings, fenced code, list items.
    Markdown,
    /// C-family and similar languages with `{}`/`[]`/`()` blocks.
    Braces,
    /// Indentation-significant languages (Python, YAML, TOML).
    Indent,
    /// Any other file type; blocks cannot be resolved.
    Unknown,
}

/// Maps a file path to the lexical family used for block resolution.
pub(crate) fn syntax_for_path(path: &Path) -> Syntax {
    let ext = path.extension().and_then(|e| e.to_str());
    match ext.map(str::to_ascii_lowercase).as_deref() {
        Some("md" | "markdown" | "mdx" | "mb") => Syntax::Markdown,
        Some(
            "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "cs" | "go" | "java" | "js" | "jsx" | "ts"
            | "tsx" | "json" | "css" | "scss" | "kt" | "swift" | "php" | "scala" | "dart" | "zig"
            | "proto",
        ) => Syntax::Braces,
        Some("py" | "pyi" | "yaml" | "yml" | "toml") => Syntax::Indent,
        _ => Syntax::Unknown,
    }
}

/// Resolves the syntactic block beginning at the 1-based `anchor` line.
///
/// Returns `None` when the syntax family cannot resolve blocks, the anchor is
/// out of range, or the anchor line is blank. A returned `(anchor, anchor)`
/// means the anchor is a bare single line (not a multi-line block opener).
pub(crate) fn resolve_block(
    lines: &[String],
    syntax: Syntax,
    anchor: usize,
) -> Option<(usize, usize)> {
    if anchor == 0 || anchor > lines.len() {
        return None;
    }
    if lines[anchor - 1].trim().is_empty() {
        return None;
    }
    match syntax {
        Syntax::Unknown => None,
        Syntax::Markdown => resolve_markdown(lines, anchor),
        Syntax::Braces => resolve_braces(lines, anchor),
        Syntax::Indent => resolve_indent(lines, anchor),
    }
}

/// True when the trimmed line consists only of closing delimiters, commas,
/// semicolons, and whitespace (e.g. `}`, `});`, `]`, `)`).
#[allow(dead_code)]
pub(crate) fn is_structural_closer(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && trimmed
            .bytes()
            .all(|b| matches!(b, b')' | b']' | b'}' | b',' | b';'))
}

/// Scans at most 64 lines forward from `anchor` (inclusive, blank lines
/// skipped) for a line that begins a multi-line block, returning its span.
pub(crate) fn find_next_block(
    lines: &[String],
    syntax: Syntax,
    anchor: usize,
) -> Option<(usize, usize)> {
    if anchor == 0 || anchor > lines.len() {
        return None;
    }
    let mut idx = anchor;
    let mut scanned = 0usize;
    while idx <= lines.len() && scanned < DIAGNOSTIC_SCAN_LIMIT {
        if !lines[idx - 1].trim().is_empty() {
            if let Some((start, end)) = resolve_block(lines, syntax, idx) {
                if start == idx && end > start {
                    return Some((start, end));
                }
            }
        }
        idx += 1;
        scanned += 1;
    }
    None
}

/// Scans at most 64 lines backward from `anchor` (exclusive, blank lines
/// skipped) for the nearest block whose opener precedes the anchor: the
/// nearest span that contains the anchor is preferred, and when the anchor
/// lies past that span's close (e.g. on a trailing blank line) the nearest
/// block that ends just before it is reported instead.
pub(crate) fn find_enclosing_block(
    lines: &[String],
    syntax: Syntax,
    anchor: usize,
) -> Option<(usize, usize)> {
    if anchor == 0 || anchor > lines.len() {
        return None;
    }
    let mut idx = anchor - 1;
    let mut scanned = 0usize;
    let mut nearest_before: Option<(usize, usize)> = None;
    while idx >= 1 && scanned < DIAGNOSTIC_SCAN_LIMIT {
        if !lines[idx - 1].trim().is_empty() {
            if let Some((start, end)) = resolve_block(lines, syntax, idx) {
                if start < anchor && start < end {
                    if end >= anchor {
                        return Some((start, end));
                    }
                    // Scanning downward visits the closest opener first.
                    if nearest_before.is_none() {
                        nearest_before = Some((start, end));
                    }
                }
            }
        }
        if idx == 1 {
            break;
        }
        idx -= 1;
        scanned += 1;
    }
    nearest_before
}

/// Upper bound on lines examined by the diagnostic scan helpers.
const DIAGNOSTIC_SCAN_LIMIT: usize = 64;

/// True when the line is a standalone `///` or `//!` doc comment.
fn is_doc_comment(line: &str) -> bool {
    let t = trim_ws(line);
    t.starts_with("///") || t.starts_with("//!")
}

/// True when the line is a Rust-style attribute or inner attribute (`#[…]`,
/// `#![…]`, or a bare `#!` such as a shebang).
fn is_rust_attribute(line: &str) -> bool {
    let t = trim_ws(line);
    t.starts_with("#[") || t.starts_with("#!")
}

/// True when the line is a decorator/annotation such as `@staticmethod`,
/// `@Override`, or `@available(...)`.
fn is_decorator(line: &str) -> bool {
    let t = trim_ws(line);
    let Some(rest) = t.strip_prefix('@') else {
        return false;
    };
    rest.bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// True when the line begins a sweepable attribute/decorator run. Plain `//`
/// comments are deliberately not included (upstream rule).
fn is_attribute_like(line: &str) -> bool {
    is_doc_comment(line) || is_rust_attribute(line) || is_decorator(line)
}

/// Walks forward from the 1-based attribute-like `anchor` to the first
/// following non-attribute, non-blank line, returning its 1-based index.
///
/// Multi-line `#[…]` attributes are consumed via the bracket lexer until the
/// `[]`/`()` depth closes, and blank lines between the run and the target are
/// skipped. Returns `None` when the file ends before a target line exists.
fn sweep_attributes(lines: &[String], anchor: usize) -> Option<usize> {
    let mut idx = anchor;
    let mut lexer = Lexer::new();
    let mut pending = false;
    loop {
        let line = lines.get(idx - 1)?;
        if pending {
            lexer.scan_line(line);
            idx += 1;
            if lexer.depth <= 0 {
                pending = false;
            }
            continue;
        }
        if is_attribute_like(line) {
            if is_rust_attribute(line) {
                lexer = Lexer::new();
                lexer.scan_line(line);
                idx += 1;
                if lexer.depth > 0 {
                    pending = true;
                }
            } else {
                idx += 1;
            }
            continue;
        }
        if line.trim().is_empty() {
            idx += 1;
            continue;
        }
        return Some(idx);
    }
}

/// Level of an ATX heading (`^#{1,6}\s`), or `None` otherwise.
fn heading_level(line: &str) -> Option<usize> {
    let t = trim_ws(line);
    let hashes = t.bytes().take_while(|b| *b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    match t.as_bytes().get(hashes) {
        Some(b' ' | b'\t') => Some(hashes),
        _ => None,
    }
}

/// When the line opens a fenced code block, returns the fence character and
/// run length; `None` otherwise.
fn fence_opener(line: &str) -> Option<(char, usize)> {
    let t = trim_ws(line);
    let ch = t.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let len = t.bytes().take_while(|b| *b == ch as u8).count();
    (len >= 3).then_some((ch, len))
}

/// True when the line closes a fence opened with `ch`/`open_len`: at least
/// `open_len` of the same character followed only by whitespace.
fn is_closing_fence(line: &str, ch: char, open_len: usize) -> bool {
    let t = trim_ws(line);
    let len = t.bytes().take_while(|b| *b == ch as u8).count();
    len >= open_len && t[len..].trim().is_empty()
}

/// True when the line is a Markdown list item (`^\s*([-*+]|\d+[.)])\s`).
fn is_list_item(line: &str) -> bool {
    let t = trim_ws(line);
    let mut bytes = t.bytes();
    match bytes.next() {
        Some(b'-' | b'*' | b'+') => matches!(bytes.next(), Some(b' ' | b'\t')),
        Some(b) if b.is_ascii_digit() => {
            let digits = t.bytes().take_while(|b| b.is_ascii_digit()).count();
            let rest = &t[digits..];
            matches!(
                (rest.as_bytes().first(), rest.as_bytes().get(1)),
                (Some(b'.' | b')'), Some(b' ' | b'\t'))
            )
        }
        _ => false,
    }
}

/// True when the line is shaped like a TOML table header (`[section]`,
/// `[[array-of-tables]]`, optionally with a trailing comment). This drives the
/// Indent-mode rule that table headers open an indent run even for keys at the
/// same indentation level.
fn is_table_header(line: &str) -> bool {
    let t = trim_ws(line);
    let Some(rest) = t.strip_prefix('[') else {
        return false;
    };
    let mut depth = 1usize;
    for (i, byte) in rest.bytes().enumerate() {
        match byte {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    let tail = rest[i + 1..].trim();
                    return tail.is_empty() || tail.starts_with('#');
                }
            }
            _ => {}
        }
    }
    false
}

/// Leading-whitespace width of a line; tabs count as four columns.
fn indent_width(line: &str) -> usize {
    line.bytes()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .map(|b| if b == b'\t' { 4 } else { 1 })
        .sum()
}

/// Strips leading spaces and tabs (not other Unicode whitespace).
fn trim_ws(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    &line[i..]
}

fn resolve_markdown(lines: &[String], anchor: usize) -> Option<(usize, usize)> {
    let anchor_line = lines.get(anchor - 1)?;
    if anchor_line.trim().is_empty() {
        return None;
    }
    // Fenced code block: runs through the matching closing fence (or EOF when
    // the fence is never closed).
    if let Some((ch, len)) = fence_opener(anchor_line) {
        let mut end = anchor;
        for idx in (anchor + 1)..=lines.len() {
            end = idx;
            if is_closing_fence(&lines[idx - 1], ch, len) {
                break;
            }
        }
        return Some((anchor, end));
    }
    // ATX heading: runs to the line before the next heading of the same or
    // higher level (fewer or equal `#`), excluding trailing blank lines.
    // Headings inside fenced code are ignored while scanning.
    if let Some(level) = heading_level(anchor_line) {
        let mut end = anchor;
        let mut fence: Option<(char, usize)> = None;
        for idx in (anchor + 1)..=lines.len() {
            let line = &lines[idx - 1];
            if let Some((fch, flen)) = fence {
                if is_closing_fence(line, fch, flen) {
                    fence = None;
                    end = idx;
                }
                continue;
            }
            if let Some((fch, flen)) = fence_opener(line) {
                fence = Some((fch, flen));
                end = idx;
                continue;
            }
            if line.trim().is_empty() {
                continue;
            }
            if let Some(lvl) = heading_level(line) {
                if lvl <= level {
                    break;
                }
            }
            end = idx;
        }
        return Some((anchor, end));
    }
    // List item: runs through subsequent lines that are blank-then-deeper or
    // indented deeper than the marker's indent; trailing blanks are excluded.
    if is_list_item(anchor_line) {
        let base = indent_width(anchor_line);
        let mut end = anchor;
        for idx in (anchor + 1)..=lines.len() {
            let line = &lines[idx - 1];
            if line.trim().is_empty() {
                continue;
            }
            if indent_width(line) > base {
                end = idx;
            } else {
                break;
            }
        }
        return Some((anchor, end));
    }
    // Paragraphs, blockquotes, code lines and other non-openers are bare
    // single lines.
    Some((anchor, anchor))
}

fn resolve_braces(lines: &[String], anchor: usize) -> Option<(usize, usize)> {
    let anchor_line = lines.get(anchor - 1)?;
    if anchor_line.trim().is_empty() {
        return None;
    }
    // Attribute/decorator sweep: the resolved span starts at the attribute.
    if is_attribute_like(anchor_line) {
        let target = sweep_attributes(lines, anchor)?;
        let (_, end) = resolve_braces_core(lines, target)?;
        return Some((anchor, end));
    }
    resolve_braces_core(lines, anchor)
}

/// Brace-resolution core. The anchor must be a non-blank, in-range line.
fn resolve_braces_core(lines: &[String], anchor: usize) -> Option<(usize, usize)> {
    let anchor_line = lines.get(anchor.checked_sub(1)?)?;
    if anchor_line.trim().is_empty() {
        return None;
    }
    let mut lexer = Lexer::new();
    lexer.scan_line(anchor_line);
    if lexer.depth <= 0 {
        return Some((anchor, anchor));
    }
    for idx in (anchor + 1)..=lines.len() {
        if lexer.scan_line(&lines[idx - 1]) {
            return Some((anchor, idx));
        }
    }
    // The opener never balanced; the block runs to end of file.
    Some((anchor, lines.len()))
}

fn resolve_indent(lines: &[String], anchor: usize) -> Option<(usize, usize)> {
    let anchor_line = lines.get(anchor - 1)?;
    if anchor_line.trim().is_empty() {
        return None;
    }
    if is_attribute_like(anchor_line) {
        let target = sweep_attributes(lines, anchor)?;
        let (_, end) = resolve_indent_core(lines, target)?;
        return Some((anchor, end));
    }
    resolve_indent_core(lines, anchor)
}

/// Indent-resolution core. The anchor must be a non-blank, in-range line.
fn resolve_indent_core(lines: &[String], anchor: usize) -> Option<(usize, usize)> {
    let anchor_line = lines.get(anchor.checked_sub(1)?)?;
    if anchor_line.trim().is_empty() {
        return None;
    }
    // TOML table headers behave like indent-run openers: the block spans the
    // header through its keys (at any indentation) until the next
    // header-shaped line or EOF.
    if is_table_header(anchor_line) {
        let mut end = anchor;
        for idx in (anchor + 1)..=lines.len() {
            let line = &lines[idx - 1];
            if line.trim().is_empty() {
                continue;
            }
            if is_table_header(line) {
                break;
            }
            end = idx;
        }
        return Some((anchor, end));
    }
    let base = indent_width(anchor_line);
    let mut end = anchor;
    for idx in (anchor + 1)..=lines.len() {
        let line = &lines[idx - 1];
        if line.trim().is_empty() {
            continue;
        }
        if indent_width(line) > base {
            end = idx;
        } else {
            break;
        }
    }
    Some((anchor, end))
}

/// Persistent line scanner tracking bracket depth while skipping string
/// literals, line comments, and block comments across lines.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Normal,
    // Double-quoted, single-quoted, and backtick (template) strings, all with
    // backslash escapes.
    Double,
    Single,
    Backtick,
    LineComment,
    BlockComment,
}

struct Lexer {
    state: ScanState,
    depth: isize,
    hit_zero: bool,
}

impl Lexer {
    fn new() -> Self {
        Lexer {
            state: ScanState::Normal,
            depth: 0,
            hit_zero: false,
        }
    }

    /// Scans one line, updating string/comment state and bracket depth.
    /// Returns whether the cumulative depth touched zero at any point while
    /// scanning (used to detect the line where a block closes).
    fn scan_line(&mut self, line: &str) -> bool {
        self.hit_zero = false;
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match self.state {
                ScanState::Normal => match bytes[i] {
                    b'"' => self.state = ScanState::Double,
                    b'\'' => self.state = ScanState::Single,
                    b'`' => self.state = ScanState::Backtick,
                    b'/' if bytes.get(i + 1) == Some(&b'/') => self.state = ScanState::LineComment,
                    b'/' if bytes.get(i + 1) == Some(&b'*') => self.state = ScanState::BlockComment,
                    b'{' | b'[' | b'(' => {
                        self.depth += 1;
                        self.note_zero();
                    }
                    b'}' | b']' | b')' => {
                        self.depth -= 1;
                        self.note_zero();
                    }
                    _ => {}
                },
                ScanState::Double | ScanState::Single | ScanState::Backtick => match bytes[i] {
                    b'\\' => i += 1,
                    b'"' if self.state == ScanState::Double => self.state = ScanState::Normal,
                    b'\'' if self.state == ScanState::Single => self.state = ScanState::Normal,
                    b'`' if self.state == ScanState::Backtick => self.state = ScanState::Normal,
                    _ => {}
                },
                ScanState::BlockComment => {
                    if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        self.state = ScanState::Normal;
                        i += 1;
                    }
                }
                ScanState::LineComment => {}
            }
            i += 1;
        }
        if self.state == ScanState::LineComment {
            self.state = ScanState::Normal;
        }
        self.hit_zero
    }

    fn note_zero(&mut self) {
        if self.depth == 0 {
            self.hit_zero = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(src: &str) -> Vec<String> {
        src.lines().map(str::to_string).collect()
    }

    #[test]
    fn heading_nesting_stops_at_same_or_higher_level() {
        let src = lines(
            "\
## Section A
content
### Sub
more
## Section B
# Top
",
        );
        // `##` stops at the next `##` and at `#`, but not at the deeper `###`.
        assert_eq!(resolve_block(&src, Syntax::Markdown, 1), Some((1, 4)));
        assert_eq!(resolve_block(&src, Syntax::Markdown, 3), Some((3, 4)));
        assert_eq!(resolve_block(&src, Syntax::Markdown, 5), Some((5, 5)));
        assert_eq!(resolve_block(&src, Syntax::Markdown, 6), Some((6, 6)));
    }

    #[test]
    fn heading_block_excludes_trailing_blank_lines() {
        let src = lines("## A\ntext\n\n\n");
        assert_eq!(resolve_block(&src, Syntax::Markdown, 1), Some((1, 2)));
    }

    #[test]
    fn fenced_code_ignores_headings_and_resolves_as_block() {
        let src = lines(
            r#"# Title
intro
```
## Not a heading
inside
```
after
# Real
"#,
        );
        // Headings inside fenced code are ignored while scanning.
        assert_eq!(resolve_block(&src, Syntax::Markdown, 1), Some((1, 7)));
        // A fence opener resolves through its matching closer.
        assert_eq!(resolve_block(&src, Syntax::Markdown, 3), Some((3, 6)));
    }

    #[test]
    fn markdown_list_continuation() {
        let src = lines(
            "\
- item one
  continuation
  more

  deep
- item two
",
        );
        assert_eq!(resolve_block(&src, Syntax::Markdown, 1), Some((1, 5)));
        assert_eq!(resolve_block(&src, Syntax::Markdown, 6), Some((6, 6)));

        let ordered = lines("1. first\n   cont\n2. second\n");
        assert_eq!(resolve_block(&ordered, Syntax::Markdown, 1), Some((1, 2)));

        // Missing marker whitespace is not a list item: bare single line.
        let no_space = lines("-item\nmore\n");
        assert_eq!(resolve_block(&no_space, Syntax::Markdown, 1), Some((1, 1)));
    }

    #[test]
    fn braces_handles_nested_braces_strings_and_comments() {
        let src = lines(
            "\
fn outer() {
    let _ = \"}\";
    // {
    let _ = '}';
    if cond {
        inner(1);
    }
}
",
        );
        assert_eq!(resolve_block(&src, Syntax::Braces, 1), Some((1, 8)));
        // A brace inside a string and inside a // comment must not close early.
    }

    #[test]
    fn braces_single_line_statement() {
        let src = lines("let x = 1;\nfn b() {\n    y();\n}\n");
        assert_eq!(resolve_block(&src, Syntax::Braces, 1), Some((1, 1)));
        assert_eq!(resolve_block(&src, Syntax::Braces, 2), Some((2, 4)));
        // Closing braces and `} else {` are bare single lines (net depth <= 0).
        assert_eq!(resolve_block(&src, Syntax::Braces, 4), Some((4, 4)));
    }

    #[test]
    fn derive_attribute_sweep_covers_struct() {
        let src = lines(
            "\
#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
}
",
        );
        assert_eq!(resolve_block(&src, Syntax::Braces, 1), Some((1, 4)));
    }

    #[test]
    fn multiline_attribute_sweep() {
        let src = lines(
            "\
#[derive(
    Debug,
    Clone,
)]
pub struct S {
    x: i32,
}
",
        );
        assert_eq!(resolve_block(&src, Syntax::Braces, 1), Some((1, 7)));
    }

    #[test]
    fn doc_comment_sweep() {
        let src = lines(
            "\
/// Documentation for the module.
///
/// More docs.
pub fn helper() {
    let x = 1;
}
",
        );
        assert_eq!(resolve_block(&src, Syntax::Braces, 1), Some((1, 6)));
        // Plain `//` comments are not swept.
        let plain = lines("// note\nlet x = 1;\n");
        assert_eq!(resolve_block(&plain, Syntax::Braces, 1), Some((1, 1)));
    }

    #[test]
    fn decorator_sweep_for_indent() {
        let src = lines(
            "\
@staticmethod
@classmethod
def helper():
    pass
",
        );
        assert_eq!(resolve_block(&src, Syntax::Indent, 1), Some((1, 4)));
    }

    #[test]
    fn python_def_indent_run_with_interior_blank_lines() {
        let src = lines(
            "\
def compute():
    a = 1

    b = 2
    return a + b
x = 99
",
        );
        assert_eq!(resolve_block(&src, Syntax::Indent, 1), Some((1, 5)));
        assert_eq!(resolve_block(&src, Syntax::Indent, 6), Some((6, 6)));
    }

    #[test]
    fn toml_table_headers_open_indent_runs() {
        let src = lines(
            "\
[package]
name = \"x\"
version = \"1\"

[dependencies]
serde = \"1\"
",
        );
        assert_eq!(resolve_block(&src, Syntax::Indent, 1), Some((1, 3)));
        assert_eq!(resolve_block(&src, Syntax::Indent, 5), Some((5, 6)));
    }

    #[test]
    fn unknown_out_of_range_and_blank_anchors_resolve_to_none() {
        let src = lines("x = 1\n\n");
        assert_eq!(resolve_block(&src, Syntax::Unknown, 1), None);
        assert_eq!(resolve_block(&src, Syntax::Braces, 2), None); // blank line
        assert_eq!(resolve_block(&src, Syntax::Markdown, 2), None);
        assert_eq!(resolve_block(&src, Syntax::Braces, 0), None);
        assert_eq!(resolve_block(&src, Syntax::Braces, 99), None);
        assert_eq!(resolve_block(&src, Syntax::Indent, 3), None);
    }

    #[test]
    fn structural_closer_detection() {
        for line in ["}", "});", "]", ")", "};", ",", ";", "},", "  }  "] {
            assert!(is_structural_closer(line), "expected closer: {line:?}");
        }
        for line in ["", "} else {", "x }", "// }", "{", "a", ")} x"] {
            assert!(!is_structural_closer(line), "expected non-closer: {line:?}");
        }
    }

    #[test]
    fn syntax_for_path_maps_extensions() {
        use std::path::Path;
        assert_eq!(syntax_for_path(Path::new("a.rs")), Syntax::Braces);
        assert_eq!(syntax_for_path(Path::new("A.JSON")), Syntax::Braces);
        assert_eq!(syntax_for_path(Path::new("README.md")), Syntax::Markdown);
        assert_eq!(syntax_for_path(Path::new("note.mb")), Syntax::Markdown);
        assert_eq!(syntax_for_path(Path::new("NOTE.MB")), Syntax::Markdown);
        assert_eq!(syntax_for_path(Path::new("script.py")), Syntax::Indent);
        assert_eq!(syntax_for_path(Path::new("Cargo.toml")), Syntax::Indent);
        assert_eq!(syntax_for_path(Path::new("notes.txt")), Syntax::Unknown);
        assert_eq!(syntax_for_path(Path::new("Makefile")), Syntax::Unknown);
    }

    #[test]
    fn find_next_block_scans_forward() {
        let src = lines(
            "\
fn a() {
}

let y = 1;

fn b() {
    z();
}
",
        );
        assert_eq!(find_next_block(&src, Syntax::Braces, 1), Some((1, 2)));
        assert_eq!(find_next_block(&src, Syntax::Braces, 3), Some((6, 8)));
        assert_eq!(find_next_block(&src, Syntax::Braces, 8), None);
    }

    #[test]
    fn find_next_block_respects_scan_limit() {
        let src: Vec<String> = (0..70).map(|_| "x = 1".to_string()).collect();
        assert_eq!(find_next_block(&src, Syntax::Braces, 1), None);
        assert_eq!(find_next_block(&src, Syntax::Markdown, 1), None);
    }

    #[test]
    fn find_enclosing_block_scans_backward() {
        let src = lines(
            "\
fn outer() {
    let a = 1;
    fn inner() {
        let b = 2;
    }
}
",
        );
        assert_eq!(find_enclosing_block(&src, Syntax::Braces, 4), Some((3, 5)));
        assert_eq!(find_enclosing_block(&src, Syntax::Braces, 3), Some((1, 6)));
        let flat = lines("let a = 1;\nlet b = 2;\n");
        assert_eq!(find_enclosing_block(&flat, Syntax::Braces, 2), None);
    }
}
