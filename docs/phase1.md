# Phase 1 (Current Behavior)

## Goals
- Local review workflow for LLM-driven fixes.
- Fast, context-aware prompt generation from stored notes.
- Consistent, context-aware keybindings.

---

## Storage Model

### Notes storage
- Comments and per-file state are stored as git notes under `refs/notes/remark` (configurable).
- Each file gets its own note keyed by `HEAD` + view + file path.

### Reviewed state
- A file can be marked reviewed/unreviewed.
- The reviewed flag is stored in the file note alongside comments.

---

## Diff View Modes
- Toggle with `i` between unified and side-by-side.
- Side-by-side is disabled automatically for added/deleted files.
- The selected mode is persisted in `.git/config` under `remark.diffView`.

---

## Keybindings

### Global
- `Esc`: dismiss topmost overlay; if none open -> quit.
- `?`: toggle help (closes with `Esc`).

### Browse / Navigation
- Focus: `h` / `l` or Left/Right.
- Movement (current pane): arrows, `j` / `k`.
- Page: `PgUp` / `PgDn`, `Ctrl+U` / `Ctrl+D`.
- Actions:
  - `c` = add/edit comment (file/line)
  - `d` = delete comment
  - `r` = resolve/unresolve comment
  - `p` = open prompt editor
  - `v` = toggle reviewed for selected file
- File-to-file navigation (diff pane only):
  - `Ctrl+N` = next unreviewed file with changes
  - `Ctrl+P` = previous unreviewed file with changes
  - Skips reviewed files but does not block direct selection from the file tree

### Comment editor
- `Enter` = newline
- `Shift+Enter` = accept and close (Ctrl+S fallback)
- `Esc` = cancel

### Prompt editor
- `Enter` = newline
- `Shift+Enter` = copy prompt and close (Ctrl+S fallback)
- `Esc` = close

---

## Reviewed Visuals
- Reviewed files are dimmed in the file tree.
- A small checkmark is prefixed to reviewed file names.

---

## Terminal Input Notes
- Shift+Enter requires terminals that send modified Enter; enable kitty keyboard protocol when available.
- Ctrl+S is a fallback for terminals that don't send modified Enter reliably.
- In WezTerm, set `enable_kitty_keyboard = true` and avoid overriding Shift+Enter with `SendString`.
