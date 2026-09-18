//! Small, bounded previews for tool inputs while their JSON is still streaming.
//!
//! Complete tool inputs continue to be assembled and validated by the provider.
//! This module only decodes top-level string fields needed to show progress for
//! long `write`, `edit`, and `append` calls without retaining a second copy of
//! the body.

const MAX_PREVIEW_LINE_CHARS: usize = 2_048;
const MAX_TARGET_BYTES: usize = 4_096;

#[derive(Default)]
pub(crate) struct ToolCallStreamPreview {
    name: String,
    input: JsonObjectStringStream,
    fields: PreviewFields,
    last_message: String,
}

impl ToolCallStreamPreview {
    pub(crate) fn push(&mut self, name: &str, arguments: &str) -> Option<String> {
        self.name.push_str(name);
        self.input.push(arguments, &mut self.fields);
        let message = self.message()?;
        if message == self.last_message {
            return None;
        }
        self.last_message.clone_from(&message);
        Some(message)
    }

    pub(crate) fn is_visible(&self) -> bool {
        !self.last_message.is_empty()
    }

    fn message(&self) -> Option<String> {
        let display_name = match self.name.as_str() {
            "write" => "Write",
            "edit" => "Edit",
            "append" => "Append",
            _ => return None,
        };
        let mut message = format!("Preparing {display_name}...");
        let target = if matches!(self.name.as_str(), "write" | "append") {
            self.fields.path.trim().to_string()
        } else {
            self.fields.patch_path().unwrap_or_default()
        };
        if target.is_empty() {
            if self.fields.body.started {
                message.push_str(&format!(
                    "\nReceiving arguments · {}",
                    human_size(self.fields.body.bytes)
                ));
            } else {
                message.push_str("\nReceiving arguments");
            }
        } else if self.fields.body.started {
            let lines = self.fields.body.lines();
            message.push_str(&format!(
                "\n{target} · {lines} {} · {}",
                if lines == 1 { "line" } else { "lines" },
                human_size(self.fields.body.bytes)
            ));
        } else {
            message.push('\n');
            message.push_str(&target);
        }
        if let Some(line) = self.fields.body.latest_nonempty() {
            message.push_str("\n…");
            message.push_str(line);
        }
        Some(message)
    }
}

#[derive(Default)]
struct PreviewFields {
    path: String,
    body: BodyProgress,
    patch_header: String,
    patch_header_finished: bool,
}

impl PreviewFields {
    fn push(&mut self, key: &str, character: char) {
        match key {
            "path" if self.path.len() < MAX_TARGET_BYTES => self.path.push(character),
            "content" => self.body.push(character),
            "patch" => {
                if !self.patch_header_finished {
                    if character == '\n' {
                        self.patch_header_finished = true;
                    } else if self.patch_header.len() < MAX_TARGET_BYTES {
                        self.patch_header.push(character);
                    }
                }
                self.body.push(character);
            }
            _ => {}
        }
    }

    fn patch_path(&self) -> Option<String> {
        let header = self.patch_header.trim();
        let path = header.strip_prefix('[')?.split_once('#')?.0.trim();
        (!path.is_empty()).then(|| path.to_string())
    }
}

#[derive(Default)]
struct BodyProgress {
    bytes: u64,
    newlines: usize,
    started: bool,
    ends_with_newline: bool,
    current_line: String,
    current_line_chars: usize,
    last_nonempty_line: String,
}

impl BodyProgress {
    fn push(&mut self, character: char) {
        self.started = true;
        self.bytes = self.bytes.saturating_add(character.len_utf8() as u64);
        self.ends_with_newline = character == '\n';
        if character == '\n' {
            self.newlines = self.newlines.saturating_add(1);
            if !self.current_line.trim().is_empty() {
                self.last_nonempty_line.clone_from(&self.current_line);
            }
            self.current_line.clear();
            self.current_line_chars = 0;
        } else if character != '\r' && self.current_line_chars < MAX_PREVIEW_LINE_CHARS {
            self.current_line.push(character);
            self.current_line_chars += 1;
        }
    }

