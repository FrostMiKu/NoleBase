//! Hand-written line-oriented state machine for the hashline patch language.
//!
//! Turns a patch string of one or more `[PATH#TAG]` sections into a [`Patch`]
//! AST. The grammar is fixed by the upstream oh-my-pi hashline spec: section
//! headers carry a mandatory 4-hex snapshot tag, hunks are `PUT` / `CUT` /
//! `REM` / `MV`, and body rows are verbatim `+TEXT` lines under a `:` header.
//! Parsing is strict: every malformed construct is a hard error prefixed with
//! the 1-based patch line it occurred on, using the current upstream grammar.

use std::collections::HashSet;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use regex::Regex;

use super::{GapLocator, Hunk, Op, Patch, Payload, PutLocator, Section, SpanLocator};

/// Envelope markers from the upstream wrapper that this tool intentionally
/// rejects: callers pass only the `[PATH#TAG]` sections.
const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
const END_PATCH_MARKER: &str = "*** End Patch";

/// Maximum length of the offending text echoed by the unrecognized-syntax
/// error, in characters.
const UNRECOGNIZED_TEXT_CAP: usize = 80;

static SECTION_HEADER: OnceLock<Regex> = OnceLock::new();

fn section_header_regex() -> &'static Regex {
    SECTION_HEADER.get_or_init(|| {
        Regex::new(r"^\[(?<path>[^#\r\n]+)#(?<tag>[0-9A-Fa-f]{4})\]$")
            .expect("section header regex is static and valid")
    })
}

/// A section being accumulated until the next section header (or EOF) closes it.
struct SectionBuilder {
    path: String,
    tag: String,
    line_num: usize,
    hunks: Vec<Hunk>,
    /// The terminal file-level op emitted for this section, when present.
    file_op: Option<&'static str>,
}

impl SectionBuilder {
    fn into_section(self) -> Section {
        Section {
            path: self.path,
            tag: self.tag,
            line_num: self.line_num,
            hunks: self.hunks,
        }
    }
}

/// An open `PUT <locator>:` body awaiting `+TEXT` rows.
struct OpenBody {
    locator: PutLocator,
    header_line: usize,
    rows: Vec<String>,
}

/// The result of parsing one hunk-header line.
enum ParsedOp {
    /// A complete op whose payload is inline.
    Bodyless(Op),
    /// A `PUT <locator>:` header whose body rows are still to come.
    Body(PutLocator),
}

/// Parses a complete hashline patch into its sections.
pub(crate) fn parse_patch(input: &str) -> Result<Patch> {
    let mut sections: Vec<Section> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut current: Option<SectionBuilder> = None;
    let mut body: Option<OpenBody> = None;

    for (index, physical) in input.split_terminator('\n').enumerate() {
        let line_num = index + 1;
        let line = physical.strip_suffix('\r').unwrap_or(physical);

        // A header, blank line, or other structural line closes an open body;
        // the same line then continues through the structural parser.
        if let Some(open) = body.take() {
            if let Some(rest) = line.strip_prefix('+') {
                let mut rows = open.rows;
                rows.push(rest.to_string());
                body = Some(OpenBody {
                    locator: open.locator,
                    header_line: open.header_line,
                    rows,
                });
                continue;
            }
            let section = current
                .as_mut()
                .expect("a PUT header implies an open section");
            let hunk = flush_body(open)?;
            push_hunk(section, hunk)?;
        }

        // Blank lines (including whitespace-only) are ignored between hunks.
        if line.trim().is_empty() {
            continue;
        }

        // Header lines trim trailing whitespace; body rows preserve their
        // original bytes.
        let header = line.trim_end();

        if header == BEGIN_PATCH_MARKER || header == END_PATCH_MARKER {
            return Err(anyhow!(
                "line {line_num}: {header} is a patch envelope marker; pass [PATH#TAG] \
                 sections and omit the surrounding envelope"
            ));
        }

        let captures = section_header_regex().captures(header);
        if let Some(captures) = captures {
            let path = captures
                .name("path")
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            let tag = captures
                .name("tag")
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_uppercase();
            if let Some(previous) = current.take() {
                sections.push(previous.into_section());
            }
            if !seen_paths.insert(path.clone()) {
                return Err(anyhow!("line {line_num}: duplicate section for {path}"));
            }
            current = Some(SectionBuilder {
                path,
                tag,
                line_num,
                hunks: Vec::new(),
                file_op: None,
            });
            continue;
        }

        if current.is_none() {
            return Err(anyhow!(
                "line {line_num}: expected a [path#TAG] section header"
            ));
        }

        if line.starts_with('+') {
            return Err(anyhow!(
                "line {line_num}: body rows require a PUT ...: header"
            ));
        }

        let section = current.as_mut().expect("checked Some above");
        match parse_hunk_body(header).map_err(|message| anyhow!("line {line_num}: {message}"))? {
            Some(ParsedOp::Body(locator)) => {
                if let Some(kind) = section.file_op {
                    return Err(anyhow!(
                        "line {line_num}: {kind} must be the last hunk of the section"
                    ));
                }
                body = Some(OpenBody {
                    locator,
                    header_line: line_num,
                    rows: Vec::new(),
                });
            }
            Some(ParsedOp::Bodyless(op)) => {
                let hunk = Hunk { line_num, op };
                push_hunk(section, hunk)?;
            }
            None => {
                let text: String = header.chars().take(UNRECOGNIZED_TEXT_CAP).collect();
                return Err(anyhow!(
                    "line {line_num}: unrecognized hashline syntax: {text}"
                ));
            }
        }
    }

    if let Some(open) = body.take() {
        let section = current
            .as_mut()
            .expect("a PUT header implies an open section");
        let hunk = flush_body(open)?;
        push_hunk(section, hunk)?;
    }
    if let Some(section) = current.take() {
        sections.push(section.into_section());
    }

    Ok(Patch { sections })
}

