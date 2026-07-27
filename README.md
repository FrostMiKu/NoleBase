# nole

A small terminal note app with a chat-style workflow. Capture text into one
daily card, then archive the day or move it into a Markdown note without
leaving the keyboard.

## Workspace

The UI is one responsive workspace rather than a collection of duplicated
popups:

- **Files** sits against the terminal's left edge.
- The right sidebar is split between **ToDo** and live **Agent output**.
- **Center** takes all remaining space and shows Daily, a document, or Search.
- Text inside Center is capped at **120 columns** and centered. The workspace
  itself still fills the terminal.
- At 170 columns and wider, all three panes are visible. On narrower terminals,
  the focused Files, ToDo, or Center surface fills the body without changing its
  state.
- **Compose** floats at the bottom of Daily on the same centered content axis.
- **Compose** remains available while reading a document, so content can be
  appended to that article without leaving it or losing its scroll position.

Files is a flat recent-files list, not a fake directory tree. Direct `.md` and
`.mb` files under the storage `data/` directory are sorted by last modification
time, newest first. Pressing `f` focuses this list; it never opens a second file
browser.

## Main workflow

Messages are appended to `daily/YYYY-MM-DD.md` and all content from one day is
shown as one card. Each card provides:

- `move` — select an existing note in Files
- `archive` — move the complete daily file into `archives/`
- `new` — name a new note in Files and move it there
- `edit` — edit that message in `$EDITOR` through a temporary Markdown file
- `del` — delete it after confirmation
- `AI` — open an optional prompt, then run the configured Anthropic agent

Messages render Markdown directly in Daily. Press `v` when a dedicated document
view is useful for scrolling through a long message.

## Markup

Daily cards and document views parse the shared MBDown language with `mbdown`,
then render its AST directly to Ratatui with `mbtui`. There is no ANSI
round-trip. In addition to CommonMark, notes may use restricted BBCode for
terminal colors, backgrounds, boxes, and responsive columns. MBDown also
recognizes `#tag` and `[[wikilink]]` references in ordinary text:

```text
[box title="Status" width=full border=single bg=17]
[color=bright-cyan]Ready[/color]
[/box]

[columns gap=2]
[column width=1fr]Left[/column]
[column width=2fr bg=#202830]Right[/column]
[/columns]
```

Columns stack when the center pane is too narrow. Widths are Unicode terminal
columns, and background colors fill the complete Box or column rectangle. The
full syntax rules live in the sibling MBDown workspace.

Rendered Markdown links and `[link=...]...[/link]` labels are clickable and open
with the system default application. Clicking `[[wikilink]]` searches both
`data/` and `archives/` by filename or filename stem. Multiple MD/MB matches
open a chooser showing archive and format metadata; a missing note is created
as a new `.md` file under `data/`.

Opening a file displays it in Center. `Esc` closes it; `e` suspends the TUI and
opens that file in `$EDITOR` (then `$VISUAL`, then `vi`). Search and message
editing also use Center instead of covering the workspace with a popup. External
changes to `.md` and `.mb` files under the note directory are detected
automatically; Daily, ToDo, Files, Search, and an open document refresh without
restarting Nole.

## Keybindings

### Compose

| Key | Action |
| --- | --- |
| type / paste | edit the compose buffer; multiline paste is preserved |
| `Enter` | send to Daily, or append to the article currently being viewed |
| `Ctrl+Enter` | send the buffer directly to Agent without creating a Daily card |
| `Shift`/`Alt`+`Enter`, `Ctrl+J` | insert a newline |
| arrows, `Home`, `End` | move the cursor |
| `Esc` | focus Daily |
| `Tab` | toggle Agent permission mode |
| `Ctrl+C` | clear the input; quit when already empty |

### Daily

| Key | Action |
| --- | --- |
| `j`/`k`, `↓`/`↑` | select a message |
| `g` / `G` | first / last message |
| `m` / `a` / `n` | move / archive / new note |
| `v` / `e` / `d` | view / edit / delete selected message |
| `u` | undo the last move, delete, or edit |
| `/` | search messages and files in Center |
| `f` / `T` | focus Files / ToDo |
| `i`, `Enter` | focus Compose |
| `Tab` | toggle Agent permission mode |
| `?` | Help |
| `q` | quit |
| `Esc` | quit |

### Files

