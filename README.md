# remark

Terminal-first code review notes for Git repos.

`remark` renders diffs for your working tree and lets you attach **line comments** that are saved as **git notes**. It also generates a collated “review prompt” you can hand to an LLM agent (or a human) to implement the requested changes.

## Why

- Keep review feedback **in the repo**, versioned, branch-aware, and shareable via `refs/notes/*`.
- Review **staged**, **unstaged**, or **base-branch** diffs without shelling out to `git`.
- Stay in the terminal, with a UI inspired by GitHub/Forgejo review flows.

## Features

- **Two-pane TUI**: file list on the left, diff on the right.
- **Views**
  - `all`: HEAD → worktree (shows combined staged + unstaged result)
  - `staged`: HEAD → index
  - `unstaged`: index → worktree
  - `base`: merge-base(base, HEAD) → HEAD
- **Diff modes** (toggle with `i`)
  - Unified (“stacked”)
  - Side-by-side (only for modified files; added/deleted fall back to unified)
  - Mode persists in `.git/config` as `remark.diffView`
- **Per-file syntax highlighting** on diff code lines using `syntastica` + `hyperpolyglot` language detection.
- **Comment markers**: unresolved comments show `💬` and resolved comments show `✓`.
- **Resolve comments**: resolve/unresolve individual comments so your prompt only contains actionable items.
- **Git notes storage**: comments are stored under a notes ref (default: `refs/notes/remark`).
- **Reviewed files**
  - Toggle reviewed state per file; reviewed files are dimmed with a checkmark.
  - Jump between unreviewed files with `Ctrl+N` / `Ctrl+P` in the diff pane.
- **Prompt rendering**
  - In TUI: open a prompt editor, edit it, and copy with `Shift+Enter`.
  - Headless: `remark prompt …` prints (or copies) the prompt without launching the UI.
- **Clipboard support**
  - Desktop clipboard via `copypasta` (Wayland supported)
  - OSC52 fallback for remote terminals

## Install / Build

This repo is a Rust binary crate.

```bash
cargo build --release
```

The resulting binary is:

```bash
target/release/remark
```

## Release (maintainers)

Releases are cut automatically after CI passes on pushes to `main` by the
`Cut Release` workflow. It:

- Updates `CHANGELOG.md` via `git-cliff`
- Runs `cargo release` to bump the patch version and tag
- Pushes the release commit and tag

Artifacts are built and published by the `Release` workflow, which is triggered
by the tag push.

Important: the tag push must be made with a token that can trigger workflows.
Set a repo secret named `RELEASE_TOKEN` (a PAT with contents write + workflow
permissions). Without it, the tag push will not trigger `Release`, and no
GitHub Release artifacts will be produced.

## Usage

Run inside a git repository:

```bash
remark
```

Optional flags:

```bash
remark --ref refs/notes/remark --base refs/heads/main
remark --view staged
remark --file src/lib.rs --line 42 --side new
```

- `--ref`: which notes ref to store reviews under (default: `refs/notes/remark`)
- `--base`: base ref used by the “base” view (default: `@{upstream}` then `main`/`master` heuristics)
- `--view`: start in view `all`, `unstaged`, `staged`, or `base`
- `--file`: preselect a file when launching the UI
- `--line`: preselect a 1-based line in the selected file (requires `--file`)
- `--side`: which side for line selection (`old` or `new`, default: `new`)

### Headless prompt output

Render the collated prompt from the stored per-file notes, without starting the TUI:

```bash
remark prompt
```

Options:

```bash
remark prompt --filter all
remark prompt --filter staged
remark prompt --filter unstaged
remark prompt --filter base --base refs/heads/main
remark prompt --ref refs/notes/remark
remark prompt --copy
```

### Resolve a comment without the UI

```bash
# Resolve a file-level comment
remark resolve --file src/lib.rs --file-comment

# Resolve a line comment (default side is "new")
remark resolve --file src/lib.rs --line 42

# Resolve an "old" (deleted) line comment
remark resolve --file src/lib.rs --line 10 --side old

# Mark a comment as unresolved again
remark resolve --file src/lib.rs --line 42 --unresolve
```

## Helix integration (LSP workflow)

Two configuration files give you a seamless flow: keybindings to open the draft,
and language-server wiring so remark shows code actions and syncs on save.

### 1) Keybindings (open the draft)

In `~/.config/helix/config.toml`:

```toml
[keys.normal]
A-h = ':hsplit .git/remark/draft.md'
A-v = ':vsplit .git/remark/draft.md'
```

These bindings open the repo’s draft file in a split. For best results, start
Helix from the repo root so `.git/remark/draft.md` resolves correctly.