/// Converts a terminated body into its `PUT` hunk, rejecting empty bodies.
fn flush_body(open: OpenBody) -> Result<Hunk> {
    let OpenBody {
        locator,
        header_line,
        rows,
    } = open;
    if rows.is_empty() {
        let guidance = match locator {
            PutLocator::Span(SpanLocator::Range { start, end }) => {
                format!("put each replacement row on the following line with a `+` prefix (for example `PUT {start}.={end}:\\n+replacement`); use CUT {start}.={end} only to delete")
            }
            PutLocator::Span(SpanLocator::Block(start)) => format!("put each replacement row on the following line with a `+` prefix (for example `PUT {start}*:\\n+replacement`); use CUT {start}* only to delete"),
            PutLocator::Gap(_) => "put each inserted row on the following line with a `+` prefix (for example `PUT >$:\\n+appended text`)".to_string(),
        };
        return Err(anyhow!("line {header_line}: empty PUT body; {guidance}"));
    }
    Ok(Hunk {
        line_num: header_line,
        op: Op::Put {
            locator,
            payload: Payload::Body(rows),
        },
    })
}

/// Records a hunk on a section, enforcing that `REM` and `MV` are terminal.
fn push_hunk(section: &mut SectionBuilder, hunk: Hunk) -> Result<()> {
    if let Some(kind) = section.file_op {
        return Err(anyhow!(
            "line {}: {kind} must be the last hunk of the section",
            hunk.line_num
        ));
    }
    match &hunk.op {
        Op::Rem => section.file_op = Some("REM"),
        Op::Mv { .. } => section.file_op = Some("MV"),
        _ => {}
    }
    section.hunks.push(hunk);
    Ok(())
}

/// Parses one hunk-header line into a parsed op. `Ok(None)` marks text outside
/// hunk-header syntax. Errors carry the message body; the caller adds the
/// leading `line {n}:` prefix.
fn parse_hunk_body(trimmed: &str) -> Result<Option<ParsedOp>, String> {
    if trimmed == "REM" {
        return Ok(Some(ParsedOp::Bodyless(Op::Rem)));
    }
    if let Some(rest) = keyword_rest(trimmed, "PUT") {
        if rest.is_empty() {
            return Ok(None);
        }
        return parse_put(rest).map(Some);
    }
    if let Some(rest) = keyword_rest(trimmed, "CUT") {
        if rest.is_empty() {
            return Ok(None);
        }
        return parse_cut(rest).map(Some);
    }
    if let Some(rest) = keyword_rest(trimmed, "MV") {
        let dest = parse_move_dest(rest)?;
        return Ok(Some(ParsedOp::Bodyless(Op::Mv { dest })));
    }
    Ok(None)
}

