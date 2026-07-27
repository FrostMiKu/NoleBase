# nole

A small terminal note app with a chat-style workflow. Capture text quickly, then
move each message into ToDo, Archive, or a Markdown note without leaving the
keyboard.

## Workspace

The UI is one responsive workspace rather than a collection of duplicated
popups:

- **Files** sits against the terminal's left edge.
- The right sidebar is split between **ToDo** and live **Agent output**.
- **Center** takes all remaining space and shows Chat, a document, or Search.
- Text inside Center is capped at **120 columns** and centered. The workspace
  itself still fills the terminal.
- At 170 columns and wider, all three panes are visible. On narrower terminals,
  the focused Files, ToDo, or Center surface fills the body without changing its
  state.
- **Compose** floats at the bottom of Chat on the same centered content axis.
- **Compose** remains available while reading a document, so notes can be
  captured without leaving the article or losing its scroll position.

Files is a flat recent-files list, not a fake directory tree. Direct `.md` and
`.mb` files under the storage `data/` directory are sorted by last modification
time, newest first. Pressing `f` focuses this list; it never opens a second file
browser.

## Main workflow

Messages are stored in `CHAT.md` and shown as cards. Each card provides:

- `todo` — move the message to `TODO.md`
- `move` — select an existing note in Files
- `archive` — move it to `ARCHIVE.md`
- `new` — name a new note in Files and move it there
- `edit` — edit that message in `$EDITOR` through a temporary Markdown file
- `del` — delete it after confirmation
- `AI` — open an optional prompt, then run the configured Anthropic agent

Messages render Markdown directly in Chat. Press `v` when a dedicated document
view is useful for scrolling through a long message.

## Markup

Chat cards and document views parse the shared MBDown language with `mbdown`,
then render its AST directly to Ratatui with `mbtui`. There is no ANSI
round-trip. In addition to CommonMark, notes may use restricted BBCode for
terminal colors, backgrounds, boxes, and responsive columns:

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

Opening a file displays it in Center. `Esc` closes it; `e` suspends the TUI and
opens that file in `$EDITOR` (then `$VISUAL`, then `vi`). Search and message
editing also use Center instead of covering the workspace with a popup. External
changes to `.md` and `.mb` files under the note directory are detected
automatically; Chat, ToDo, Files, Search, and an open document refresh without
restarting Nole.

## Keybindings

### Compose

| Key | Action |
| --- | --- |
| type / paste | edit the compose buffer; multiline paste is preserved |
| `Enter` | send |
| `Ctrl+Enter` | send the buffer directly to Agent without creating a Chat card |
| `Shift`/`Alt`+`Enter`, `Ctrl+J` | insert a newline |
| arrows, `Home`, `End` | move the cursor |
| `Esc` | focus Chat |
| `Tab` | toggle Agent permission mode |
| `Ctrl+C` | clear the input; quit when already empty |

### Chat

| Key | Action |
| --- | --- |
| `j`/`k`, `↓`/`↑` | select a message |
| `g` / `G` | first / last message |
| `t` / `m` / `a` / `n` | ToDo / move / archive / new note |
| `v` / `e` / `d` | view / edit / delete selected message |
| `u` | undo the last move, delete, or edit |
| `/` | search messages and files in Center |
| `f` / `T` | focus Files / ToDo |
| `i`, `Enter` | focus Compose |
| `Tab` | toggle Agent permission mode |
| `?` | Help |
| `q` | quit |
| `Esc` | stay at the base workspace (does not quit) |

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

- **ToDo:** `j`/`k` or arrows select; `Enter`, Space, or `x` toggles; `Esc`/`q`
  returns to Center.
- **Document:** arrows or `j`/`k` scroll; `PageUp`/`PageDown` move by pages;
  `i` or Enter focuses Compose; `Esc`/`q` closes. Sending from Compose records a
  Chat card while keeping the document open and shows a top-right notification.
  `Ctrl+Enter` instead sends the buffer directly to Agent and includes the path
  of the note currently being viewed as context.
  On a file, `e` invokes `$EDITOR`; on a message, `e` opens the in-app message
  editor.
- **Search:** type to filter; arrows select; `Enter` or click opens a result;
  `Esc` returns to Chat. Closing a search result first returns to Search.

Message card edits suspend the TUI and open a temporary `.md` file in
`$EDITOR` (then `$VISUAL`, then `vi`). When the editor exits successfully, Nole
writes the content back to the original nole-msg id and removes the temporary
file. Editing from a message preview keeps that preview open and refreshes it.

