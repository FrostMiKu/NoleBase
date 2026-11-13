//! The single tool-output contract and readable rendering for all execution scopes.
use super::results::{normalize_stored_result, result_reference_message, should_externalize_text};
use super::{ApprovalDenied, SessionResultStore};
use crate::core::provider::ImageBlock;
use serde::ser::{Serialize, SerializeStruct, Serializer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Success,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorCode {
    ToolFailed,
    InvalidInput,
    ApprovalDenied,
    ProcessExit,
    UnsupportedImage,
    ResultStorage,
    ExecutionFailed,
    Cancelled,
}
impl ToolErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolFailed => "tool_failed",
            Self::InvalidInput => "invalid_input",
            Self::ApprovalDenied => "approval_denied",
            Self::ProcessExit => "process_exit",
            Self::UnsupportedImage => "unsupported_image",
            Self::ResultStorage => "result_storage",
            Self::ExecutionFailed => "execution_failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Every tool returns this typed model. `content` is always UTF-8 text;
/// structured tool-specific data is rendered into that text by the tool.
/// Status is derived from the outcome, keeping it consistent with error codes.
#[derive(Clone, Debug)]
pub struct ToolOutput {
    pub content: String,
    pub exit_code: Option<i32>,
    pub(crate) error_code: Option<ToolErrorCode>,
    pub images: Vec<ImageBlock>,
}
impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            exit_code: None,
            error_code: None,
            images: Vec::new(),
        }
    }
    pub fn process(exit_code: i32, content: impl Into<String>) -> Self {
        Self {
            exit_code: Some(exit_code),
            ..Self::text(content)
        }
    }
    pub fn with_images(content: impl Into<String>, images: Vec<ImageBlock>) -> Self {
        Self {
            images,
            ..Self::text(content)
        }
    }
    pub fn failure(code: ToolErrorCode, content: impl Into<String>) -> Self {
        Self {
            error_code: Some(code),
            ..Self::text(content)
        }
    }
    pub fn status(&self) -> ToolStatus {
        if self.error_code().is_some() {
            ToolStatus::Error
        } else {
            ToolStatus::Success
        }
    }
    pub fn error_code(&self) -> Option<ToolErrorCode> {
        self.error_code.or_else(|| {
            self.exit_code
                .filter(|code| *code != 0)
                .map(|_| ToolErrorCode::ProcessExit)
        })
    }
    pub(super) fn from_error(error: &anyhow::Error) -> Self {
        let code = if error.downcast_ref::<ApprovalDenied>().is_some() {
            ToolErrorCode::ApprovalDenied
        } else if error
            .downcast_ref::<super::tool::ToolInputError>()
            .is_some()
        {
            ToolErrorCode::InvalidInput
        } else {
            ToolErrorCode::ToolFailed
        };
        Self::failure(code, super::tool_error_message(error))
    }
    fn header(&self) -> String {
        let mut text = format!(
            "Status: {}\n",
            match self.status() {
                ToolStatus::Success => "success",
                ToolStatus::Error => "error",
            }
        );
        if let Some(code) = self.exit_code {
            text.push_str(&format!("Exit code: {code}\n"));
        }
        if let Some(code) = self.error_code() {
            text.push_str(&format!("Error code: {}\n", code.as_str()));
        }
        text
    }
    pub(super) fn stored_text(&self) -> String {
        format!("{}\n{}", self.header(), self.content)
    }
}

impl Serialize for ToolOutput {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("ToolOutput", 5)?;
        state.serialize_field("status", &self.status())?;
        state.serialize_field("content", &self.content)?;
        state.serialize_field("exit_code", &self.exit_code)?;
        state.serialize_field("error_code", &self.error_code())?;
        state.serialize_field("images", &self.images)?;
        state.end()
    }
}

pub(super) fn externalize_tool_output_with(
    results: &SessionResultStore,
    token_cap: u64,
    mut output: ToolOutput,
) -> anyhow::Result<ToolOutput> {
    // Include framing in the budget; both success and failure bodies use the
    // same store. Native images remain separate provider image parts.
    if should_externalize_text(&render_tool_output(&output), token_cap) {
        let stored = normalize_stored_result(&output.stored_text());
        let uri = results.store(&stored)?;
        output.content = result_reference_message(&uri, &output.content, &stored);
    }
    Ok(output)
}

pub(super) fn render_tool_output(output: &ToolOutput) -> String {
    if output.exit_code.is_some() || output.status() == ToolStatus::Error {
        output.stored_text()
    } else {
        output.content.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_outputs_share_one_typed_protocol_contract() {
        for (output, expected) in [
            (
                ToolOutput::text("first\nsecond"),
                json!({"status":"success","content":"first\nsecond","exit_code":null,"error_code":null,"images":[]}),
            ),
            (
                ToolOutput::process(0, "done"),
                json!({"status":"success","content":"done","exit_code":0,"error_code":null,"images":[]}),
            ),
            (
                ToolOutput::process(-1, "stderr\nreason"),
                json!({"status":"error","content":"stderr\nreason","exit_code":-1,"error_code":"process_exit","images":[]}),
            ),
            (
                ToolOutput::failure(ToolErrorCode::InvalidInput, "Missing path"),
                json!({"status":"error","content":"Missing path","exit_code":null,"error_code":"invalid_input","images":[]}),
            ),
        ] {
            assert_eq!(serde_json::to_value(output).unwrap(), expected);
        }
    }

    #[test]
    fn stored_outcomes_have_readable_headers_and_unescaped_content() {
        let directory = tempfile::tempdir().unwrap();
        let results = SessionResultStore::in_directory(directory.path()).unwrap();
        let body = "first\n\"message\": 中文\\path\nlast\n";
        for (output, header) in [
            (ToolOutput::text(body), "Status: success\n\n"),
            (
                ToolOutput::process(23, body),
                "Status: error\nExit code: 23\nError code: process_exit\n\n",
            ),
            (
                ToolOutput::failure(ToolErrorCode::ToolFailed, body),
                "Status: error\nError code: tool_failed\n\n",
            ),
        ] {
            let compact = externalize_tool_output_with(&results, 1, output).unwrap();
            let reference = compact
                .content
                .split_whitespace()
                .find(|word| word.starts_with("nole://result/"))
                .unwrap();
            let id = reference.strip_prefix("nole://result/").unwrap();
            assert_eq!(
                std::fs::read_to_string(results.root.join(id)).unwrap(),
                format!("{header}{body}")
            );
            assert!(compact.content.contains("Use read"));
        }
    }
}