/// Splits `keyword` off the front of a header line. The keyword must sit at
/// column 0 and be followed by a whitespace delimiter; `Some("")` means the
/// bare keyword, `None` means the line merely shares the prefix (e.g. `PUTX`).
fn keyword_rest<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(keyword)?;
    match rest.chars().next() {
        None => Some(""),
        Some(ch) if ch.is_whitespace() => Some(rest.trim_start()),
        _ => None,
    }
}

/// Splits a `PUT`/`CUT` argument into its locator text, optional `@register`,
/// and whether a trailing `:` was present.
fn split_register_colon(input: &str) -> Result<(&str, Option<String>, bool), String> {
    let end_trimmed = input.trim_end();
    let (without_colon, had_colon) = match end_trimmed.strip_suffix(':') {
        Some(prefix) => (prefix.trim_end(), true),
        None => (end_trimmed, false),
    };
    if let Some(split) = without_colon.rfind('@') {
        let name = &without_colon[split + 1..];
        if !valid_register(name) {
            return Err(format!("invalid register name `@{name}`"));
        }
        Ok((
            without_colon[..split].trim_end(),
            Some(name.to_string()),
            had_colon,
        ))
    } else {
        Ok((without_colon, None, had_colon))
    }
}

/// `@name` registers accept only ASCII letters, digits, `_`, and `-`.
fn valid_register(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Parses a `PUT` argument (everything after the keyword).
fn parse_put(rest: &str) -> Result<ParsedOp, String> {
    if rest.contains(':') && !rest.trim_end().ends_with(':') {
        return Err(
            "a PUT body must start on the next line: end the header at `:`, then prefix every body row with `+`"
                .to_string(),
        );
    }
    let (locator_part, register, had_colon) = split_register_colon(rest)?;
    let locator_part = locator_part.trim();

    let locator = if is_gap_sigil(locator_part) {
        let gap = parse_gap_locator(locator_part)
            .ok_or_else(|| format!("invalid PUT locator `{locator_part}`"))?;
        PutLocator::Gap(gap)
    } else {
        let span = parse_span_locator(locator_part)?
            .ok_or_else(|| format!("invalid PUT locator `{locator_part}`"))?;
        PutLocator::Span(span)
    };

    if had_colon {
        if register.is_some() {
            return Err("PUT with a `:` header takes body rows, not a named register".into());
        }
        return Ok(ParsedOp::Body(locator));
    }

    match locator {
        PutLocator::Span(_) => {
            let Some(name) = register else {
                return Err(
                    "PUT over a range or block needs a body (`:`) or a named register".into(),
                );
            };
            Ok(ParsedOp::Bodyless(Op::Put {
                locator,
                payload: Payload::Register(Some(name)),
            }))
        }
        PutLocator::Gap(_) => Ok(ParsedOp::Bodyless(Op::Put {
            locator,
            payload: Payload::Register(register),
        })),
    }
}

/// Parses a `CUT` argument (everything after the keyword).
fn parse_cut(rest: &str) -> Result<ParsedOp, String> {
    let (locator_part, register, had_colon) = split_register_colon(rest)?;
    if had_colon {
        return Err("CUT deletes and captures; it takes no body, so drop the `:`".into());
    }
    let locator_part = locator_part.trim();
    if is_gap_sigil(locator_part) {
        return Err(format!(
            "invalid CUT locator `{locator_part}`; CUT takes a span (`N.=M` or `N*`)"
        ));
    }
    let locator = parse_span_locator(locator_part)?
        .ok_or_else(|| format!("invalid CUT locator `{locator_part}`"))?;
    Ok(ParsedOp::Bodyless(Op::Cut { locator, register }))
}

/// Parses a positive 1-based line number: `[1-9][0-9]*` with leading-zero-free form.
fn parse_positive(text: &str) -> Option<usize> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) || text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}

/// Parses a span locator (`N.=M` or `N*`). `Ok(None)` marks text outside span
/// syntax; `Err` reports a semantic locator error (end before start).
fn parse_span_locator(loc: &str) -> Result<Option<SpanLocator>, String> {
    let loc = loc.trim();
    if let Some(digits) = loc.strip_suffix('*') {
        return Ok(parse_positive(digits).map(SpanLocator::Block));
    }
    let Some((start_text, end_text)) = loc.split_once(".=") else {
        return Ok(None);
    };
    let (Some(start), Some(end)) = (parse_positive(start_text), parse_positive(end_text)) else {
        return Ok(None);
    };
    if start > end {
        return Err("range end must be at least the start line".into());
    }
    Ok(Some(SpanLocator::Range { start, end }))
}

