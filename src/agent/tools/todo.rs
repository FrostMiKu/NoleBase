//! The agent's self-maintained task plan, rendered in the Agent sidebar.

use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context as _, Result};
use serde_json::{json, Value};

use crate::agent::Tool;
use crate::agent_session::{TodoItem, TodoStatus};

const MAX_TODO_ITEMS: usize = 32;
const MAX_TODO_CONTENT_CHARS: usize = 160;

/// Shared plan state: replaced wholesale by the `todo_write` tool, read by
/// compaction injection, and mirrored into `AgentConversation::todos` after
/// every tool batch so checkpoints and restarts keep it.
#[derive(Clone, Default)]
pub struct TodoHandle(Arc<Mutex<Vec<TodoItem>>>);

impl TodoHandle {
    fn lock(&self) -> MutexGuard<'_, Vec<TodoItem>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn snapshot(&self) -> Vec<TodoItem> {
        self.lock().clone()
    }

    pub fn replace(&self, todos: Vec<TodoItem>) {
        *self.lock() = todos;
    }
}

/// Plain-text status marker shared by the tool result echo and the compaction
/// injection; the sidebar uses its own animated variant.
pub fn todo_marker(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "○",
        TodoStatus::InProgress => "◐",
        TodoStatus::Completed => "✓",
    }
}

fn render_todos(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "Todo list cleared.".to_string();
    }
    let mut output = String::from("Todos updated:");
    for item in todos {
        output.push('\n');
        output.push_str(todo_marker(item.status));
        output.push(' ');
        output.push_str(&item.content);
    }
    output
}

pub struct TodoWrite {
    todos: TodoHandle,
}

impl TodoWrite {
    pub fn new(todos: TodoHandle) -> Self {
        Self { todos }
    }
}

#[async_trait::async_trait]
impl Tool for TodoWrite {
    fn name(&self) -> &'static str {
        "todo_write"
    }

    fn description(&self) -> &'static str {
        "Maintain the plan of the current task as one full list, replacing the previous list every call. Create it only for non-trivial work of three or more steps; keep at most one item in_progress and start it only when the previous one is completed; update statuses the moment they change instead of batching; mark items completed as soon as they are; skip the tool entirely for simple tasks."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "maxItems": 32,
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {
                                "type": "string",
                                "description": "Imperative one-line task description",
                                "minLength": 1,
                                "maxLength": 160
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"]
                            }
                        },
                        "required": ["content", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["todos"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let raw = input
            .get("todos")
            .and_then(Value::as_array)
            .context("field todos must be an array")?;
        if raw.len() > MAX_TODO_ITEMS {
            anyhow::bail!("todos must not exceed {MAX_TODO_ITEMS} items");
        }
        let mut todos = Vec::with_capacity(raw.len());
        for item in raw {
            let content = item
                .get("content")
                .and_then(Value::as_str)
                .context("every todo needs a string content")?
                .trim()
                .to_string();
            if content.is_empty() {
                anyhow::bail!("todo content must not be empty");
            }
            if content.chars().count() > MAX_TODO_CONTENT_CHARS {
                anyhow::bail!("todo content must not exceed {MAX_TODO_CONTENT_CHARS} characters");
            }
            let status = match item.get("status").and_then(Value::as_str) {
                Some("pending") => TodoStatus::Pending,
                Some("in_progress") => TodoStatus::InProgress,
                Some("completed") => TodoStatus::Completed,
                _ => anyhow::bail!("todo status must be pending, in_progress, or completed"),
            };
            todos.push(TodoItem { content, status });
        }
        self.todos.replace(todos.clone());
        Ok(render_todos(&todos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(handle: &TodoHandle, todos: Value) -> Result<String> {
        crate::agent::test_support::test_runtime()
            .block_on(TodoWrite::new(handle.clone()).execute(&json!({ "todos": todos })))
    }

    #[test]
    fn replaces_the_whole_list_and_echoes_it() {
        let handle = TodoHandle::default();
        let first = write(
            &handle,
            json!([
                {"content": "Design the tool", "status": "completed"},
                {"content": "Wire the sidebar", "status": "in_progress"},
                {"content": "Cover with tests", "status": "pending"},
            ]),
        )
        .unwrap();
        assert_eq!(handle.snapshot().len(), 3);
        assert!(first.contains("Todos updated:"));
        assert!(first.contains("✓ Design the tool"));
        assert!(first.contains("◐ Wire the sidebar"));
        assert!(first.contains("○ Cover with tests"));

        // The next call replaces, never merges.
        let cleared = write(&handle, json!([])).unwrap();
        assert!(handle.snapshot().is_empty());
        assert_eq!(cleared, "Todo list cleared.");
    }

    #[test]
    fn rejects_oversized_and_malformed_entries() {
        let handle = TodoHandle::default();
        let oversized = write(
            &handle,
            json!([{"content": "x".repeat(161), "status": "pending"}]),
        )
        .unwrap_err();
        assert!(oversized
            .to_string()
            .contains("must not exceed 160 characters"));

        let many: Vec<Value> = (0..33)
            .map(|index| json!({"content": format!("task {index}"), "status": "pending"}))
            .collect();
        assert!(write(&handle, many.into())
            .unwrap_err()
            .to_string()
            .contains("32 items"));

        for bad in [
            json!([{"content": "  ", "status": "pending"}]),
            json!([{"content": "task", "status": "queued"}]),
            json!([{"status": "pending"}]),
        ] {
            assert!(write(&handle, bad).is_err());
        }
        assert!(handle.snapshot().is_empty());
    }

    #[test]
    fn schema_binds_the_contract() {
        let schema = TodoWrite::new(TodoHandle::default()).input_schema();
        assert_eq!(schema["required"], json!(["todos"]));
        assert_eq!(schema["properties"]["todos"]["maxItems"], 32);
        assert_eq!(
            schema["properties"]["todos"]["items"]["properties"]["status"]["enum"],
            json!(["pending", "in_progress", "completed"])
        );
        assert_eq!(schema["additionalProperties"], false);
    }
}
