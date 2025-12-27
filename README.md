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
  - Side-by-side
- **Per-file syntax highlighting** on diff code lines using `syntastica` + `hyperpolyglot` language detection.
- **Comment markers**: lines with comments show `💬` in the gutter.
- **Git notes storage**: comments are stored under a notes ref (default: `refs/notes/remark`).
- **Prompt rendering**
  - In TUI: open prompt preview and copy it to clipboard.
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

## Usage

Run inside a git repository:

```bash
remark
```

Optional flags:

```bash
remark --ref refs/notes/remark --base refs/heads/main
```

- `--ref`: which notes ref to store reviews under (default: `refs/notes/remark`)
- `--base`: base ref used by the “base” view (default: `@{upstream}` then `main`/`master` heuristics)

### Headless prompt output

Render the collated prompt from the stored note, without starting the TUI:

```bash
remark prompt
```

Options:

```bash
remark prompt --view all
remark prompt --view staged
remark prompt --view unstaged
remark prompt --view base --base refs/heads/main
remark prompt --ref refs/notes/remark
remark prompt --copy
```

## Keybindings

### Global / browse mode

- `Tab` / `Shift-Tab`: switch focus between **Files** and **Diff**
- `1` / `2` / `3` / `4`: switch view **all / unstaged / staged / base**
- `i`: toggle diff mode **unified ↔ side-by-side**
- `↑` / `↓`: move selection (focused pane)
- `PgUp` / `PgDn`: scroll (focused pane)
- `c`: add/edit comment on the current line (commentable lines only)
- `d`: delete comment on the current line
- `Ctrl+S`: save review note to the configured notes ref
- `p`: toggle prompt preview panel
- `y`: copy prompt to clipboard (when prompt preview is open)
- `q` / `Q`: quit (unsaved changes are discarded unless you saved with `Ctrl+S`)

### Comment editor

- `Enter`: newline
- `F2` or `Alt+Enter`: accept comment and advance
- `Esc`: cancel editor

Note: many terminals do not reliably distinguish `Shift+Enter` / `Ctrl+Enter`, which is why acceptance uses `F2` / `Alt+Enter`.

## What counts as “commentable”

Line comments are attached to **new-file line numbers** (the “right” side):

- Unified mode: context and added lines have a `new_line` number and can be commented.
- Side-by-side mode: comments attach to the **right** column’s line number.

Removed-only lines don’t have a new-file line number; they are not commentable currently.

## Storage model

### Where the review is stored

Reviews are stored as **git notes** under a configurable ref (default `refs/notes/remark`).

Notes are attached to a **synthetic object id** derived from the view parameters, so you get different notes for:

- `all`
- `staged`
- `unstaged`
- `base:<ref>`

### Note contents

Each note is markdown with an embedded JSON block:

- JSON contains per-file line comments keyed by `(path, 1-based new_line_number)`.
- The markdown also includes a “Review (LLM Prompt)” section which is what `remark prompt` renders.

## Clipboard behavior

When copying a prompt (`y` in prompt preview, or `remark prompt --copy`):

1. `remark` tries the desktop clipboard via `copypasta` (works on Wayland/X11/macOS/Windows depending on environment).
2. If that fails (common in headless/SSH sessions), it falls back to **OSC52**, which asks your terminal emulator to place the text into your local clipboard.

## Current limitations / roadmap

- No fuzzy file picker yet (only file list navigation).
- Comments are currently line-level only; no multi-line / file-level comments.
- Removed-only lines aren’t commentable yet (no stable “new line” anchor).
- UI styling is intentionally simple; more “delta-like” header formatting can be expanded further (file headers, hunk metadata rendering, etc.).

## Implementation notes

Core libraries used:

- `ratatui` + `crossterm` for TUI rendering/input
- `gix` for repository access (status, trees, objects, notes ref updates)
- `gix-diff` for unified diff generation
- `syntastica` + `syntastica-parsers` + `syntastica-themes` for syntax highlighting
- `hyperpolyglot` for language detection
- `copypasta` (desktop clipboard) + OSC52 fallback (remote clipboard-friendly)

