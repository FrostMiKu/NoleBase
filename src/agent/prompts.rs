//! System prompt construction and user prompt buffering.

use std::path::Path;

use chrono::{DateTime, Local};

use crate::provider::{Message, MessagePart, MessageRole, SystemBlock};
use crate::skill::Skill;

pub(crate) fn format_buffered_prompts(prompts: Vec<String>) -> String {
    prompts
        .into_iter()
        .map(|prompt| prompt_with_datetime(&prompt, Local::now()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn append_user_text(messages: &mut Vec<Message>, text: String) {
    if let Some(message) = messages
        .last_mut()
        .filter(|message| message.role == MessageRole::User)
    {
        if let Some(MessagePart::Text { text: existing }) = message.parts.last_mut() {
            existing.push_str("\n\n");
            existing.push_str(&text);
        } else {
            message.parts.push(MessagePart::Text { text });
        }
        return;
    }
    messages.push(Message::user(text));
}

pub(crate) fn system_prompt_sections(
    root: &Path,
    agents_instructions: &str,
    skills: &[Skill],
    memory: &str,
) -> Vec<SystemBlock> {
    let project_marker = "## Project instructions";
    let memory_marker = "## Memory";
    vec![
        SystemBlock {
            text: system_prompt_text(root),
            cache: true,
        },
        SystemBlock {
            text: format!("{project_marker}\n{agents_instructions}"),
            cache: false,
        },
        SystemBlock {
            text: skill_catalog_prompt(skills),
            cache: true,
        },
        SystemBlock {
            text: format!("{memory_marker}\n{memory}"),
            cache: false,
        },
    ]
}

pub(crate) fn skill_catalog_prompt(skills: &[Skill]) -> String {
    let mut prompt = String::from(
        "## Available skills\nSkills supplement the current request and do not grant tools or permissions.\n",
    );
    if skills.is_empty() {
        prompt.push_str("No skills are currently available.");
    } else {
        for skill in skills {
            prompt.push_str(&format!("- `{}`: {}\n", skill.id, skill.description));
        }
        prompt.pop();
    }
    prompt
}

#[cfg(test)]
pub(crate) fn system_prompt(root: &Path, agents_instructions: &str, memory: &str) -> String {
    system_prompt_sections(root, agents_instructions, &[], memory)
        .into_iter()
        .map(|block| block.text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn system_prompt_text(root: &Path) -> String {
    format!(
        r#"You are Nole's note assistant. Base claims and edits on the user's files and tool results; never invent workspace state.

## Notes
Nole notes use CommonMark plus `#tag`, `[[note]]`, `![[file]]`, fenced `mermaid`, and closed BBCode tags: `[b]`, `[i]`, `[u]`, `[s]`, `[dim]`, `[color=COLOR]`, `[bg=COLOR]`, `[link=URL]`, `[center]`, `[right]`, `[indent first=N]`, `[box title="..." width=WIDTH border=single|none border-color=COLOR bg=COLOR px=N py=N]`, `[columns gap=N]`, and `[column width=WIDTH]`. Colors may be names, palette indexes, or `#RRGGBB`. Close every tag. Resolve wikilinks before creating or changing their targets. Local links in notes are relative to the containing note; links in chat are relative to the Nole root. Never emit terminal escape sequences.

## Workspace
Root: {root}
- Managed notes are in `data/`, `daily/`, and `archives/`; use `workspace/main/` for intermediate files. Paths are relative to the Nole root unless a tool says otherwise.
- Never read or expose `config/ai.toml`, and access attachments only through attachment tools."#,
        root = root.display(),
    )
}

pub(crate) fn prompt_with_datetime(prompt: &str, now: DateTime<Local>) -> String {
    format!(
        "Current local date and time: {}\n\n{prompt}",
        now.to_rfc3339()
    )
}
