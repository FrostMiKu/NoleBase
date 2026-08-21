# NólëBase

NólëBase is an Agent-driven terminal knowledge management system. It brings
daily capture, Markdown and MBDown notes, semantic tags, task tracking, and an
autonomous workspace Agent into one local-first TUI.

Knowledge stays in plain files under `~/.nole`. The Agent can search, read,
edit, organize, and connect that workspace through approval-gated tools, while
capture and navigation remain fast, direct, and keyboard-driven.

## Install

```sh
cargo install nole
nole
```

`nole --version` (or `-V`) prints the current version and exits.

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
- `new` — name a new note in Files and move it there
- `edit` — edit that message in the configured editor through a temporary Markdown file
- `del` — delete it after confirmation
- `AI` — ask the Agent to work on this daily file, or format it when left blank

Messages render Markdown directly in Daily. Press `v` when a dedicated document
view is useful for scrolling through a long message.

With a file, Skill, or Daily preview open in Center, run `File: Export…` from
the command palette. Choose **Original** for byte-for-byte publication or
**HTML** for a safe standalone `.html` document. The destination prompt starts
with the source name and correct extension, prefixed by the configured
`export_directory` setting (default `~`, the user's home directory). It accepts
an absolute path, a `~/...` path, or a path relative to the parent of the Nole
root. The destination must be outside Nole and its parent must already exist.
If the destination is an existing regular file, Nole asks for explicit
confirmation before atomically replacing it; directories, symlinks, and special
files are never replaceable. Publication never leaves a partial final file.
Rendering runs on a worker thread, so generation does not block keyboard input
or redraws; a background failure restores the destination prompt for retry.

Wikilinks resolve matching filenames across `daily/`, `data/`, and `archives/`.
When the same name exists in multiple locations, Nole shows a source-labelled
chooser instead of silently preferring one copy.

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
full syntax rules are documented in the standalone
[`mbdown`](https://crates.io/crates/mbdown) parser crate, and its native
Ratatui renderer is published as [`mbtui`](https://crates.io/crates/mbtui).

Fenced `mermaid` blocks render directly as width-aware Unicode character
diagrams. Rendering is local and does not require a browser, an image-capable
terminal, or a network service. Flowchart, sequence, state, class, ER, Gantt,
pie, mindmap, and other common Mermaid diagram types are supported. Invalid
diagrams, or diagrams that cannot fit the available width, remain visible as
ordinary source code blocks.

````markdown
```mermaid
flowchart LR
    Draft --> Review --> Published
```
````

CommonMark images (`![alt](source)`) render in Daily cards, document views, and
Agent messages. Nole detects Kitty, Sixel, and iTerm2 graphics after entering
the alternate screen and otherwise falls back to true-color Unicode half
blocks. Relative local sources resolve from the containing note (or `daily/`
for Daily cards) and must remain inside the Nole root; HTTP(S) sources are
loaded through at most five validated redirects. PNG, JPEG, GIF first frames,
and WebP are supported.
Downloads are limited to 8 MB and decoded images to 4096x4096 with a 64 MB
allocation budget. Localhost, private-network, and link-local HTTP(S) sources
are allowed. Remote bytes are shared across display sizes; transient network
and server failures are retried, and terminal failures are retried after a
short cooldown. Images reserve twelve terminal rows, scale proportionally, and
are sliced to the visible virtual-scroll window. While loading, or after a
failure, the alt text remains visible.

Standalone HTML export uses the offline MBDown renderer. Raw HTML, unsupported
MBDown markers, and unsafe links remain visible as escaped text instead of
executing; a restrictive CSP permits only the exact inlined renderers. Local
note links stay visibly marked but inert so a standalone document never leaks a
private Nole `file://` path. Supported local images are embedded after
root-containment, symlink, per-image, cumulative-size, and decoded-format
checks. Managed images are embedded only through valid
`nole://attachment/<uuid>` references; direct physical `attachments/` and
`config/` paths are rejected. Remote images are never fetched and remain
ordinary HTTP(S) links. Missing or broken images stay visible as explicit
fallbacks and are reported as export warnings. Fenced Mermaid diagrams,
inline/display math, and language-labelled fenced code render in the browser
through pinned Mermaid.js, KaTeX, and highlight.js runtimes embedded in the
single HTML file, including KaTeX fonts and syntax-highlighting styles;
rendering requires no network, and escaped source remains visible if a
renderer fails or does not recognize the language. The page pins readable
light defaults and chooses whichever black/white foreground has the stronger
WCAG contrast for MBDown blocks with custom backgrounds.

Managed attachments are mutable files with stable UUID identities, referenced as
`nole://attachment/<uuid>`. The same URI works from Daily cards, notes, and Agent
messages, so moving a note or updating an attachment does not break references.
Open the **Attachment** workspace view, or run `Attachments: Browse`, to inspect
type, size, and reference count; `Enter` opens the managed file for editing and
`d` moves an unreferenced attachment to trash after confirmation. Referenced
attachments cannot be deleted.

Rendered Markdown links and `[link=...]...[/link]` labels are clickable. HTTP(S),
`mailto:`, and other URI links open with the system default application; local
file links and `![[file]]` embeds resolve relative to the containing Markdown
file. In Agent messages, which have no containing file, they resolve relative to
the Nole root. Clicking `[[wikilink]]` searches `daily/`, `data/`, and
`archives/` by filename or filename stem. Multiple MD/MB matches open a chooser
showing source and format metadata; a missing note is created as a new `.md`
file under `data/`. The document index also records every wiki-link target in
the managed notes, so opening a note shows a `Backlinks` section under the body
listing every managed note that links to it — the reverse direction of the link
graph.

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
opens that file in the configured editor (then `$EDITOR`, `$VISUAL`, and `vi`).
Search and message
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
| `Ctrl+U` | undo the last Compose append and restore it to the buffer |
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
| `m` / `n` | move / new note |
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
| `e` | open the selected file in the configured editor |
| `/` | filter directly inside Files |
| `r` / `d` | rename inline / delete with confirmation |
| `Esc`, `q` | return to Center |

During a message move, Files becomes the target picker. During new-file and
rename operations, the input appears at the top of the same Files surface.
Errors leave the active input/context in place so they can be corrected.

### ToDo, documents, Search, and edit

- **ToDo:** scans task-list items from every file in `daily/`. Typing filters
  tasks, arrows select, and `Enter` toggles the checkbox in its source daily
  file. `Esc` returns to Daily.
- **Document:** arrows or `j`/`k` scroll; `PageUp`/`PageDown` move by pages;
  `i` or Enter focuses Compose; `Esc`/`q` closes. Sending from Compose appends
  to the current article, keeps it open, and scrolls directly to the new content.
  `Ctrl+Enter` instead sends the buffer directly to Agent and includes the path
  of the note currently being viewed as context.
  On a file, `e` invokes the configured editor; on a message, `e` opens the in-app message
  editor. `/` opens the same search surface as workspace search, scoped to the
  current article; Enter jumps to the selected source line and Esc returns to
  the article.
- **Search:** type to filter; arrows select; `Enter` or click opens a result;
  `Esc` returns to Daily. Closing a search result first returns to Search.
- **Attachment:** arrows select; `Enter` opens a workspace copy with the system
  application; `d` confirms deletion of an unreferenced attachment. The first
  row remains separated from the panel header by a blank selection row.

Message card edits suspend the TUI and open a temporary `.md` file in the
configured editor. When the editor exits successfully, Nole
writes the content back to the original daily date and removes the temporary
file. Editing from a message preview keeps that preview open and refreshes it.

Mouse activation uses only the left button. The wheel scrolls the pane under the
pointer, and confirmations/Help block all interaction with the workspace below.
`Tab` globally cycles `APPROVE`, `AUTO`, and `YOLO` permission modes without
changing keyboard focus.

## Storage

Data lives under `${NOLE_DIR}` when that environment variable is set, otherwise
under `~/.nole`:

```text
config/         # private application configuration
  ai.toml       # LLM provider and optional Tavily configuration
  settings.toml # theme, default export directory, editor, and terminal shell
  agent-session.json # current Agent conversation; absent when empty
  AGENTS.md      # user-authored Agent instructions
themes/         # Agent-editable application and MBDown themes
  default.toml   # generated current default colors
  <name>.toml    # additional custom themes
MEMORY.md       # Agent-maintained persistent memory
template.mb     # initial content for "Note: New from template"
daily/         # chat cards; absent dates have no file
  YYYY-MM-DD.md
archives/      # flat storage for archived articles
  <name>.md
  <name>.mb
data/          # flat note storage
  <name>.md
  <name>.mb
attachments/   # mutable UUID-addressed managed attachments and trash
cache/         # disposable validated index snapshots; safe to delete
workspace/
  main/         # Agent-owned scratch files; preserved across restarts
```

The Agent may create, edit, move, and delete files freely inside
`workspace/main/`. Clearing the Agent session clears and recreates this
directory. Moving a source from elsewhere remains approval-gated because it
removes user-owned data. The `http_request` tool's `save_to` option streams any
http(s) URL into a new file under `workspace/main/` (see below). Generic file
tools cannot access
attachment internals; the Agent uses dedicated tools to import, read, list, check
out, update, and delete attachments. `checkout_attachment` creates an editable
workspace copy and returns a content token; after editing, `update_attachment`
uses that token to atomically update the same UUID after approval, refusing stale
content. Importing the edited copy instead creates a new attachment.

`.md` and `.mb` extensions are recognized case-insensitively. NoleBase shows
direct, regular files from both `data/` and `archives/` as separate Notes and
Archives groups; symlinks and nested paths are rejected. Startup creates
`daily/` and `archives/`, but a daily file is created only when content is first
sent for that date. Later sends append with a blank line separator. Archiving an
article moves it from `data/` to `archives/`; restoring it moves it back without
overwriting an existing file.

### Theme

On first start Nole creates `themes/default.toml` with its current colors and a
documented `config/settings.toml`. The `editor` and `shell` settings are optional
and commented out by default. Set either one to override its documented
fallback. `export_directory` sets the default directory for `File: Export…`
destinations and defaults to `~`, the user's home directory; it accepts the
same `~`, absolute, and relative forms as the destination prompt itself:

```toml
theme = "default"

# Default directory for File: Export… destinations. "~" is the user's home
# directory; absolute paths and paths relative to the parent of the Nole root
# are also accepted.
export_directory = "~"

# Command used to edit notes. Defaults to $EDITOR, then $VISUAL, then vi.
# editor = "code -w"

# Executable used by the floating terminal. Defaults to the system login shell.
# shell = "fish"
```

Each direct
`themes/<name>.toml` file contains semantic `#RRGGBB` tokens grouped under
`[surface]`, `[selection]`, `[text]`, `[ui]`, `[diff]`, `[markdown]`, and
`[animation]`.
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
any custom theme. The selection is saved to `config/settings.toml` without
overwriting the editor or terminal settings. Changes to
that file or to a direct TOML file under `themes/` are loaded automatically.
Because `themes/` is outside `config/`, the Agent can create and edit custom
themes without gaining access to private configuration.

### AI agent

On first start Nole creates `config/ai.toml` with private file permissions:

```toml
api_format = "messages"
api_key = ""
tavily_api_key = ""
model = "claude-sonnet-4-5"
base_url = "https://api.anthropic.com"
max_tokens = 8192
context_window_tokens = 200000
max_rounds = 25
max_concurrent_local_reads = 8
max_concurrent_network_tools = 8
max_concurrent_subagents = 4
```

Set `api_format` to `messages` for the Anthropic Messages protocol, or to
`completions` for the OpenAI-compatible Chat Completions protocol. Set the
provider credential in `api_key`; `completions` also permits an empty key for
local endpoints. `base_url` must not include `/v1`; Nole appends
`/v1/messages`, `/v1/messages/count_tokens`, or `/v1/chat/completions` itself.

The card's `AI` button runs the configured provider in the background. It first opens a prompt dialog; an
entered prompt is sent with the daily file path, while an empty prompt asks the
Agent to improve that file's Markdown formatting in place without changing its
meaning or adding facts. The lower two-thirds of the right
sidebar shows a chronological Agent timeline: user prompts, tool activity,
intermediate text, and final responses; Todo uses the upper third.
`max_rounds` is the request-round budget for each user prompt and, independently,
for each `explore` and `review` invocation; it is not a tool-call limit, and one
response may call several tools. A user follow-up received while the Agent is
running starts a fresh budget after the in-flight tool finishes. At the main
Agent's limit, Nole rings the terminal bell and asks whether to continue or
stop. On an `explore` or `review` subagent's last allowed request, Nole appends
its finalization prompt and removes all tools. Subagent provider responses stream
internally so long generations stay active, but their text and thinking deltas
remain isolated; only the completed report is returned to the parent. If a
subagent reaches `max_tokens` before that final request, its partial response is
kept in the isolated history and the next request asks it to complete the report.
Reaching `max_tokens` again on the final request fails the invocation rather than
returning a truncated report. Stopping the main Agent keeps its completed
conversation and tool history, so a later prompt can continue it.
`context_window_tokens` is the model's total context size. Nole reserves
`max_tokens` for the next response and, before the remaining input budget is
exhausted, uses provider token counting when available and replaces a safe prefix
of older completed turns with a dense summary. The system prompt, current turn,
recent history, and complete `tool_use`/`tool_result` pairs are retained.
The three `max_concurrent_*` settings bound local read/search calls, web
search/fetch calls, and isolated subagents across the complete Agent tree.
Compatible endpoints without token counting fall back to a conservative local
estimate.
Provider HTTP calls retry connection failures and HTTP 408, 425, 429, and 5xx
responses up to three attempts with exponential backoff or a bounded
`Retry-After`. If a successful HTTP response later fails while decoding its body
or event stream, the complete provider request is replayed up to three bounded
request attempts. The same policy covers the main Agent and isolated subagents.
Partial streamed text and thinking are discarded before replay, while confirmed
provider usage remains counted. The Agent header's `↑` and `↓` values are
session-cumulative input and output across the complete Agent tree. `Cache R`
is provider-confirmed cache-read input; its percentage is the share of all input
served from cache (`read / total input`). `W` appears only when the provider
reports nonzero cache-write tokens—OpenAI-compatible usage normally reports
cached reads but not writes. `t/s` covers only timed main-Agent streamed output,
excluding retry waits and untimed subagent or non-streaming responses.
`R`/`Retry` reports attempts that required a retry.
To diagnose connection, DNS, TLS, timeout, or compatible-endpoint failures,
start Nole with debug logging enabled and redirect standard error to a file:

```sh
NOLE_DEBUG=1 nole 2>nole-debug.log
```

The UI continues to show its concise error. The debug log includes the complete
Agent error chain but does not include the API key, request headers, or request
body.
If a compatible endpoint stops at `max_tokens`, Nole preserves any partial text
as intermediate output and automatically requests continuation. A response
with no text is retried a limited number of times; persistent failures report
the stop reason and returned content-block types.
Agent output enters a daily card only when the Agent explicitly calls
`add_daily_entry`; omitting its date records the content on the current local
date.

Set `tavily_api_key` to enable the Agent's Tavily `search_web` tool. When the
key is empty or absent, Nole omits the tool and its instructions entirely, so
the Agent does not know that web search is available.

While the Agent is running, its panel border carries a moving color gradient
and the current tool uses the same animated full-text color gradient. Messages
API text is streamed into the current Agent entry and rendered as MBDown. The
panel header shows request rounds, session-cumulative input/output, timed
main-stream throughput, and provider-confirmed cache reads as a share of total
input; cache writes appear when reported. Only final replies count as Agent
turns. Token and throughput statistics reset when the Agent session is cleared;
the retry count covers the current application run. Multiple tool calls returned
in one model response still count as one round.
Consecutive read-only calls in one response execute as a concurrent wave. This
includes local reads and searches, web search/fetch, and multiple `explore` or
`review` subagents. Mutation, approval, and TUI interaction tools are exclusive
barriers: Nole finishes the preceding wave before running one, then starts a new
wave.
Filesystem-backed Agent reads use Tokio's asynchronous filesystem APIs rather
than blocking the Agent runtime thread.
By default, concurrency is bounded across the complete Agent tree to eight
local reads, eight network tools, and four subagents; the `max_concurrent_*`
settings above adjust these limits. Tool results are returned to the model in
its original call order even when completion order differs.
Press `Ctrl+P` to open the fuzzy command palette. Commands run through one
application command pipeline; the initial commands interrupt the active Agent
task, clear its saved session, create or manage notes, or open `template.mb`,
`ai.toml`, `AGENTS.md`, and `MEMORY.md` with the configured editor.
`File: Export…` is available when Center is previewing a file, Skill, or Daily
document. The UI retains its Original/HTML picker; the Agent's `export_file`
tool exposes HTML rendering only because exact-byte external copies use `copy`.
Press `Ctrl+\`` or run `Terminal: Open` from the command palette to open a
PTY-backed floating terminal. Its shell starts in the active Nole directory
(`~/.nole` by default, or `NOLE_DIR` when configured). Hiding the terminal
retains its single shell session; exiting the shell closes and discards it.
Set `shell` in `config/settings.toml` to an executable name or path; when it is
absent or blank, Nole uses the system login shell.
The existing `c` and `C` Agent-panel shortcuts invoke those same commands.
Agent conversations and their visible panel history persist in the single
`config/agent-session.json` file across prompts and application restarts. Each
completed conversation update atomically replaces that file; Nole does not
maintain multiple sessions. Continue in the compose box with `Ctrl+Enter`; the
Agent receives the completed conversation history.
One Agent worker lives for the lifetime of the application. It reuses its HTTP
connection pool, tool instances, precomputed tool definitions, and unchanged
file-read snapshots across prompts. Before each task it checks the actual
contents of `ai.toml`, `AGENTS.md`, and `MEMORY.md`, rebuilding the Agent only
when one changed. Versioned file-read snapshots remain valid across prompts;
successful edits immediately record the new tag while retaining recent versions
for conservative line-level recovery when unrelated content drifts. Workspace
file events mark only affected paths as dirty; the next edit compares the actual
identity, preserving delayed events from the Agent's own writes while rejecting
real external changes. Clearing the Agent session clears snapshots and registers.
Tool definitions are emitted in fixed registration order instead of hash-map
iteration order. Messages requests use four explicit prompt-cache breakpoints:
the final tool, the stable base system block, the skill catalog after project
instructions, and the final message content block. The moving message
breakpoint makes the unchanged conversation prefix reusable while edits to
MEMORY.md, skills, or project instructions can still fall back to an earlier
stable prefix. The current timestamp remains in the newest user message.
The Agent can inspect the same shared tag index with `list_tags` and
`search_tag`. Its `rename_tag` tool follows the active permission policy before
changing exact Hashtag source spans across a multi-file diff.
Broad exploration, discovery, comparison, and research run through the
`explore` tool. It starts an isolated read-only agent with file, note, tag, and
web lookup tools, but no mutation, interaction, or recursive-agent tools. Its
search calls, source excerpts, and intermediate reasoning stay in a private
conversation; the main Agent session stores only the `explore` call and its
concise evidence-based report. Independent critical evaluation runs through the
`review` tool. It starts the same kind of isolated read-only agent with the same
capabilities, while the main Agent's task supplies the artifact, intended goals
and constraints, and the standards or concerns to evaluate. The reviewer does
not impose a preset domain checklist: it follows that task, grounds findings in
available evidence, prioritizes them by impact, and returns one self-contained
review without modifying anything. Targeted reads and lookups can
still use the corresponding tools directly. Independent `explore` and `review`
calls run concurrently, as do independent read, search, and fetch calls within
each subagent, subject to the shared limits above. Isolated agents share a
reusable task-scoped runtime; each profile supplies its own instructions,
completion contract, and registered tool capabilities. `explore` and `review`
are the built-in read-only profiles rather than special-purpose agent loops,
and unregistered mutation, interaction, or recursive-agent capabilities are
unavailable to them.
You can also press `Ctrl+Enter` while the Agent is running. Nole combines all
such prompts in one buffer and delivers them before the next pending tool call.
An in-flight concurrent wave is allowed to finish, while later unstarted calls
from the old plan are deferred so the Agent can reconsider them with the new
input and a fresh `max_rounds` budget. A follow-up appears at the end of the
timeline in muted text while queued, then
uses normal MBDown colors once the Agent consumes it. Final responses and later
prompts append to the same virtual-scrolling timeline. Only clearing the Agent
session removes panel history.
Focus the Agent panel and press `c` to cancel the current task, or `C` to clear
the conversation and start a new session. Cancellation stops in-flight provider
and tool HTTP requests and interactive waits promptly. A bounded local file
operation that has already started is allowed to finish before the worker exits.

All scrollable TUI surfaces use virtual row windows. Daily cards, note previews,
Agent output, approval diffs, help, searches, file/Todo lists, and multiline
inputs submit only their currently visible rows to Ratatui; off-screen rows are
retained as scroll state rather than rendered.

The Agent reads through a single unified `read` tool that dispatches on its
target. A UTF-8 file returns hashline text: a `[path#TAG]` header, absolute
one-based `N:text` rows, and a pagination footer. File, extracted document, URL,
and attachment line windows use the independent inclusive `range` argument, for
example `{"path":"data/note.md","range":"50-200"}`. Office documents,
OpenDocument, RTF, EPUB, CSV, and PDF
are converted to Markdown in-process; extracted Markdown is cached by document
identity so continuation reads do not repeat conversion. A directory returns a
typed JSON listing, while an http(s) URL returns structured fetched content with
HTML converted to Markdown. The reader is a parser registry, so additional
formats can be added without changing dispatch logic.
Failed fetches identify the request or processing phase; HTTP status failures
also include the final URL, selected diagnostic headers, and a bounded,
control-character-sanitized response preview.
When configured, `search_web` queries Tavily with optional topic, depth, time range,
answer, result-count, and included/excluded domain controls, then returns compact
ranked results. Every user prompt sent to the Agent includes the current local
date and time. File reads default to 200 lines and accept at most 2,000 lines per
call. Paths under the Nole root are returned in root-relative form so they can be
passed directly to other file tools.
While `read` inspects content (HTML is converted to Markdown, and results are
capped at 1 MB), the `http_request` tool preserves a remote response's exact
bytes as a new file under `workspace/main/` when given `save_to`. It accepts a
required http(s) `url`, an optional `method` (GET, POST, PUT, PATCH, DELETE, or
HEAD), optional `headers`, an optional string `body`, and an optional `range`
object (`offset` + `limit`) for byte-range requests. Without `save_to`, the
unprocessed response is returned inline — status, final URL, response headers,
and body (UTF-8 text or base64), capped at 1 MB with `truncated` and
`content_length` reported when larger so the Agent can page with `range` or fall
back to `save_to`. With `save_to`, the body is streamed into a hidden staging
file while its SHA-256 is computed incrementally and published only when the
transfer is complete — returning the saved root-relative `path`, exact `bytes`,
optional `media_type`, the final `url` after redirects, and a `sha256:<hex>`
content token. Saved files never overwrite existing files, never follow
symlinks, and enforce the 64 MiB per-file and 512 MiB workspace total quotas
against both the declared `Content-Length` and the actual streamed bytes; a
failed or cancelled transfer leaves no partial file. The saved file can be used
immediately by `read`, `import_attachment`, or any generic workspace tool.

Paginated reads and local list/search tools use an inclusive one-based `range`
such as `1-50`, not offset/limit. Structured `read` pages return `range`,
`returned`, `total`, `has_more`, and `items`; callers choose any later range
explicitly. Other list and search tools additionally return an optional `next`.
Tool inputs are validated against their advertised JSON Schema before execution,
including required fields, types, bounds, enums, and unknown-property rejection.

`read` on a directory lists any directory by absolute path or a path relative to
the Nole root. `depth=1` returns direct children and values up to 16 include
nested descendants without following symlinks. Each item includes its type,
depth, extension, byte size, streaming line count, and creation and modification
timestamps. Results support metadata sorting and range pagination.

`list_notes` returns active `data/` notes with their line count, creation and
modification timestamps, and byte size. Results can be sorted ascending or
descending by name or any of those metadata fields and paginated with `range`.

`grep` performs ripgrep-style regular-expression search in any local file or
directory, with optional case-insensitive or fixed-string matching and include
globs. It reads files of any size line by line, respects standard ignore files,
does not follow discovered symlinks, and returns one-based line and byte-column
positions with range pagination. `search_files` remains the case-insensitive
fuzzy filename search used by the Files sidebar.

`write` creates a complete new file and always refuses an existing path. Ordinary
files have no 1 MiB tool limit; workspace files remain subject to the 64 MiB
per-file and 512 MiB total workspace quotas, while Skills retain their separate
size limit because they enter Agent context. The full candidate content passes
MBDown or Skill validation before publication. `edit` accepts a hashline patch:
each section starts with the latest `[path#TAG]` returned by `read`, followed by
operations against the original one-based line numbers. `PUT N.=M:` replaces an
inclusive range with `+TEXT` body rows; `PUT <N:`, `PUT >N:`, and `PUT >$:`
insert before, after, or at end-of-file. `PUT N*:` and `PUT >N*:` resolve
Markdown, brace-delimited, and indentation-delimited blocks. `CUT` captures and
deletes a range or block, and a later `PUT ... @name` pastes a named register;
`REM` and `MV DEST` perform section-level delete and move operations. Multiple
sections are preflighted together before publication.

Unchanged content streams through bounded temporary candidates, so files larger
than 1 MiB remain editable after paged reads; hashline planning is capped at
8 MiB. Changed/deleted ranges must have been covered by `read`, and insertions
require adjacent anchor lines. On an old retained tag, edits rebase only when
every touched line and insertion anchor is unchanged; conflicting drift is
rejected. Each successful edit returns the new tag, line count, and renumbered
changed windows, so another edit needs no intervening read. Existing
`daily/YYYY-MM-DD.md` files use the same `read`, `edit`, and `delete` operations
as other Markdown files.
`add_daily_entry` remains the high-level create-or-append operation and does not
require approval. Its optional `date` uses `YYYY-MM-DD`; omitting it records
content on the current local date.
The Agent can list `daily/` to discover available dates before reading the
relevant files.

Generic file mutation tools accept paths relative to the Nole directory or
absolute paths elsewhere on the filesystem. They preserve the existing safety
contracts: regular-file checks, no symlink traversal, create-new destinations,
read-before-`edit` snapshots, collision preflight, and rollback for a failed
`move_many`. Relative paths still resolve under Nole. Generic tools cannot mutate
`config/`, attachment internals, or files directly inside `daily/`; recursive
directory removal inside Nole remains limited to `workspace/main/`.

`export_file` renders a UTF-8 `.md` or `.mb` source as standalone HTML at a
new external destination. Preparation validates the source and destination
before the permission gate; preparation and publication run off the Agent
async runtime. Publication revalidates source content and destination afterward,
never overwrites, and returns the resolved destination, exact output byte count,
and renderer-warning summary. Exact-byte external copies use the generic `copy`
tool instead. `config/` and attachment object internals are never export sources.

The `notify` tool lets the Agent display a short notification card in the TUI's
top-right corner and emits the terminal bell. Notifications are non-blocking
and expire automatically.
The `open` tool switches the TUI to an existing managed `.md` or `.mb` note in
`daily/`, `data/`, or `archives/`, so the Agent can present relevant material to
the user directly.
The `ask` tool pauses the Agent and opens a TUI dialog for clarification.
The Agent may provide up to ten choices; use Up/Down and Enter to select one,
or type a different free-text response. Esc cancels the question. Questions
are interactive requests rather than permission checks, so permission mode does
not skip them.

The `shell` tool runs a non-interactive command through Brush with the user's
login profile, interactive rc files, aliases, and shell functions loaded. Its
stdin is closed, and `PAGER=cat`, `GIT_PAGER=cat`, `SYSTEMD_PAGER=cat`,
`TERM=dumb`, `GIT_TERMINAL_PROMPT=0`, `CI=true`, and `NO_COLOR=1` are injected
to prevent routine commands from waiting for interaction. Shell commands have
full host access and are not sandboxed.

For genuinely interactive work, `terminal_open`, `terminal_input`,
`terminal_read`, and `terminal_close` manage one persistent Agent PTY. A
five-row monitor remains at the top of the center panel while the session
exists; the underlying virtual terminal remains 24 rows. In `APPROVE` and
`AUTO`, every shell command and every Agent PTY input opens an approval dialog
showing the Agent's purpose and the complete command or input. `YOLO` skips
these approvals. Text sent through `terminal_input` presses Enter by default;
`submit=false` types the text without submitting it, and named keys include
`ctrl-a` through `ctrl-z`. An exited PTY keeps its
final screen until `terminal_close` removes it. Denying an approval blocks that
action but preserves an existing PTY; cancelling the Agent or clearing the Agent
session terminates and removes a live PTY
immediately.

The system prompt requires the Agent to use `ask` when it needs an answer before
it can complete the current task. Later `Ctrl+Enter` prompts remain part
of the same conversation until `C` is pressed or `Agent: Clear session` is run.

Nole creates an empty `template.mb`. `Note: New from template` starts the new
note with its exact contents; regular note creation still generates the note title.
Nole also creates empty `config/AGENTS.md` and `MEMORY.md` files. Their complete
contents are appended to the system prompt in that order for every Agent task.
`config/AGENTS.md` is user-owned: Agent file tools cannot mutate anything in
`config/`. The Agent may read and update root-level `MEMORY.md`; localized
updates use the normal read-before-`edit` approval flow.

In `APPROVE` mode, filesystem mutations and other sensitive updates always pause
for a diff or confirmation. `AUTO` approves mutations under `${NOLE_DIR}`
automatically but requires explicit approval for paths elsewhere. `YOLO` skips
all permission dialogs. Diff approvals emphasize the exact changed graphemes
inside paired replacement lines in both unified and side-by-side views. Every
mode retains path, symlink, identity,
read-before-update, and non-overwrite validation.
Adding a new card never requires approval. Note listings return at
most 2,000 entries per call; file and web responses are capped at 1 MB.
Filesystem mutation tools reject symlink targets. The API configuration itself
is not exposed to tools.

## Libraries

Every Markdown and MBDown card in NólëBase is parsed and rendered by two
standalone crates that are also available to any Rust TUI project:

- [`mbdown`](https://crates.io/crates/mbdown) — parser and canonical formatter
  for the MBDown markup language. It produces a backend-neutral syntax tree
  covering CommonMark plus hashtags, wikilinks, file embeds, restricted inline
  BBCode, inline and display math, and structural containers such as boxes and
  responsive columns. YAML/TOML front matter stays available as source text.
- [`mbtui`](https://crates.io/crates/mbtui) — native Ratatui renderer for
  `mbdown` documents. It handles Unicode-aware layout for tables,
  syntax-highlighted code blocks, boxes, and columns; applies semantic styles;
  and reports hashtag, wikilink, and embed regions with terminal-cell
  coordinates so applications can add interaction without reparsing. Images
  come back as cell rectangles with reserved blank rows for any image backend.

Render MBDown directly in your own application:

```rust
use mbtui::Renderer;

let rendered = Renderer::new(80).render(&mbdown::parse("# Hello")?);
for line in &rendered.text {
    // native Ratatui lines, ready for a Paragraph widget
}
```

## Build and check

MBDown and its terminal widgets (`mbdown`, `mbtui`) are published as
standalone crates on crates.io and pulled in as ordinary dependencies, so no
separate workspace checkout is needed.

```bash
cargo run -q
cargo build --release
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```
