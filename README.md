# note

A small terminal note app with a chat-style workflow. Capture text quickly, then
move each message into ToDo, Archive, or a Markdown note without leaving the
keyboard.

## Workspace

The UI is one responsive workspace rather than a collection of duplicated
popups:

- **Files** sits against the terminal's left edge.
- **ToDo** sits against the right edge.
- **Center** takes all remaining space and shows Chat, a document, Search, or the
  message editor.
- Text inside Center is capped at **120 columns** and centered. The workspace
  itself still fills the terminal.
- At 170 columns and wider, all three panes are visible. On narrower terminals,
  the focused Files, ToDo, or Center surface fills the body without changing its
  state.
- **Compose** floats at the bottom of Chat on the same centered content axis.

Files is a flat recent-files list, not a fake directory tree. Direct Markdown
files under the note directory are sorted by last modification time, newest
first. Pressing `f` focuses this list; it never opens a second file browser.

## Main workflow

Messages are stored in `CHAT.md` and shown as cards. Each card provides:

- `todo` — move the message to `TODO.md`
- `move` — select an existing note in Files
- `archive` — move it to `ARCHIVE.md`
- `new` — name a new note in Files and move it there
- `edit` — edit that message in Center
- `del` — delete it after confirmation

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
changes to Markdown files under the note directory are detected automatically;
Chat, ToDo, Files, Search, and an open document refresh without restarting Note.

## Keybindings

### Compose

| Key | Action |
| --- | --- |
| type / paste | edit the compose buffer; multiline paste is preserved |
| `Enter` | send |
| `Shift`/`Ctrl`/`Alt`+`Enter`, `Ctrl+J` | insert a newline |
| arrows, `Home`, `End` | move the cursor |
| `Esc` / `Tab` | focus Chat |
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
| `i`, `Enter`, `Tab` | focus Compose |
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
  `Esc`/`q` closes. On a file, `e` invokes `$EDITOR`; on a message, `e` opens the
  in-app message editor.
- **Search:** type to filter; arrows select; `Enter` or click opens a result;
  `Esc` returns to Chat. Closing a search result first returns to Search.
- **Message edit:** `Enter` saves; modified Enter or `Ctrl+J` inserts a newline;
  `Esc` cancels and asks before discarding changed text.

Mouse activation uses only the left button. The wheel scrolls the pane under the
pointer, and confirmations/Help block all interaction with the workspace below.

## Storage

Data lives under `${NOTE_DIR}` when that environment variable is set, otherwise
under `~/.note`:

```text
CHAT.md       # chat stream
TODO.md       # tasks
ARCHIVE.md    # archive
<name>.md     # other notes
```

`.md` and `.markdown` extensions are recognized case-insensitively. Only direct,
regular files in the storage root are managed; symlinks, nested paths, and paths
outside the root are rejected. `CHAT.md`, `TODO.md`, and `ARCHIVE.md` are
protected from rename/delete, and protected names cannot be created. `CHAT.md`
is excluded from Files; ToDo and Archive remain readable and externally
editable.

`CHAT.md` stores each message in a hidden block:

```markdown
<!-- note-msg id="…" created_at="2026-06-24T10:00:00+08:00" -->
your message body
<!-- /note-msg -->
```

Moves and deletes edit only the relevant block, preserving unrelated manual
changes.

## Build and check

The workspace expects `note` and the MBDown workspace to be
sibling directories:

```text
Codes/
  mbterm/
  note/
```

```bash
cargo run -q
cargo build --release
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```
