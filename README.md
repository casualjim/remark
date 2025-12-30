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