### 2) Language server configuration

In `~/.config/helix/languages.toml`:

```toml
[language-server.remark]
command = "remark"
args = ["lsp"]
required-root-patterns = [".git"]

# TypeScript / TSX
[[language]]
name = "typescript"
language-servers = ["remark", "typescript-language-server"]

[[language]]
name = "tsx"
language-servers = ["remark", "typescript-language-server"]

# Go
[[language]]
name = "go"
language-servers = ["remark", "gopls"]

# Python
[[language]]
name = "python"
language-servers = ["remark", "pyright"]

# Shell (pick what you use)
[[language]]
name = "bash"
language-servers = ["remark", "bash-language-server"]

[[language]]
name = "toml"
language-servers = [ "crates-lsp", "taplo" ]
formatter = { command = "taplo", args = ["fmt", "-"] }

[[language]]
name = "rust"
roots = ["Cargo.toml", "Cargo.lock"]
language-servers = [
  "remark",
  "rust-analyzer"
]
```

Workflow:
1) Use the remark code action (space+a) on a line or file header to seed a comment.
2) Press `Alt-h` / `Alt-v` to open the draft.
3) Edit and save; the LSP syncs draft changes back into notes.

Viewing comments:

To view a single comment in the editor you can move to the start of the line and the inlay
hint will be shown. You can also use the hover action (space + k) to show the message diagnostics.

To view all the comments for a file you can just view the diagnostics (space + d). That should list all the
LSP diagnostics and so you can review comments that way too.

## Zed integration

Zed only starts custom language servers when they are registered by an extension.

### Install the extension (users)

Install `remark-lsp` from Zed's extensions UI.

### Install the dev extension (maintainers)

Use the dev extension while iterating locally:

1) In Zed, open the command palette and choose **Extensions: Install Dev Extension**.
2) Select the folder `zed-extension/remark-lsp` in this repo.

### Configure languages

Add `remark-lsp` to the languages you want, and keep defaults with `...`:

```json
{
  "languages": {
    "Rust": { "language_servers": ["remark-lsp", "..."] },
    "Go": { "language_servers": ["remark-lsp", "..."] },
    "TypeScript": { "language_servers": ["remark-lsp", "..."] },
    "TSX": { "language_servers": ["remark-lsp", "..."] }
  }
}
```

The extension resolves the `remark` binary from your PATH, then runs `remark lsp`.
If Zed can't find it, make sure `$HOME/.cargo/bin` is in the PATH Zed sees.
You can pass extra LSP flags via `REMARK_LSP_ARGS`, for example:

```bash
REMARK_LSP_ARGS="--no-inlay-hints" zed
```

### Draft workflow (tasks)

Zed doesn't yet support the LSP `showDocument` flow that remark uses to open the draft,
so use a task to open `.git/remark/draft.md`. First, use the code action to mark a line
or file for comment, then open the draft file via a task.

Create a project-local tasks file at `.zed/tasks.json` (in the repo root):

```json
[
  {
    "label": "remark: open draft",
    "command": "gitdir=$(git rev-parse --git-dir) && zed \"$gitdir/remark/draft.md\"",
    "cwd": "$ZED_WORKTREE_ROOT"
  }
]
```

Notes:
- Requires the `zed` CLI to be on your PATH. (you may need to symlink: `sudo ln -sf /usr/bin/zeditor /usr/bin/zed`)
- The draft file is inside `.git`, so it won't show up in git status.

To run the task, open the command palette and use **task: spawn** (Alt-Shift-T), then pick the
`remark: open draft ...` entry.

:notice: If the draft buffer has already been openened before, it won't pick up changes to the draft file. Use the command palette to reload the file: `editor: reload file`. 

## VS Code integration

VS Code only starts language servers via extensions. This repo includes a VS
Code extension at `vscode-extension/remark-lsp`.

### Install the dev extension (maintainers)

1) Open `vscode-extension/remark-lsp` in VS Code.
2) Run `npm install` and `npm run compile` (or `npm run watch`).
3) Press `F5` to launch the Extension Development Host.

### Configure languages

The extension starts `remark lsp` from your PATH by default. You can customize
it via settings:

- `remark.path`: explicit path to the `remark` binary.
- `remark.lspArgs`: extra args passed to `remark lsp` (string or string array).
- `remark.languages`: language ids to attach to (default: `["*"]` for all).

You can also pass extra LSP flags via `REMARK_LSP_ARGS`, for example:

```bash
REMARK_LSP_ARGS="--no-inlay-hints" code
```

### Add comments from VS Code

Use the light bulb (quick fixes) to add comments:

- **Remark: Add line comment**
- **Remark: Add file comment**