    fn lines(&self) -> usize {
        if self.started {
            self.newlines + usize::from(!self.ends_with_newline)
        } else {
            0
        }
    }

    fn latest_nonempty(&self) -> Option<&str> {
        let current = self.current_line.trim();
        if !current.is_empty() {
            Some(current)
        } else {
            let previous = self.last_nonempty_line.trim();
            (!previous.is_empty()).then_some(previous)
        }
    }
}

#[derive(Clone, Copy, Default)]
enum ObjectPhase {
    #[default]
    BeforeObject,
    BeforeKey,
    InKey,
    AfterKey,
    BeforeValue,
    InValue,
    AfterValue,
    Done,
}

#[derive(Default)]
struct JsonObjectStringStream {
    phase: ObjectPhase,
    decoder: JsonStringDecoder,
    key: String,
}

impl JsonObjectStringStream {
    fn push(&mut self, input: &str, fields: &mut PreviewFields) {
        for character in input.chars() {
            match self.phase {
                ObjectPhase::BeforeObject if character == '{' => {
                    self.phase = ObjectPhase::BeforeKey;
                }
                ObjectPhase::BeforeKey if character == '"' => {
                    self.key.clear();
                    self.decoder = JsonStringDecoder::default();
                    self.phase = ObjectPhase::InKey;
                }
                ObjectPhase::BeforeKey if character == '}' => self.phase = ObjectPhase::Done,
                ObjectPhase::InKey => {
                    let decoded = self.decoder.push(character);
                    decoded.for_each(|character| self.key.push(character));
                    if decoded.ended {
                        self.phase = ObjectPhase::AfterKey;
                    }
                }
                ObjectPhase::AfterKey if character == ':' => {
                    self.phase = ObjectPhase::BeforeValue;
                }
                ObjectPhase::BeforeValue if character == '"' => {
                    self.decoder = JsonStringDecoder::default();
                    self.phase = ObjectPhase::InValue;
                }
                ObjectPhase::InValue => {
                    let decoded = self.decoder.push(character);
                    decoded.for_each(|character| fields.push(&self.key, character));
                    if decoded.ended {
                        self.phase = ObjectPhase::AfterValue;
                    }
                }
                ObjectPhase::AfterValue if character == ',' => {
                    self.phase = ObjectPhase::BeforeKey;
                }
                ObjectPhase::AfterValue if character == '}' => self.phase = ObjectPhase::Done,
                _ => {}
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
enum EscapeState {
    #[default]
    Normal,
    Escaped,
    Unicode {
        value: u16,
        digits: u8,
    },
}

#[derive(Default)]
struct JsonStringDecoder {
    escape: EscapeState,
    pending_high_surrogate: Option<u16>,
}

#[derive(Clone, Copy, Default)]
struct DecodedCharacters {
    first: Option<char>,
    second: Option<char>,
    ended: bool,
}

impl DecodedCharacters {
    fn push(&mut self, character: char) {
        if self.first.is_none() {
            self.first = Some(character);
        } else {
            self.second = Some(character);
        }
    }

    fn for_each(self, mut apply: impl FnMut(char)) {
        if let Some(character) = self.first {
            apply(character);
        }
        if let Some(character) = self.second {
            apply(character);
        }
    }
}

impl JsonStringDecoder {
    fn push(&mut self, character: char) -> DecodedCharacters {
        let mut decoded = DecodedCharacters::default();
        match self.escape {
            EscapeState::Normal if character == '"' => {
                self.flush_pending(&mut decoded);
                decoded.ended = true;
            }
            EscapeState::Normal if character == '\\' => self.escape = EscapeState::Escaped,
            EscapeState::Normal => {
                self.flush_pending(&mut decoded);
                decoded.push(character);
            }
            EscapeState::Escaped => {
                self.escape = EscapeState::Normal;
                match character {
                    '"' => decoded.push('"'),
                    '\\' => decoded.push('\\'),
                    '/' => decoded.push('/'),
                    'b' => decoded.push('\u{0008}'),
                    'f' => decoded.push('\u{000c}'),
                    'n' => decoded.push('\n'),
                    'r' => decoded.push('\r'),
                    't' => decoded.push('\t'),
                    'u' => {
                        self.escape = EscapeState::Unicode {
                            value: 0,
                            digits: 0,
                        }
                    }
                    _ => decoded.push('\u{fffd}'),
                }
            }
            EscapeState::Unicode { mut value, digits } => {
                if let Some(digit) = character.to_digit(16) {
                    value = (value << 4) | digit as u16;
                    if digits == 3 {
                        self.escape = EscapeState::Normal;
                        self.emit_code_unit(value, &mut decoded);
                    } else {
                        self.escape = EscapeState::Unicode {
                            value,
                            digits: digits + 1,
                        };
                    }
                } else {
                    self.escape = EscapeState::Normal;
                    self.flush_pending(&mut decoded);
                    decoded.push('\u{fffd}');
                }
            }
        }
        decoded
    }

    fn emit_code_unit(&mut self, unit: u16, decoded: &mut DecodedCharacters) {
        if (0xd800..=0xdbff).contains(&unit) {
            self.flush_pending(decoded);
            self.pending_high_surrogate = Some(unit);
            return;
        }
        if (0xdc00..=0xdfff).contains(&unit) {
            if let Some(high) = self.pending_high_surrogate.take() {
                let scalar = 0x1_0000 + (((high as u32 - 0xd800) << 10) | (unit as u32 - 0xdc00));
                decoded.push(char::from_u32(scalar).unwrap_or('\u{fffd}'));
            } else {
                decoded.push('\u{fffd}');
            }
            return;
        }
        self.flush_pending(decoded);
        decoded.push(char::from_u32(unit as u32).unwrap_or('\u{fffd}'));
    }

    fn flush_pending(&mut self, decoded: &mut DecodedCharacters) {
        if self.pending_high_surrogate.take().is_some() {
            decoded.push('\u{fffd}');
        }
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1_024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1_024.0 && unit + 1 < UNITS.len() {
        value /= 1_024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_preview_decodes_fragmented_json_and_tracks_the_latest_nonempty_line() {
        let mut preview = ToolCallStreamPreview::default();
        assert_eq!(preview.push("wri", r#"{"pa"#), None);
        let first = preview
            .push("te", r#"th":"notes\/design.md","content":"first\n\n\u6700"#)
            .unwrap();
        assert_eq!(
            first,
            "Preparing Write...\nnotes/design.md · 3 lines · 10 B\n…最"
        );
        let second = preview.push("", r#"\u540e line"}"#).unwrap();
        assert_eq!(
            second,
            "Preparing Write...\nnotes/design.md · 3 lines · 18 B\n…最后 line"
        );
    }

    #[test]
    fn edit_preview_extracts_the_first_patch_target() {
        let mut preview = ToolCallStreamPreview::default();
        let message = preview
            .push(
                "edit",
                r#"{"patch":"[data\/Note.md#A12F]\nPUT 2.=2:\n+revised"}"#,
            )
            .unwrap();
        assert_eq!(
            message,
            "Preparing Edit...\ndata/Note.md · 3 lines · 38 B\n…+revised"
        );
    }

    #[test]
    fn append_preview_uses_its_structured_path_and_content() {
        let mut preview = ToolCallStreamPreview::default();
        let message = preview
            .push(
                "append",
                r#"{"path":"data\/Note.md","tag":"A12F","content":"first\nlast"}"#,
            )
            .unwrap();
        assert_eq!(
            message,
            "Preparing Append...\ndata/Note.md · 2 lines · 10 B\n…last"
        );
    }
}
