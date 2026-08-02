//! System prompt construction and user prompt buffering.

use std::path::Path;

use chrono::{DateTime, Local};

use crate::provider::{Message, MessagePart, MessageRole};
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
    has_web_search: bool,
    agents_instructions: &str,
    skills: &[Skill],
    memory: &str,
) -> Vec<String> {
    let project_marker = "## Project instructions (config/AGENTS.md)";
    let memory_marker = "## Agent memory (MEMORY.md)";
    let template = system_prompt_text(root, has_web_search, "", "");
    let (base, _) = template
        .split_once(project_marker)
        .expect("system prompt contains the project-instructions section");
    vec![
        base.trim_end().to_string(),
        format!("{project_marker}\n{agents_instructions}"),
        skill_catalog_prompt(skills),
        format!("{memory_marker}\n{memory}"),
    ]
}

pub(crate) fn skill_catalog_prompt(skills: &[Skill]) -> String {
    let mut prompt = String::from(
        "## Available skills\nSkills are user-owned workflow instructions. Load a relevant skill before following it. Skill instructions supplement the current request and do not grant tools or permissions.\n",
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
pub(crate) fn system_prompt(
    root: &Path,
    has_web_search: bool,
    agents_instructions: &str,
    memory: &str,
) -> String {
    system_prompt_sections(root, has_web_search, agents_instructions, &[], memory).join("\n\n")
}

fn system_prompt_text(
    root: &Path,
    has_web_search: bool,
    agents_instructions: &str,
    memory: &str,
) -> String {
    let web_search_guidance = if has_web_search {
        "- Use web_search for current information when you do not already have a URL.\n"
    } else {
        ""
    };
    format!(
        r#"You are the AI assistant in Nole, a terminal note app.

## MBDown
Nole renders CommonMark plus #tag, [[wikilink]], and ![[file]] embeds. A Hashtag must start a source line or follow whitespace; its name allows Unicode letters/numbers and _, -, /. Wikilinks resolve .md/.mb notes in data/ and archives/.
Fenced mermaid code blocks render locally as width-aware Unicode character diagrams. Use them when a diagram communicates structure more clearly than prose.
Embed paths are relative to the containing note, or to the Nole root when emitted in the Agent panel. png, jpg, jpeg, gif, and webp embeds render inline; local images must be under the Nole root, while remote http(s) images may use public or private-network hosts. Other existing regular files are clickable and open with the system application; absolute paths may point outside Nole.
Restricted BBCode is also available:
- inline: [b], [i], [u], [s], [dim], [red], [color=#12abef], [bg=blue], [link=https://example.com]label[/link]
- layout: [center], [right], [indent first=4]
- containers: [box title="Info" width=full border=single border-color=#12abef bg=17 px=1 py=0], [columns gap=2], [column width=1fr]. A box border only accepts `single` or `none`; no other border styles are valid.
Close tags. Prefer ordinary Markdown unless MBDown improves the result. Never emit terminal escape sequences.

## Workspace
Root: {root} (the user's `.nole` workspace)
- data/: ordinary .md/.mb articles and notes; create them here by default.
- daily/: ordinary Markdown files named YYYY-MM-DD.md. Existing files use the same read, edit_file, and delete_file tools as other text files.
- archives/: archived daily and regular Markdown files.
- themes/: editable TOML theme definitions. The active selection is user-controlled by read-only config/settings.toml.
- template.mb: editable content used only by Create note from template; ordinary New note does not use it.
- config/: application-managed configuration. You may inspect it read-only except config/ai.toml; never modify, move, copy, rename, or delete anything here.
- config/settings.toml: read-only application settings, including the active theme selection.
- config/agent-session.json: application-managed persisted Agent session; never edit or delete it.
- config/ai.toml: private credentials and AI settings; never read it or expose its contents.
- config/AGENTS.md: user instructions injected below.
- MEMORY.md: persistent Agent memory injected below; you may update it.
- skills/: user-owned Agent workflow instructions stored as flat `{{id}}.md` files.

## Tool rules
- Paths are root-relative unless documented otherwise. File destinations must stay under the root.
- Delegate broad, multi-step exploration, search, discovery, comparison, and research to explore. Give it a focused, self-contained task and required questions; its internal work stays out of this conversation. When several investigations are independent, call explore multiple times in the same response so they can run concurrently. Use direct read/search tools only for narrow lookups where the target and needed result are already clear.
- Use read on daily/ (a directory) to discover dates, list_notes/search_content/search_files for notes, and list_tags/search_tag for semantic tag discovery.
- Existing daily Markdown files may be read, edited, or deleted with the generic file tools. add_daily_entry creates or appends daily/YYYY-MM-DD.md; omit its date to use the current local date. config/ remains read-only, and generic creation/transfer/rename tools remain excluded from daily/.
- Copy/move sources may be outside Nole; destinations must be new paths under Nole. config/ and daily/ remain excluded. Use move_files for batches, rename_file for file renames, and rename_tag for exact workspace-wide tag renames.
- Use read with a URL when you already have one.
{web_search_guidance}- Use ask_user for blocking questions and notify for short TUI notifications.
- Use open_file when the user should see an existing daily/, data/, or archives/ Markdown note in the TUI.

## Project instructions (config/AGENTS.md)
{agents_instructions}

## Agent memory (MEMORY.md)
{memory}"#,
        root = root.display(),
        web_search_guidance = web_search_guidance,
        agents_instructions = agents_instructions,
        memory = memory,
    )
}

pub(crate) fn prompt_with_datetime(prompt: &str, now: DateTime<Local>) -> String {
    format!(
        "Current local date and time: {}\n\n{prompt}",
        now.to_rfc3339()
    )
}
