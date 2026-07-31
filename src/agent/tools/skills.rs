//! Progressive loading for user-owned Agent skills.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::agent::{Tool, ToolExecutionPolicy};
use crate::skill::Skill;

pub struct LoadSkill {
    skills: HashMap<String, String>,
}

impl LoadSkill {
    pub fn new(skills: &[Skill]) -> Self {
        Self {
            skills: skills
                .iter()
                .map(|skill| (skill.id.clone(), skill.body.clone()))
                .collect(),
        }
    }
}

#[async_trait::async_trait]
impl Tool for LoadSkill {
    fn name(&self) -> &'static str {
        "load_skill"
    }
    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "Load the complete instructions for one available Agent skill by its exact id. Load a relevant skill before following its workflow."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Exact id from the available skills catalog"
                }
            },
            "required": ["id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let id = input
            .get("id")
            .and_then(Value::as_str)
            .context("field id must be a string")?;
        self.skills
            .get(id)
            .cloned()
            .with_context(|| format!("unknown or unavailable skill: {id}"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn loader_returns_only_the_requested_skill_body() {
        let loader = LoadSkill::new(&[
            Skill {
                id: "alpha".to_string(),
                description: "Alpha".to_string(),
                body: "Alpha body".to_string(),
                path: PathBuf::from("skills/alpha.md"),
            },
            Skill {
                id: "beta".to_string(),
                description: "Beta".to_string(),
                body: "Beta body".to_string(),
                path: PathBuf::from("skills/beta.md"),
            },
        ]);
        let runtime = crate::agent::test_support::test_runtime();

        assert_eq!(
            runtime
                .block_on(loader.execute(&json!({"id": "beta"})))
                .unwrap(),
            "Beta body"
        );
        assert!(runtime
            .block_on(loader.execute(&json!({"id": "missing"})))
            .unwrap_err()
            .to_string()
            .contains("unknown or unavailable skill"));
    }
}
