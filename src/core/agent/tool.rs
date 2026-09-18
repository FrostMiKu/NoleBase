//! The model-facing tool contract: execution results, the Tool trait, and
//! schema-validated registration.

use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use super::types::ToolExecutionPolicy;

/// A tool execution result: the human/model text plus any native image blocks
/// the tool produced (currently only `read`). Images travel alongside the text
/// so the caller can place them protocol-safe in outgoing messages.
#[derive(Clone, Debug)]
pub struct ToolOutput {
    pub exit_code: Option<i32>,
    pub content: String,
    pub images: Vec<crate::core::provider::ImageBlock>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            exit_code: None,
            content: text.into(),
            images: Vec::new(),
        }
    }

    pub fn process(exit_code: i32, content: impl Into<String>) -> Self {
        Self {
            exit_code: Some(exit_code),
            content: content.into(),
            images: Vec::new(),
        }
    }

    pub fn with_images(
        content: impl Into<String>,
        images: Vec<crate::core::provider::ImageBlock>,
    ) -> Self {
        Self {
            exit_code: None,
            content: content.into(),
            images,
        }
    }

    pub(crate) fn into_inline_text(self) -> String {
        self.content
    }
}

/// The minimal interface needed to expose a new tool to the model.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn input_schema(&self) -> Value;
    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::Exclusive
    }
    /// Whether this tool may be automatically continued as a background job.
    /// Interactive tools must stay foreground so their response is delivered
    /// to the same Agent turn that requested it.
    fn allows_auto_background(&self) -> bool {
        true
    }
    async fn execute(&self, input: &Value) -> Result<ToolOutput>;
}
pub(crate) fn tool_error_message(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

/// A tool paired with its compiled model-facing input contract.
pub(crate) struct RegisteredTool {
    tool: Arc<dyn Tool>,
    validator: jsonschema::Validator,
}

impl RegisteredTool {
    pub(crate) fn new<T: Tool + 'static>(tool: T, schema: &Value) -> Self {
        let name = tool.name();
        let validator = jsonschema::validator_for(schema)
            .unwrap_or_else(|error| panic!("invalid JSON schema for tool {name}: {error}"));
        Self {
            tool: Arc::new(tool),
            validator,
        }
    }

    pub(crate) fn execution_policy(&self) -> ToolExecutionPolicy {
        self.tool.execution_policy()
    }

    pub(crate) fn allows_auto_background(&self) -> bool {
        self.tool.allows_auto_background()
    }

    pub(crate) async fn execute(&self, input: &Value) -> Result<ToolOutput> {
        self.validator.validate(input).map_err(|error| {
            anyhow::anyhow!("invalid input for tool {}: {error}", self.tool.name())
        })?;
        self.tool.execute(input).await
    }
}