| Key | Action |
| --- | --- |
| `f` | refresh and focus Files |
| `j`/`k`, `↓`/`↑`, mouse wheel | select a file |
| `Enter`, `v`, click | open the file in Center |
| `e` | open the selected file in `$EDITOR` |
| `/` | filter directly inside Files |
| `r` / `d` | rename inline / delete with confirmation |
| `Esc`, `q` | return to Center |

During a message move, Files becomes the target picker. During new-file and
rename operations, the input appears at the top of the same Files surface.
Errors leave the active input/context in place so they can be corrected.

### ToDo, documents, Search, and edit

- **ToDo:** scans task-list items from every file in `daily/`. `j`/`k` or
  arrows select; `Enter`, Space, or `x` toggles the checkbox in its source
  daily file; `Esc`/`q` returns to Center.
- **Document:** arrows or `j`/`k` scroll; `PageUp`/`PageDown` move by pages;
  `i` or Enter focuses Compose; `Esc`/`q` closes. Sending from Compose appends
  to the current article while keeping it open and shows a top-right notification.
  `Ctrl+Enter` instead sends the buffer directly to Agent and includes the path
  of the note currently being viewed as context.
  On a file, `e` invokes `$EDITOR`; on a message, `e` opens the in-app message
  editor. `/` opens the same search surface as workspace search, scoped to the
  current article; Enter jumps to the selected source line and Esc returns to
  the article.
- **Search:** type to filter; arrows select; `Enter` or click opens a result;
  `Esc` returns to Daily. Closing a search result first returns to Search.

Message card edits suspend the TUI and open a temporary `.md` file in
`$EDITOR` (then `$VISUAL`, then `vi`). When the editor exits successfully, Nole
writes the content back to the original daily date and removes the temporary
file. Editing from a message preview keeps that preview open and refreshes it.

Mouse activation uses only the left button. The wheel scrolls the pane under the
pointer, and confirmations/Help block all interaction with the workspace below.
`Tab` globally switches between approval mode and bypass mode without changing
keyboard focus.

## Storage

Data lives under `${NOLE_DIR}` when that environment variable is set, otherwise
under `~/.nole`:

```text
config/        # reserved for configuration
  ai.toml       # Anthropic and optional Tavily configuration
  AGENTS.md      # user-authored Agent instructions
MEMORY.md       # Agent-maintained persistent memory
daily/         # chat cards; absent dates have no file
  YYYY-MM-DD.md
archives/      # flat storage for archived daily cards and articles
  YYYY-MM-DD.md
  <name>.md
  <name>.mb
data/          # flat note storage
  <name>.md
  <name>.mb
```

`.md` and `.mb` extensions are recognized case-insensitively. NoleBase shows
direct, regular files from both `data/` and `archives/` as separate Notes and
Archives groups; symlinks and nested paths are rejected. Startup creates
`daily/` and `archives/`, but a daily file is created only when content is first
sent for that date. Later sends append with a blank line separator. Archiving an
article moves it from `data/` to `archives/`; restoring it moves it back without
overwriting an existing file.

### AI agent

On first start Nole creates `config/ai.toml` with private file permissions:

```toml
api_key = ""
tavily_api_key = ""
model = "claude-sonnet-4-5"
base_url = "https://api.anthropic.com"
max_tokens = 4096
max_rounds = 25
```

Set the Anthropic API key directly in `api_key`. The card's `AI` button runs the
Anthropic Messages API in the background. It first opens a prompt dialog; an
empty prompt sends the source card content. Tool calls and Agent state appear in
the bottom status bar. The lower two-thirds of the right sidebar shows the
user's prompt followed by the Agent's final reply; Todo uses the upper third.
`max_rounds` limits Messages API request rounds for each submitted prompt, not
the lifetime of the in-memory conversation; one response may call several tools.
Agent output enters the current daily card only when
the Agent explicitly calls `append_daily`, or when the Agent panel is focused
and you press Enter. The latter appends to the current Daily card, clears the
Agent panel, and returns focus to the center view.

Set `tavily_api_key` to enable the Agent's Tavily `web_search` tool. When the
key is empty or absent, Nole omits the tool and its instructions entirely, so
the Agent does not know that web search is available.