/// Parses a gap locator (`<N`, `>N`, `>N*`, `>$`).
fn parse_gap_locator(loc: &str) -> Option<GapLocator> {
    let loc = loc.trim();
    let mut chars = loc.chars();
    match chars.next()? {
        '<' => {
            let line = parse_positive(chars.as_str())?;
            Some(GapLocator::Before(line))
        }
        '>' => {
            let rest = chars.as_str();
            if rest == "$" {
                Some(GapLocator::Eof)
            } else if let Some(digits) = rest.strip_suffix('*') {
                let line = parse_positive(digits)?;
                Some(GapLocator::AfterBlock(line))
            } else {
                let line = parse_positive(rest)?;
                Some(GapLocator::After(line))
            }
        }
        _ => None,
    }
}

/// Whether a locator starts with a gap sigil instead of a span anchor.
fn is_gap_sigil(loc: &str) -> bool {
    matches!(loc.chars().next(), Some('<') | Some('>'))
}

/// Parses a `MV` destination: a double-quoted path or the rest of the line.
fn parse_move_dest(rest: &str) -> Result<String, String> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Err("MV requires a destination path".into());
    }
    if trimmed.starts_with('"') {
        if trimmed.len() < 2 || !trimmed.ends_with('"') {
            return Err("MV destination quote is unterminated".into());
        }
        let inner = &trimmed[1..trimmed.len() - 1];
        if inner.is_empty() {
            return Err("MV requires a destination path".into());
        }
        return Ok(inner.to_string());
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(patch: &str) -> Section {
        parse_patch(patch)
            .unwrap()
            .sections
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn every_op_form_parses_to_expected_ast() {
        let patch = "[a.rs#1a2b]
PUT 1.=2:
+line one
+  indented
PUT 3*:
+block body
PUT <1:
+head row
PUT >5:
+
PUT >6*:
+after block
PUT >$:
+tail
PUT <7 @reg
PUT >8 @reg2
PUT >9
PUT 10.=11 @reg
PUT 12* @reg2
CUT 13.=14
CUT 15* @reg
REM
[b.md#ABCD]
MV \"dest dir/c.md\"
";
        let parsed = parse_patch(patch).unwrap();
        let first = &parsed.sections[0];
        assert_eq!(first.path, "a.rs");
        assert_eq!(first.tag, "1A2B");
        assert_eq!(first.line_num, 1);
        let hunk = |i: usize| first.hunks[i].line_num;
        let lines: Vec<usize> = first.hunks.iter().map(|h| h.line_num).collect();
        assert_eq!(
            lines,
            vec![2, 5, 7, 9, 11, 13, 15, 16, 17, 18, 19, 20, 21, 22]
        );
        assert_eq!(hunk(0), 2);
        assert_eq!(
            first.hunks[0].op,
            Op::Put {
                locator: PutLocator::Span(SpanLocator::Range { start: 1, end: 2 }),
                payload: Payload::Body(vec!["line one".into(), "  indented".into()]),
            }
        );
        assert_eq!(hunk(1), 5);
        assert_eq!(
            first.hunks[1].op,
            Op::Put {
                locator: PutLocator::Span(SpanLocator::Block(3)),
                payload: Payload::Body(vec!["block body".into()]),
            }
        );
        assert_eq!(
            first.hunks[2].op,
            Op::Put {
                locator: PutLocator::Gap(GapLocator::Before(1)),
                payload: Payload::Body(vec!["head row".into()]),
            }
        );
        assert_eq!(
            first.hunks[3].op,
            Op::Put {
                locator: PutLocator::Gap(GapLocator::After(5)),
                payload: Payload::Body(vec![String::new()]), // bare `+` is a blank row
            }
        );
        assert_eq!(
            first.hunks[4].op,
            Op::Put {
                locator: PutLocator::Gap(GapLocator::AfterBlock(6)),
                payload: Payload::Body(vec!["after block".into()]),
            }
        );
        assert_eq!(
            first.hunks[5].op,
            Op::Put {
                locator: PutLocator::Gap(GapLocator::Eof),
                payload: Payload::Body(vec!["tail".into()]),
            }
        );
        assert_eq!(
            first.hunks[6].op,
            Op::Put {
                locator: PutLocator::Gap(GapLocator::Before(7)),
                payload: Payload::Register(Some("reg".into())),
            }
        );
        assert_eq!(
            first.hunks[7].op,
            Op::Put {
                locator: PutLocator::Gap(GapLocator::After(8)),
                payload: Payload::Register(Some("reg2".into())),
            }
        );
        assert_eq!(
            first.hunks[8].op,
            Op::Put {
                locator: PutLocator::Gap(GapLocator::After(9)),
                payload: Payload::Register(None), // colonless gap PUT without @name = anonymous
            }
        );
        assert_eq!(
            first.hunks[9].op,
            Op::Put {
                locator: PutLocator::Span(SpanLocator::Range { start: 10, end: 11 }),
                payload: Payload::Register(Some("reg".into())),
            }
        );
        assert_eq!(
            first.hunks[10].op,
            Op::Put {
                locator: PutLocator::Span(SpanLocator::Block(12)),
                payload: Payload::Register(Some("reg2".into())),
            }
        );
        assert_eq!(
            first.hunks[11].op,
            Op::Cut {
                locator: SpanLocator::Range { start: 13, end: 14 },
                register: None
            }
        );
        assert_eq!(
            first.hunks[12].op,
            Op::Cut {
                locator: SpanLocator::Block(15),
                register: Some("reg".into())
            }
        );
        assert_eq!(first.hunks[13].op, Op::Rem);

        let second = &parsed.sections[1];
        assert_eq!(second.path, "b.md");
        assert_eq!(second.tag, "ABCD");
        assert_eq!(second.line_num, 23);
        assert_eq!(
            second.hunks[0].op,
            Op::Mv {
                dest: "dest dir/c.md".into()
            }
        );
    }

    #[test]
    fn multi_section_patch_keeps_order_and_line_numbers() {
        let patch = "[one#00aa]
PUT 2.=3:
+x
[two#BB11]
MV out.md
[three#cccc]
REM
";
        let parsed = parse_patch(patch).unwrap();
        assert_eq!(parsed.sections.len(), 3);
        assert_eq!(parsed.sections[0].path, "one");
        assert_eq!(parsed.sections[0].tag, "00AA");
        assert_eq!(parsed.sections[0].line_num, 1);
        assert_eq!(parsed.sections[0].hunks[0].line_num, 2);
        assert_eq!(parsed.sections[1].path, "two");
        assert_eq!(parsed.sections[1].tag, "BB11");
        assert_eq!(parsed.sections[1].line_num, 4);
        assert_eq!(parsed.sections[1].hunks[0].line_num, 5);
        assert_eq!(parsed.sections[2].path, "three");
        assert_eq!(parsed.sections[2].tag, "CCCC");
        assert_eq!(parsed.sections[2].line_num, 6);
        assert_eq!(parsed.sections[2].hunks[0].line_num, 7);
    }

    #[test]
    fn body_rows_preserve_verbatim_content() {
        let patch = "[f#0000]
PUT 1.=1:
++plus
+-minus
+  spaces
+
+trailing  
";
        let section = section(patch);
        let body = match &section.hunks[0].op {
            Op::Put {
                payload: Payload::Body(rows),
                ..
            } => rows,
            other => panic!("expected PUT body, got {other:?}"),
        };
        assert_eq!(body, &["+plus", "-minus", "  spaces", "", "trailing  "]);
    }

    #[test]
    fn blank_line_terminates_body_and_is_not_a_row() {
        let patch = "[f#0000]
PUT 1.=1:
+row1
   
PUT 2.=2:
+row2
";
        let section = section(patch);
        assert_eq!(section.hunks.len(), 2);
        let row = |i: usize| match &section.hunks[i].op {
            Op::Put {
                payload: Payload::Body(rows),
                ..
            } => rows.clone(),
            other => panic!("expected PUT body, got {other:?}"),
        };
        assert_eq!(row(0), vec!["row1"]);
        assert_eq!(row(1), vec!["row2"]);
    }

    #[test]
    fn crlf_and_trailing_whitespace_on_headers_are_tolerated() {
        let patch = "[f#0000]  \r\nPUT 1.=1:  \r\n+ok  \r\n";
        let section = section(patch);
        assert_eq!(section.tag, "0000");
        match &section.hunks[0].op {
            Op::Put {
                locator: PutLocator::Span(SpanLocator::Range { start: 1, end: 1 }),
                payload: Payload::Body(rows),
            } => {
                assert_eq!(rows, &["ok  "]);
            }
            other => panic!("unexpected op: {other:?}"),
        }
    }

    #[test]
    fn empty_input_and_blank_only_patch_parse_to_no_sections() {
        let parsed = parse_patch("").unwrap();
        assert!(parsed.sections.is_empty());
        let parsed = parse_patch("\n  \n\t\n").unwrap();
        assert!(parsed.sections.is_empty());
    }

    #[test]
    fn content_before_first_section_header_is_rejected() {
        let err = parse_patch("garbage\n[f#0000]").unwrap_err();
        assert!(
            err.to_string()
                .contains("line 1: expected a [path#TAG] section header"),
            "{err}"
        );
    }

    #[test]
    fn duplicate_section_path_is_rejected() {
        let err = parse_patch("[f#0000]\nREM\n[f#1111]\nREM").unwrap_err();
        assert!(
            err.to_string().contains("line 3: duplicate section for f"),
            "{err}"
        );
    }

    #[test]
    fn range_end_must_follow_start() {
        let err = parse_patch("[f#0000]\nPUT 5.=3:").unwrap_err();
        assert!(
            err.to_string()
                .contains("line 2: range end must be at least the start line"),
            "{err}"
        );
        let err = parse_patch("[f#0000]\nCUT 5.=3").unwrap_err();
        assert!(
            err.to_string()
                .contains("line 2: range end must be at least the start line"),
            "{err}"
        );
    }

    #[test]
    fn span_put_without_colon_or_register_is_rejected() {
        for hunk in ["PUT 3.=4", "PUT 7*"] {
            let err = parse_patch(&format!("[f#0000]\n{hunk}")).unwrap_err();
            assert!(
                err.to_string().contains(
                    "line 2: PUT over a range or block needs a body (`:`) or a named register"
                ),
                "{hunk}: {err}"
            );
        }
    }

    #[test]
    fn body_row_without_open_put_header_is_rejected() {
        let err = parse_patch("[f#0000]\n+orphan").unwrap_err();
        assert!(
            err.to_string()
                .contains("line 2: body rows require a PUT ...: header"),
            "{err}"
        );
    }

    #[test]
    fn empty_put_body_is_rejected_with_body_format_and_cut_guidance() {
        let err = parse_patch("[f#0000]\nPUT 2.=4:\n[g#0000]").unwrap_err();
        assert!(
            err.to_string().contains("PUT 2.=4:\\n+replacement"),
            "{err}"
        );
        let err = parse_patch("[f#0000]\nPUT 3*:\n").unwrap_err();
        assert!(err.to_string().contains("PUT 3*:\\n+replacement"), "{err}");
    }

    #[test]
    fn put_body_on_header_line_is_rejected_with_copyable_guidance() {
        let err = parse_patch("[f#0000]\nPUT >$: appended text").unwrap_err();
        assert!(
            err.to_string().contains(
                "a PUT body must start on the next line: end the header at `:`, then prefix every body row with `+`"
            ),
            "{err}"
        );
    }

    #[test]
    fn unrecognized_syntax_is_rejected_and_truncated() {
        let err = parse_patch("[f#0000]\n  PUT 1.:").unwrap_err();
        assert!(
            err.to_string()
                .contains("line 2: unrecognized hashline syntax:   PUT 1.:"),
            "{err}"
        );
        let long = "x".repeat(200);
        let err = parse_patch(&format!("[f#0000]\n{long}")).unwrap_err();
        let message = err.to_string();
        assert!(message.contains(&format!("unrecognized hashline syntax: {}", "x".repeat(80))));
    }

    #[test]
    fn envelope_markers_are_rejected_with_guidance() {
        for marker in ["*** Begin Patch", "*** End Patch"] {
            let err = parse_patch(&format!("{marker}\n[f#0000]")).unwrap_err();
            let message = err.to_string();
            assert!(message.contains("line 1:"), "{message}");
            assert!(message.contains("envelope"), "{message}");
            assert!(message.contains("sections"), "{message}");
        }
    }

    #[test]
    fn tag_case_is_normalized_to_uppercase() {
        let section = section("[f#aBcD]\nREM\n");
        assert_eq!(section.tag, "ABCD");
    }

    #[test]
    fn quoted_mv_dest_and_unquoted_rest_of_line() {
        let spaced = section("[f#0000]\nMV \"a b/c d.md\"\n");
        assert_eq!(
            spaced.hunks[0].op,
            Op::Mv {
                dest: "a b/c d.md".into()
            }
        );
        let plain = section("[f#0000]\nMV out.md\n");
        assert_eq!(
            plain.hunks[0].op,
            Op::Mv {
                dest: "out.md".into()
            }
        );
    }

    #[test]
    fn register_name_charset_is_rejected() {
        for bad in ["@bad name", "@", "@with/slash", "@dotted.name"] {
            let err = parse_patch(&format!("[f#0000]\nPUT 2.=4 {bad}")).unwrap_err();
            assert!(
                err.to_string().contains("line 2: invalid register name"),
                "{bad}: {err}"
            );
        }
        let ok = parse_patch("[f#0000]\nPUT 2.=4 @a_B-9\n").unwrap();
        match &ok.sections[0].hunks[0].op {
            Op::Put {
                payload: Payload::Register(Some(name)),
                ..
            } => assert_eq!(name, "a_B-9"),
            other => panic!("unexpected op: {other:?}"),
        }
    }

    #[test]
    fn zero_and_leading_zero_locators_are_rejected() {
        for bad in ["PUT 0.=2:", "PUT 05.=6:", "PUT <0:", "PUT >01:", "CUT 0.=1"] {
            let err = parse_patch(&format!("[f#0000]\n{bad}")).unwrap_err();
            let message = err.to_string();
            assert!(message.starts_with("line 2: "), "{bad}: {message}");
            assert!(
                message.contains("PUT locator") || message.contains("CUT locator"),
                "{bad}: {message}"
            );
        }
    }

    #[test]
    fn rem_and_mv_must_be_last_hunks_of_their_section() {
        let err = parse_patch("[f#0000]\nREM\nCUT 1.=2").unwrap_err();
        assert!(
            err.to_string()
                .contains("line 3: REM must be the last hunk of the section"),
            "{err}"
        );
        let err = parse_patch("[f#0000]\nMV x.md\nCUT 1.=2").unwrap_err();
        assert!(
            err.to_string()
                .contains("line 3: MV must be the last hunk of the section"),
            "{err}"
        );
        let err = parse_patch("[f#0000]\nPUT 1.=1:\n+x\nREM\nCUT 2.=3").unwrap_err();
        assert!(
            err.to_string()
                .contains("line 5: REM must be the last hunk of the section"),
            "{err}"
        );
    }

    #[test]
    fn mv_may_follow_line_edits() {
        let section = section("[f#0000]\nPUT 1.=1:\n+x\nMV y.md\n");
        assert_eq!(section.hunks.len(), 2);
        assert_eq!(
            section.hunks[1].op,
            Op::Mv {
                dest: "y.md".into()
            }
        );
    }

    #[test]
    fn mv_requires_a_non_empty_destination() {
        for hunk in ["MV", "MV \"\"", "MV \"unterminated"] {
            let err = parse_patch(&format!("[f#0000]\n{hunk}")).unwrap_err();
            let message = err.to_string();
            assert!(message.starts_with("line 2: "), "{hunk}: {message}");
            assert!(message.contains("destination"), "{hunk}: {message}");
        }
    }

    #[test]
    fn put_with_both_colon_and_register_is_rejected() {
        let err = parse_patch("[f#0000]\nPUT 1.=2 @r:").unwrap_err();
        assert!(
            err.to_string()
                .contains("line 2: PUT with a `:` header takes body rows"),
            "{err}"
        );
    }

    #[test]
    fn cut_rejects_colons_and_gap_locators() {
        let err = parse_patch("[f#0000]\nCUT 1.=2:").unwrap_err();
        assert!(err.to_string().contains("line 2: "), "{err}");
        let err = parse_patch("[f#0000]\nCUT <3").unwrap_err();
        assert!(
            err.to_string().contains("line 2: invalid CUT locator `<3`"),
            "{err}"
        );
    }
}
