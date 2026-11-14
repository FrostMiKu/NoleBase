# NólëBase

NólëBase (Nole) is a local-first knowledge workspace for capturing ideas,
building connected notes, and working with an AI Agent. It brings daily records,
Markdown notes, tasks, and Agent conversations together in a terminal interface
and a native desktop client.

Start with a thought, a task, or a question. Capture it in Daily, develop it into
a note, and connect it to what you already know. When you need help, the Agent
can work directly with your workspace to find information, organize material,
and turn rough notes into something useful.

Nole is under active development and has yet to reach its first release.

## From daily capture to lasting knowledge

- **Capture as you go.** Write ideas, meeting notes, and task lists in Daily.
  Entries are saved by date, giving you a chronological record to return to.
- **Give ideas a home.** Move daily entries into named Markdown notes, start
  notes from a template, and archive material as your work evolves.
- **Build connections.** Use `[[wikilinks]]`, backlinks, and `#tags` to connect
  related notes. Search across daily records, notes, and archives to pick up a
  thread wherever it began.
- **Keep tasks close to their context.** Markdown task lists in Daily feed a
  shared Todo view, where completion updates the original record.
- **Work with rich material.** Notes support tables, code blocks, images,
  attachments, LaTeX math, and Mermaid diagrams. The terminal client can export
  notes as Markdown or standalone HTML for sharing and offline reading.

## An Agent that works with your knowledge

The Agent can read and search your workspace, edit notes, organize files, and
help carry a task through to completion. Conversations are saved as sessions so
you can return to ongoing work.

Ask it to summarize a week of daily records, connect scattered notes on a topic,
turn meeting notes into a project outline, or research a question and save the
findings. Web search is available with a Tavily API key; shell and terminal tools
extend the Agent to work that involves local commands.

You choose the model provider and how actions are approved:

- **APPROVE** asks you to review file changes and sensitive actions.
- **AUTO** approves file changes inside the Nole workspace and asks for approval
  for changes elsewhere; shell commands still require approval.
- **YOLO** lets the Agent proceed directly, with built-in file and command safety
  checks active.

Custom instructions and persistent memory help the Agent follow your working
habits across sessions.

## One workspace, two interfaces

**Terminal:** a keyboard-driven workspace with Daily, notes, Search, Todo, and
live Agent output. Capture and navigation stay close at hand, with Markdown
rendering, an external editor, attachment management, and export tools.

**Desktop:** a native client with a file tree, note previews, Markdown reading
and editing, Daily, Search, Tags, Todo, and Agent conversations. Edits save
automatically, and Agent actions support streaming replies, approvals, and
interactive questions.

Both clients use the same local workspace, Agent sessions, and themes.

## Your knowledge lives in local files

Nole stores its workspace in `~/.nole`, or in the directory selected by
`NOLE_DIR`. Daily records and notes are plain Markdown files that you can read,
edit, and back up with your own tools. External edits refresh in the app.

The main folders are:

```text
daily/       Daily records, grouped by date
data/        Active notes
archives/    Archived notes
attachments/ Managed attachments
agents/      Saved Agent sessions
config/      Application settings and Agent instructions
themes/      Custom themes
```

Capture, editing, and local search work independently of the Agent. Agent
requests send conversation context and tool results to your configured model
provider.

## Try Nole

Build from source with Rust 1.100 or newer:

```sh
git clone https://github.com/FrostMiKu/NoleBase.git
cd NoleBase
cargo run --release --bin nole
```

To launch the native desktop client, use the native GPUI platform toolchain:

```sh
cargo run --release --no-default-features --features gui --bin nole-gui
```

On macOS, the desktop client requires Xcode Command Line Tools.

The first launch creates the workspace and configuration files. Start by writing
an entry in Daily. In the terminal client, `Enter` saves your entry, `Ctrl+Enter`
sends a prompt to the Agent, `Ctrl+P` opens the command palette, and `?` opens Help
while Daily is focused.

### Connect an AI provider

Open `~/.nole/config/ai.toml` (or `config/ai.toml` in your chosen workspace) and
set your provider's API key, model, and base URL. Nole supports the Anthropic
Messages protocol (`api_format = "messages"`) and the OpenAI-compatible Chat
Completions protocol (`api_format = "completions"`), including local endpoints.

Use the host prefix for `base_url`, such as `https://api.anthropic.com`; Nole adds
the protocol's `/v1/...` path. Set `context_window_tokens` and `max_tokens` to
match your model. Add `tavily_api_key` to enable web search.

Appearance, editor, and export preferences live in `config/settings.toml`.
Custom Agent instructions live in `config/AGENTS.md`.

## Development

Nole is written in Rust, with Ratatui for the terminal client and GPUI Kit for
the desktop client. Both share the workspace and Agent core.

```sh
cargo fmt --check
cargo test --locked
cargo test --locked --no-default-features
cargo clippy --all-targets -- -D warnings
```

Nole is licensed under MIT. Bundled third-party notices are in
[`assets/licenses/`](assets/licenses/).