While the Agent is running, its panel border carries a moving color gradient
and the panel lists tool activity. A color highlight advances character by
character across the current activity item; the bottom status text stays
static. The panel title shows request rounds against the configured limit plus
input/output token counts, for example `↻3/25 · ↑12.4k ↓842`. Multiple tool
calls returned in one model response still count as one round. When the Agent
finishes, its final response replaces the activity log.
Agent conversations persist across completed prompts. Continue in the compose
box with `Ctrl+Enter`; the Agent receives the completed conversation history.
Focus the Agent panel and press `c` to cancel the current task, or `C` to clear
the conversation and start a new session. Cancellation is
cooperative: no later tools will start, although an in-flight HTTP request or
tool call may need to return before its worker thread exits.

All scrollable TUI surfaces use virtual row windows. Daily cards, note previews,
Agent output, approval diffs, help, searches, file/Todo lists, and multiline
inputs submit only their currently visible rows to Ratatui; off-screen rows are
retained as scroll state rather than rendered.

The Agent can read arbitrary text files with zero-based `offset`/`limit` line
pagination, write only inside the Nole directory, and fetch HTTP(S) text. When
configured, `web_search` queries Tavily with optional topic, depth, time range,
answer, and result-count controls, then returns compact ranked results.
Every user prompt sent to the Agent includes the current local date and time.
`read_file` defaults to 200 lines and accepts at most 2,000 lines per call. Its
structured response includes the total line count and whether more content
remains.

`list_notes` returns managed notes with their line count, creation and
modification timestamps, and byte size. Results can be sorted ascending or
descending by name or any of those metadata fields and paginated with
`offset`/`limit`.

`search_content` performs case-insensitive full-text search across daily cards
and managed note contents, returning daily dates or file paths and line
numbers. `search_files` uses the same case-insensitive fuzzy filename matching
as the Files sidebar. Both search tools support result `offset`/`limit`
pagination.

`write_file` creates new files and refuses existing paths. `update_file` changes
existing files, while `read_daily`, `update_daily`, and `append_daily` provide
date-based access to daily cards without exposing `daily/` to generic file
tools. `read_daily` accepts an inclusive `start_date`/`end_date` range and
returns every existing card in it; use equal bounds for one day. `update_file`
accepts one or more zero-based `[start_line, end_line)` replacements and
preserves the rest of the file internally, so large files do not need to be
read or submitted in full. Changed/deleted ranges must have been covered by
`read_file` in the same Agent run; insertions require adjacent anchor lines.
Daily updates require a prior range read containing the exact date.

`copy_file` and `move_file` accept a regular source file anywhere on the
filesystem, but the destination must be a new path inside the Nole directory;
neither operation requires approval. `move_files` moves up to 200 sources into
one existing Nole directory, preserves basenames, preflights all collisions,
and attempts rollback if a later move fails. `rename_file` gives same-directory
renames an explicit non-overwriting operation. `delete_file` only accepts
regular files inside Nole and uses the common approval dialog. Generic file
tools cannot operate directly inside `daily/` or on `config/ai.toml`.

The `notify` tool lets the Agent display a short notification card in the TUI's
top-right corner. Notifications are non-blocking and expire automatically.
The `ask_user` tool pauses the Agent and opens a TUI dialog for clarification.
The Agent may provide up to ten choices; use Up/Down and Enter to select one,
or type a different free-text response. Esc cancels the question. Questions
are interactive requests rather than permission checks, so APPROVE/BYPASS does
not skip them.

The system prompt requires the Agent to use `ask_user` when it needs an answer
before it can complete the current task. Later `Ctrl+Enter` prompts remain part
of the same in-memory conversation until `C` is pressed or Nole exits.

Nole also creates empty `config/AGENTS.md` and `MEMORY.md` files. Their complete
contents are appended to the system prompt in that order for every Agent task.
`config/AGENTS.md` is user-owned: Agent file tools cannot mutate anything in
`config/`. The Agent may read and update root-level `MEMORY.md` through the
normal read-before-update and approval flow.

In `APPROVE` mode, updates and deletes pause and show an MBTUI-rendered diff or
deletion preview. Use Enter/Y to approve or N/Esc to deny. In `BYPASS` mode
they proceed without the approval dialog, but the read-before-update rule still
applies. Adding a new card never requires approval. Note listings return at
most 2,000 entries per call; file and web responses are capped at 1 MB.
Filesystem mutation tools reject symlink targets. The API configuration itself
is not exposed to tools.

## Build and check

The workspace expects `nole` and the MBDown workspace to be
sibling directories:

```text
Codes/
  mbdown/
  nole/
```

```bash
cargo run -q
cargo build --release
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```