This opens a multi-line editor panel (no escaping needed). Line comments always
target the new side. The extension uses `remark add` so the CLI handles syncing.

### Draft workflow (tasks)

Use the command palette for either:

- **Remark: Open Draft** (command), or
- **Tasks: Run Task** → `remark: open draft` (task)

If you are not using the extension, create a project-local tasks file at
`.vscode/tasks.json`:

```json
{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "remark: open draft",
      "type": "shell",
      "command": "bash",
      "args": ["-lc", "gitdir=$(git rev-parse --git-dir) && code \"$gitdir/remark/draft.md\""]
    }
  ]
}
```

Notes:
- Requires the `code` CLI to be on your PATH (extension tasks do not).
- The draft file is inside `.git`, so it won’t show up in git status.

Workflow:
1) Add comments via the quick fix UI or `remark add`.
2) Run **Tasks: Run Task** → `remark: open draft`.
3) Edit and save; `remark` will sync draft changes on the next command or prompt.

## Keybindings

### Global / browse mode

- `h` / `l` or `←` / `→`: switch focus between **Files** and **Diff**
- `1` / `2` / `3` / `4`: switch view **all / unstaged / staged / base**
- `i`: toggle diff mode **unified ↔ side-by-side**
- `R`: reload file list
- `↑` / `↓`, `j` / `k`: move selection (focused pane)
- `PgUp` / `PgDn`, `Ctrl+U` / `Ctrl+D`: page up/down (focused pane)
- `Ctrl+N` / `Ctrl+P`: next/prev unreviewed file (diff pane)
- `v`: toggle reviewed (selected file)
- `c`: add/edit comment (file header or commentable line)
- `d`: delete comment (file header or commentable line)
- `r`: resolve/unresolve comment
- `p`: open prompt editor
- `Esc`: dismiss overlay or quit
- `?`: help (press `?` or `Esc` again to close)

### Comment editor

- `Enter`: newline
- `Shift+Enter`: accept comment and close (`Ctrl+S` fallback)
- `Esc`: cancel editor

### Prompt editor

- `Enter`: newline
- `Shift+Enter`: copy prompt and close (`Ctrl+S` fallback)
- `Esc`: close prompt editor

### Terminal input note

Shift+Enter is only distinguishable if your terminal sends modified Enter. Enable the kitty
keyboard protocol in your terminal emulator (for WezTerm, set `enable_kitty_keyboard = true`)
and avoid overriding Shift+Enter with a raw `SendString` binding. `Ctrl+S` is supported as a
fallback for terminals that don't report modified Enter.

## What counts as “commentable”

`remark` supports:

- **File-level comments**: select the file header row at the top of the diff and press `c`.
- **Line comments**:
  - Added/context lines attach to **new** (right) line numbers.
  - Removed lines attach to **old** (left) line numbers.

## Storage model

### Where the review is stored

Reviews are stored as **git notes** under a configurable ref (default `refs/notes/remark`).

Notes are attached to **synthetic object ids** derived from:

- the current `HEAD` commit id
- the review mode (`all` for worktree reviews, or `base:<ref>` for base comparisons)
- the file path

This means each reviewed file gets its **own note**, and when you create a new commit you naturally get a fresh set of notes for that commit.

### Note contents

Each per-file note is markdown with an embedded JSON block:

- JSON contains:
  - a file-level comment (optional)
  - line comments keyed by `(old|new, 1-based line_number)`

The LLM prompt is generated by collating all per-file notes for the current view.

Per-file notes also store a `reviewed` flag to persist the reviewed state in the file tree.

## Clipboard behavior

When copying a prompt (`Shift+Enter` in the prompt editor, or `remark prompt --copy`):

1. `remark` tries the desktop clipboard via `copypasta` (works on Wayland/X11/macOS/Windows depending on environment).
2. If that fails (common in headless/SSH sessions), it falls back to **OSC52**, which asks your terminal emulator to place the text into your local clipboard.

## Current limitations

- Comments are currently file-level + line-level only; no multi-line / hunk-level comments.
- Comments are anchored to old/new line numbers (they can drift as the diff changes).
- UI styling is intentionally simple (especially around file/hunk headers).

## Implementation notes

Core libraries used:

- `ratatui` + `crossterm` for TUI rendering/input
- `gix` for repository access (status, trees, objects, notes ref updates)
- `gix-diff` for unified diff generation
- `syntastica` + `syntastica-parsers` + `syntastica-themes` for syntax highlighting
- `hyperpolyglot` for language detection
- `copypasta` (desktop clipboard) + OSC52 fallback (remote clipboard-friendly)

## License

MIT (see `LICENSE`).
