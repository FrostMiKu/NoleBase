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
  appended to that article without leaving it; the viewport follows the newly
  appended content to the end.

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

CommonMark images (`![alt](source)`) render in Daily cards, document views, and
Agent messages. Nole detects Kitty, Sixel, and iTerm2 graphics after entering
the alternate screen and otherwise falls back to true-color Unicode half
blocks. Relative local sources resolve from the containing note (or `daily/`
for Daily cards) and must remain inside the Nole root; HTTP(S) sources are
loaded through at most five validated redirects. PNG, JPEG, GIF first frames,
and WebP are supported.
Downloads are limited to 8 MB and decoded images to 4096x4096 with a 64 MB
allocation budget. Localhost, private-network, and link-local remote targets are
rejected. Images reserve twelve terminal rows, scale proportionally,
and are sliced to the visible virtual-scroll window. While loading, or after a
failure, the alt text remains visible.

Rendered Markdown links and `[link=...]...[/link]` labels are clickable and open
with the system default application. Clicking `[[wikilink]]` searches both
`data/` and `archives/` by filename or filename stem. Multiple MD/MB matches
open a chooser showing archive and format metadata; a missing note is created
as a new `.md` file under `data/`.

Hashtags are an exact navigation layer over workspace search. Clicking a
`#tag` in Daily or a document opens all lines carrying that exact tag, so
`#rust` does not include `#rustlang`. `Tags: Browse` in the command palette
lists tags by document count and mention count. `Tags: Rename` performs an
exact, workspace-wide rename without changing code spans, escaped text, or
longer tag names. Nole builds this index for
`daily/`, `data/`, and `archives/` on a background thread, then updates it
incrementally from file-watcher events. Typing in global search queries the
in-memory snapshot and never rescans files on the UI thread. Search results
remain grouped as Daily, Notes, then Archives.

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
  to the current article, keeps it open, and scrolls directly to the new content.
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
config/         # private application configuration
  ai.toml       # Anthropic and optional Tavily configuration
  settings.toml # selected theme
  AGENTS.md      # user-authored Agent instructions
themes/         # Agent-editable application and MBDown themes
  default.toml   # generated current default colors
  <name>.toml    # additional custom themes
MEMORY.md       # Agent-maintained persistent memory
template.mb     # initial content for "Note: New from template"
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

### Theme

On first start Nole creates `themes/default.toml` with its current colors and
writes `theme = "default"` to `config/settings.toml`. Each direct
`themes/<name>.toml` file contains semantic `#RRGGBB` tokens grouped under
`[surface]`, `[selection]`, `[text]`, `[ui]`, `[markdown]`, and `[animation]`.
The `[selection]` group defines the background, inactive background, foreground,
and indicator color shared by selectable lists. The reserved
`default` option uses `themes/default.toml`, falling back to Nole's built-in
colors if that file is absent. `random` chooses one valid custom theme when it
is selected and on each startup. A selected custom theme that does not exist
also falls back to `default`.

Regular color tokens accept either `#RRGGBB` or `"terminal"`; the latter uses
the terminal's own default color and is especially useful for `surface.canvas`
and `surface.status_bar`. Animation gradient entries must remain `#RRGGBB`.

Use `Theme: Switch` from the command palette to choose `default`, `random`, or
any custom theme. The selection is saved to `config/settings.toml`. Changes to
that file or to a direct TOML file under `themes/` are loaded automatically.
Because `themes/` is outside `config/`, the Agent can create and edit custom
themes without gaining access to private configuration.

### AI agent

On first start Nole creates `config/ai.toml` with private file permissions:

```toml
api_key = ""
tavily_api_key = ""
model = "claude-sonnet-4-5"
base_url = "https://api.anthropic.com"
max_tokens = 8192
context_window_tokens = 200000
max_rounds = 25
```

