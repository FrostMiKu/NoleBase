# note

A small terminal note app with a chat-style UI. Record things like sending
messages, then triage each message into a todo, an existing markdown file, or a
new one — all without leaving the terminal.

## What it does

- **Chat-style input** — type or paste (including multi-line IM content) at the
  bottom. `Enter` inserts a newline; `Ctrl`/`Alt`+`Enter` sends. Messages are
  stored in `~/.note/CHAT.md` and shown in full (wrapped) in the chat list.
- **Per-message actions** — every message has buttons:
  - `todo` — move to `~/.note/TODO.md` as `- [ ] …`
  - `move` — move into an existing markdown file under `~/.note`
  - `new` — create a new markdown file and move the message there
  - `view` — open a full-content preview
  - `edit` — open `CHAT.md` in `$EDITOR`
  - `del` — delete the message (with confirmation)
- **File browser** — press `f` to pop up the list of `~/.note/*.md`; preview or
  edit any file from there.
- **Mouse + keyboard** — click any button, or use vim-like keys.

## Storage

All data lives under `~/.note`:

```
~/.note/CHAT.md   # the chat stream
~/.note/TODO.md   # accumulated tasks
~/.note/<name>.md # files you move things into
```

`CHAT.md` stores each message as a hidden HTML-comment block so that
delete/move stay reliable even when pasted content contains blank lines or
markdown:

```
<!-- note-msg id="…" created_at="2026-06-24T10:00:00+08:00" -->
your message body
<!-- /note-msg -->
```

Mutations are surgical (append a block, or remove the exact block for an id),
so manual edits made via `$EDITOR` are never clobbered.

## Keybindings

The app has two base modes plus overlay popups.

**Insert mode** (default, input box focused):

| Key            | Action                          |
| -------------- | ------------------------------- |
| type / paste   | record text (paste keeps newlines) |
| `Enter`        | insert a newline                |
| `Ctrl`/`Alt`+`Enter` | send the message           |
| `Tab` / `Esc`  | leave insert → Normal mode      |
| `Ctrl+C`       | clear input (or quit if empty)  |

> Plain `Ctrl+J` behaves like `Enter` (newline) — in raw mode it is delivered
> as a bare `Enter`. Use `Ctrl`/`Alt`+`Enter` to send.

**Normal mode** (message shortcuts active):

| Key            | Action                              |
| -------------- | ----------------------------------- |
| `j` / `↓` `k` / `↑` | select next / previous message |
| `t`            | move selected → `TODO.md`           |
| `m`            | move selected → existing file       |
| `n`            | move selected → new file            |
| `v`            | preview selected message            |
| `e`            | edit `CHAT.md` in `$EDITOR`         |
| `d`            | delete selected (confirm)           |
| `f`            | open the **Files** popup            |
| `i` / `Enter` / `Tab` | return to Insert mode        |
| `q`            | quit                                |

**Files popup** (`f`): `j`/`k` move, `Enter`/`v` preview the file, `e` edit it
in `$EDITOR`, `Esc`/`f`/`Tab` close. Clicking a file row previews it.

**In popups**: `↑`/`↓` navigate, `Enter` confirm, `Esc` cancel. In the
move-file list, `v` previews the highlighted file. In preview, `↑`/`↓`/`PgUp`/`PgDn` scroll.

The mouse works in any mode: click a button to act on that message, click a
file row in the popup to preview it, scroll to move the chat view.

## Files popup

Press `f` to pop up the list of every `.md` file under `~/.note` (excluding
`CHAT.md`), so messages you move are always findable. The list refreshes from
disk each time it opens; from there a file is one key (`v`) or click away from
a full preview, or `e` to open it in `$EDITOR`.

## `$EDITOR`

`view`'s companion `edit` suspends the TUI, opens `CHAT.md` in `$EDITOR`
(falling back to `$VISUAL`, then `vi`), and reloads on return. External edits
are preserved thanks to the surgical file model.

## Build & run

```bash
cargo run -q        # debug run
cargo build --release
```

Checks:

```bash
cargo fmt
cargo test
cargo clippy -- -D warnings
```