Mouse activation uses only the left button. The wheel scrolls the pane under the
pointer, and confirmations/Help block all interaction with the workspace below.
`Tab` globally switches between approval mode and bypass mode without changing
keyboard focus.

## Storage

Data lives under `${NOLE_DIR}` when that environment variable is set, otherwise
under `~/.nole`:

```text
CHAT.md        # chat stream
TODO.md        # tasks
ARCHIVE.md     # archive
config/        # reserved for configuration
  ai.toml       # Anthropic Messages API configuration
data/          # flat note storage
  <name>.md
  <name>.mb
```

`.md` and `.mb` extensions are recognized case-insensitively. Files manages only
direct, regular files in `data/`; symlinks, nested paths, and paths outside that
directory are rejected. `CHAT.md`, `TODO.md`, and `ARCHIVE.md` stay at the
storage root and are protected from rename/delete. On startup, ordinary `.md`
and `.mb` files from the legacy root layout move into `data/`. Existing files in
`data/` are never overwritten when a legacy name conflicts.

`CHAT.md` stores each message in a hidden block:

```markdown
<!-- nole-msg id="…" created_at="2026-06-24T10:00:00+08:00" -->
your message body
<!-- /nole-msg -->
```

Moves and deletes edit only the relevant block, preserving unrelated manual
changes.

### AI agent

On first start Nole creates `config/ai.toml` with private file permissions:

```toml
api_key = ""
model = "claude-sonnet-4-5"
base_url = "https://api.anthropic.com"
max_tokens = 4096
```

Set the Anthropic API key directly in `api_key`. The card's `AI` button runs the
Anthropic Messages API in the background. It first opens a prompt dialog; an
empty prompt sends the source card content. Tool calls and Agent state appear in
the bottom status bar. The lower two-thirds of the right sidebar shows the
user's prompt followed by the Agent's final reply; Todo uses the upper third.
Agent output enters Chat only when
the Agent explicitly calls `add_message`, or when the Agent panel is focused
and you press Enter. The latter creates one Chat card, clears the Agent panel,
and returns focus to the center view.

While the Agent is running, its panel border carries a moving color gradient
and a color highlight advances character by character across the stationary
bottom status text. Both return to the normal static theme as soon as the Agent
stops.

All scrollable TUI surfaces use virtual row windows. Chat cards, note previews,
Agent output, approval diffs, help, searches, file/Todo lists, and multiline
inputs submit only their currently visible rows to Ratatui; off-screen rows are
retained as scroll state rather than rendered.

The Agent can read arbitrary text files with zero-based `offset`/`limit` line
pagination, list directories with bounded recursion, write only inside the
Nole directory, and fetch HTTP(S) text. `read_file` defaults to 200 lines and
accepts at most 2,000 lines per call. Its structured response includes the
total line count and whether more content remains.

`search_content` performs case-insensitive full-text search across Chat cards
and managed note contents, returning message ids or file paths and line
numbers. `search_files` uses the same case-insensitive fuzzy filename matching
as the Files sidebar. Both search tools support result `offset`/`limit`
pagination.

`write_file` creates new files and refuses existing paths. `update_file` changes
existing files, while `read_message`, `update_message`, and `add_message`
provide id-based access to Chat without exposing `CHAT.md` to generic file
tools. A file update is rejected until every line of that exact file has been
covered by one or more `read_file` calls in the same Agent run. Message updates
still require `read_message` for the exact id.

The `notify` tool lets the Agent display a short notification card in the TUI's
top-right corner. Notifications are non-blocking and expire automatically.
The `ask_user` tool pauses the Agent and opens a TUI dialog for clarification.
The Agent may provide up to ten choices; use Up/Down and Enter to select one,
or type a different free-text response. Esc cancels the question. Questions
are interactive requests rather than permission checks, so APPROVE/BYPASS does
not skip them.

In `APPROVE` mode, updates pause and show an MBTUI-rendered unified diff. Use
Enter/Y to approve or N/Esc to deny. In `BYPASS` mode updates proceed without
the approval dialog, but the read-before-update rule still applies. Adding a
new card never requires approval. Directory listings are capped at 2,000
entries and depth 10; file and web responses are capped at 1 MB. Symlinks are
listed but never followed. The API configuration itself is not exposed to
tools.

## Build and check

The workspace expects `nole` and the MBDown workspace to be
sibling directories:

```text
Codes/
  mbterm/
  nole/
```

```bash
cargo run -q
cargo build --release
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```