Set the Anthropic API key directly in `api_key`. The card's `AI` button runs the
Anthropic Messages API in the background. It first opens a prompt dialog; an
empty prompt sends the source card content. The lower two-thirds of the right
sidebar shows a chronological Agent timeline: user prompts, tool activity,
intermediate text, and final responses; Todo uses the upper third.
`max_rounds` is the request-round budget for one Agent segment, not a tool-call
limit; one response may call several tools. At the limit, Nole rings the terminal
bell and asks whether to continue with another segment or stop. Stopping keeps
the completed conversation and tool history, so a later prompt can continue it.
`context_window_tokens` is the model's total context size. Nole reserves
`max_tokens` for the next response and, before the remaining input budget is
exhausted, uses Anthropic's token-counting endpoint and replaces a safe prefix
of older completed turns with a dense summary. The system prompt, current turn,
recent history, and complete `tool_use`/`tool_result` pairs are retained.
Compatible endpoints without token counting fall back to a conservative local
estimate.
If a compatible endpoint stops at `max_tokens`, Nole preserves any partial text
as intermediate output and automatically requests continuation. A response
with no text is retried a limited number of times; persistent failures report
the stop reason and returned content-block types.
Agent output enters a daily card only when the Agent explicitly calls
`append_daily`.

Set `tavily_api_key` to enable the Agent's Tavily `web_search` tool. When the
key is empty or absent, Nole omits the tool and its instructions entirely, so
the Agent does not know that web search is available.

While the Agent is running, its panel border carries a moving color gradient
and the current tool uses the same animated full-text color gradient. Messages
API text is streamed into the current Agent entry and rendered as MBDown. The
panel header shows request rounds plus session-cumulative input/output tokens,
observed output throughput in `t/s`, and cache-read tokens with their share of
total input tokens. These statistics reset only when the Agent session is
cleared. Multiple tool calls returned in one model response still count as one
round.
Press `Ctrl+P` to open the fuzzy command palette. Commands run through one
application command pipeline; the initial commands interrupt the active Agent
task, clear its in-memory session, create or manage notes, or open `template.mb`,
`ai.toml`, `AGENTS.md`, and `MEMORY.md` with `$EDITOR`.
The existing `c` and `C` Agent-panel shortcuts invoke those same commands.
Agent conversations persist across completed prompts. Continue in the compose
box with `Ctrl+Enter`; the Agent receives the completed conversation history.
The Agent can inspect the same shared tag index with `list_tags` and
`search_tag`. Its `rename_tag` tool shows a multi-file diff and follows the
normal approval/bypass policy before changing exact Hashtag source spans.
You can also press `Ctrl+Enter` while the Agent is running. Nole combines all
such prompts in one buffer and delivers them before the next pending tool call.
An in-flight tool is allowed to finish, while later unstarted calls from the
old plan are deferred so the Agent can reconsider them with the new input. A
follow-up appears at the end of the timeline in muted text while queued, then
uses normal MBDown colors once the Agent consumes it. Final responses and later
prompts append to the same virtual-scrolling timeline. Only clearing the Agent
session removes panel history.
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

`list_directory` lists any directory by absolute path or a path relative to
the Nole root. `depth=1` returns direct children and values up to 16 include
nested descendants without following symlinks. Each entry includes its type,
depth, extension, byte size, line count for files up to 1 MB, and creation and
modification timestamps. Results support metadata sorting and pagination; one
call scans at most 10,000 entries.

`list_notes` returns managed notes with their line count, creation and
modification timestamps, and byte size. Results can be sorted ascending or
descending by name or any of those metadata fields and paginated with
`offset`/`limit`.

`search_content` performs case-insensitive full-text search across daily cards,
active notes, and archived notes, in that order, returning daily dates or file
paths and line numbers. `search_files` uses the same case-insensitive fuzzy
filename matching as the Files sidebar. Both search tools support result
`offset`/`limit` pagination.

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
top-right corner and emits the terminal bell. Notifications are non-blocking
and expire automatically.
The `open_file` tool switches the TUI to an existing managed `.md` or `.mb`
note in `data/` or `archives/`, so the Agent can present relevant material to
the user directly.
The `ask_user` tool pauses the Agent and opens a TUI dialog for clarification.
The Agent may provide up to ten choices; use Up/Down and Enter to select one,
or type a different free-text response. Esc cancels the question. Questions
are interactive requests rather than permission checks, so APPROVE/BYPASS does
not skip them.

The system prompt requires the Agent to use `ask_user` when it needs an answer
before it can complete the current task. Later `Ctrl+Enter` prompts remain part
of the same in-memory conversation until `C` is pressed or Nole exits.

Nole creates an empty `template.mb`. `Note: New from template` starts the new
note with its exact contents; regular note creation still generates the note title.
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
