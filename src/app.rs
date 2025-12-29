use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use gix::ObjectId;

use crate::git::ViewKind;
use crate::highlight::Highlighter;
use crate::review::{FileReview, LineSide, Review};
use unicode_width::UnicodeWidthStr;

const DEFAULT_NOTES_REF: &str = "refs/notes/remark";

#[derive(Debug, Clone)]
struct Config {
    notes_ref: String,
    base_ref: Option<String>,
}

impl Config {
    fn from_env(repo: &gix::Repository) -> Self {
        let mut notes_ref = DEFAULT_NOTES_REF.to_string();
        let mut base_ref: Option<String> = crate::git::default_base_ref(repo);

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--ref" => {
                    if let Some(v) = args.next() {
                        notes_ref = v;
                    }
                }
                "--base" => {
                    if let Some(v) = args.next() {
                        base_ref = Some(v);
                    }
                }
                "-h" | "--help" => {
                    print_help_and_exit();
                }
                _ => {}
            }
        }

        Self {
            notes_ref,
            base_ref,
        }
    }
}

fn print_help_and_exit() -> ! {
    eprintln!(
        "remark\n\nUSAGE:\n  remark [--ref <notes-ref>] [--base <ref>]\n\nOPTIONS:\n  --ref <notes-ref>   Notes ref to store reviews (default: {DEFAULT_NOTES_REF})\n  --base <ref>        Base ref for base view (default: @{{upstream}} / main / master)\n\nKEYS (browse):\n  Tab / Shift+Tab     focus files/diff\n  1/2/3/4             all/unstaged/staged/base\n  i                   toggle unified/side-by-side diff\n  R                   reload file list\n  Up/Down             navigate (focused pane)\n  PgUp/PgDn           scroll (focused pane)\n  Enter               focus diff (from files)\n  c                   add/edit comment (file or line)\n  d                   delete comment (file or line)\n  r                   resolve/unresolve comment\n  p                   preview collated prompt\n  y                   copy prompt to clipboard (when open)\n  ?                   help\n  q / Q               quit\n\nKEYS (comment editor):\n  Enter               newline\n  Ctrl+S / F2         accept comment and move on\n  Esc                 cancel\n"
    );
    std::process::exit(2);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Browse,
    EditComment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Files,
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommentLocator {
    File,
    Line { side: LineSide, line: u32 },
}

#[derive(Debug, Clone)]
pub(crate) struct CommentTarget {
    pub(crate) path: String,
    pub(crate) locator: CommentLocator,
}

#[derive(Debug, Clone)]
pub(crate) struct DiffRow {
    pub(crate) kind: crate::diff::Kind,
    pub(crate) old_line: Option<u32>,
    pub(crate) new_line: Option<u32>,
    pub(crate) spans: Vec<ratatui::text::Span<'static>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffViewMode {
    Unified,
    SideBySide,
}

#[derive(Debug, Clone)]
pub(crate) struct SideBySideRow {
    pub(crate) left_kind: Option<crate::diff::Kind>,
    pub(crate) right_kind: Option<crate::diff::Kind>,
    pub(crate) old_line: Option<u32>,
    pub(crate) new_line: Option<u32>,
    pub(crate) left_spans: Vec<ratatui::text::Span<'static>>,
    pub(crate) right_spans: Vec<ratatui::text::Span<'static>>,
}

#[derive(Debug, Clone)]
pub(crate) enum RenderRow {
    FileHeader { path: String },
    Section { text: String },
    Unified(DiffRow),
    SideBySide(SideBySideRow),
}

#[derive(Debug, Clone, Copy)]
enum KeepLine {
    Old(u32),
    New(u32),
}

pub fn run() -> Result<()> {
    let repo = gix::discover(std::env::current_dir().context("get current directory")?)
        .context("discover git repository")?;
    let config = Config::from_env(&repo);

    let mut ui = crate::ui::Ui::new()?;

    let mut app = App::new(repo, config.notes_ref, config.base_ref)?;
    let res = app.run_loop(&mut ui);

    ui.restore().ok();
    res
}

struct App {
    repo: gix::Repository,
    notes_ref: String,
    base_ref: Option<String>,

    view: ViewKind,
    focus: Focus,
    mode: Mode,
    diff_view_mode: DiffViewMode,

    head_commit_oid: Option<ObjectId>,
    review: Review,

    files: Vec<FileEntry>,
    file_selected: usize,
    file_scroll: u16,
    files_viewport_height: u16,

    diff_rows: Vec<RenderRow>,
    diff_cursor: usize,
    diff_scroll: u16,
    diff_viewport_height: u16,
    diff_viewport_width: u16,
    diff_row_offsets: Vec<u32>,
    diff_row_heights: Vec<u16>,
    diff_total_visual_lines: u32,

    editor_target: Option<CommentTarget>,
    editor_buffer: crate::ui::NoteBuffer,

    status: String,
    show_help: bool,
    show_prompt: bool,

    highlighter: Highlighter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileStageKind {
    None,
    Staged,
    Unstaged,
    Partial,
}

#[derive(Debug, Clone)]
pub(crate) struct FileEntry {
    pub(crate) path: String,
    pub(crate) change: FileChangeKind,
    pub(crate) stage: FileStageKind,
}

impl App {
    fn new(repo: gix::Repository, notes_ref: String, base_ref: Option<String>) -> Result<Self> {
        let highlighter = Highlighter::new()?;
        let mut app = Self {
            repo,
            notes_ref,
            base_ref,
            view: ViewKind::All,
            focus: Focus::Files,
            mode: Mode::Browse,
            diff_view_mode: DiffViewMode::Unified,
            head_commit_oid: None,
            review: Review::new(),
            files: Vec::new(),
            file_selected: 0,
            file_scroll: 0,
            files_viewport_height: 1,
            diff_rows: Vec::new(),
            diff_cursor: 0,
            diff_scroll: 0,
            diff_viewport_height: 1,
            diff_viewport_width: 1,
            diff_row_offsets: Vec::new(),
            diff_row_heights: Vec::new(),
            diff_total_visual_lines: 0,
            editor_target: None,
            editor_buffer: crate::ui::NoteBuffer::new(),
            status: String::new(),
            show_help: false,
            show_prompt: false,
            highlighter,
        };
        app.reload_view()?;
        Ok(app)
    }

    fn run_loop(&mut self, ui: &mut crate::ui::Ui) -> Result<()> {
        let tick_rate = Duration::from_millis(33);

        loop {
            let size = ui.terminal.size().context("read terminal size")?;
            let outer = ratatui::layout::Rect::from(size);
            let [main, _footer] = ratatui::layout::Layout::default()
                .direction(ratatui::layout::Direction::Vertical)
                .constraints([
                    ratatui::layout::Constraint::Min(1),
                    ratatui::layout::Constraint::Length(1),
                ])
                .areas(outer);

            let rects = crate::ui::layout(main);
            self.files_viewport_height = rects.files.height.saturating_sub(2).max(1);
            self.diff_viewport_height = rects.diff.height.saturating_sub(2).max(1);
            self.diff_viewport_width = rects.diff.width.saturating_sub(2).max(1);
            self.recompute_diff_metrics(self.diff_viewport_width);
            self.ensure_file_visible(self.files_viewport_height);
            self.ensure_diff_visible(self.diff_viewport_height);
            self.clamp_diff_scroll();

            ui.terminal
                .draw(|f| {
                    crate::ui::draw(
                        f,
                        crate::ui::DrawState {
                            view: self.view,
                            base_ref: self.base_ref.as_deref(),
                            focus: self.focus,
                            mode: self.mode,
                            diff_view_mode: self.diff_view_mode,
                            review: &self.review,
                            files: &self.files,
                            file_selected: self.file_selected,
                            file_scroll: self.file_scroll,
                            diff_rows: &self.diff_rows,
                            diff_cursor: self.diff_cursor,
                            diff_scroll: self.diff_scroll,
                            diff_cursor_visual: self
                                .diff_row_offsets
                                .get(self.diff_cursor)
                                .copied()
                                .unwrap_or(0),
                            editor_target: self.editor_target.as_ref(),
                            editor_buffer: &self.editor_buffer,
                            status: &self.status,
                            show_help: self.show_help,
                            show_prompt: self.show_prompt,
                            prompt_text: &crate::review::render_prompt(&self.review),
                        },
                    )
                })
                .context("draw ui")?;

            if crossterm::event::poll(tick_rate).context("poll events")? {
                match crossterm::event::read().context("read event")? {
                    Event::Key(key) => {
                        if self.handle_key(key)? {
                            break;
                        }
                    }
                    Event::Mouse(m) => {
                        self.handle_mouse(m, rects)?;
                    }
                    Event::Resize(_, _) => {
                        self.status = "Resized".to_string();
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        match self.mode {
            Mode::Browse => self.handle_browse_key(key),
            Mode::EditComment => self.handle_edit_key(key),
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.modifiers.is_empty() {
            match key.code {
                KeyCode::Char('Q') => return Ok(true),
                KeyCode::Char('q') => {
                    return Ok(true);
                }
                _ => {}
            }
        }

        if self.show_help {
            // While help is open, treat most keys as inert.
            let is_plain_qmark = key.code == KeyCode::Char('?')
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT);
            if key.code == KeyCode::Esc || is_plain_qmark {
                self.show_help = false;
            }
            return Ok(false);
        }

        if key.code == KeyCode::Char('?')
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            self.show_help = !self.show_help;
            return Ok(false);
        }

        if key.code == KeyCode::Char('p') && key.modifiers.is_empty() {
            self.show_prompt = !self.show_prompt;
            return Ok(false);
        }

        if self.show_prompt && key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
            let prompt = crate::review::render_prompt(&self.review);
            match crate::clipboard::copy(&prompt) {
                Ok(method) => self.status = format!("Copied prompt to clipboard ({method})"),
                Err(e) => self.status = format!("Clipboard failed: {e}"),
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Tab => self.focus = next_focus(self.focus),
            KeyCode::BackTab => self.focus = prev_focus(self.focus),
            KeyCode::Char('1') => self.try_set_view(ViewKind::All)?,
            KeyCode::Char('2') => self.try_set_view(ViewKind::Unstaged)?,
            KeyCode::Char('3') => self.try_set_view(ViewKind::Staged)?,
            KeyCode::Char('4') => self.try_set_view(ViewKind::Base)?,
            KeyCode::Char('i') if key.modifiers.is_empty() => self.toggle_diff_view_mode()?,
            KeyCode::Char('R') if key.modifiers.is_empty() => self.reload_file_list()?,
            _ => match self.focus {
                Focus::Files => self.handle_files_key(key)?,
                Focus::Diff => self.handle_diff_key(key)?,
            },
        }

        Ok(false)
    }

    fn handle_files_key(&mut self, key: KeyEvent) -> Result<()> {
        let page = self.files_viewport_height.saturating_sub(1).max(1) as i32;
        match key.code {
            KeyCode::Up => self.select_file(-1)?,
            KeyCode::Down => self.select_file(1)?,
            KeyCode::PageUp => self.select_file(-page)?,
            KeyCode::PageDown => self.select_file(page)?,
            KeyCode::Enter => {
                self.focus = Focus::Diff;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_diff_key(&mut self, key: KeyEvent) -> Result<()> {
        let page = self.diff_viewport_height.saturating_sub(1).max(1) as i32;
        match key.code {
            KeyCode::Up => self.move_diff_cursor(-1),
            KeyCode::Down => self.move_diff_cursor(1),
            KeyCode::PageUp => self.move_diff_cursor(-page),
            KeyCode::PageDown => self.move_diff_cursor(page),
            KeyCode::Char('c') => self.begin_comment()?,
            KeyCode::Char('d') => self.delete_comment()?,
            KeyCode::Char('r') => self.toggle_resolved()?,
            _ => {}
        }
        Ok(())
    }

    fn toggle_resolved(&mut self) -> Result<()> {
        let Some(target) = self.current_comment_target() else {
            self.status = "Not a commentable target".to_string();
            return Ok(());
        };

        let new_state = match target.locator {
            CommentLocator::File => self.review.toggle_file_comment_resolved(&target.path),
            CommentLocator::Line { side, line } => {
                self.review
                    .toggle_line_comment_resolved(&target.path, side, line)
            }
        };

        match new_state {
            Some(true) => {
                self.status = "Resolved".to_string();
                self.persist_file_note(&target.path)?;
            }
            Some(false) => {
                self.status = "Unresolved".to_string();
                self.persist_file_note(&target.path)?;
            }
            None => {
                self.status = "No comment here".to_string();
            }
        }
        Ok(())
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.code == KeyCode::Esc {
            self.mode = Mode::Browse;
            self.editor_target = None;
            self.editor_buffer = crate::ui::NoteBuffer::new();
            self.status = "Canceled".to_string();
            return Ok(false);
        }

        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.accept_comment_and_move_on()?;
            return Ok(false);
        }

        match key.code {
            KeyCode::F(2) => {
                self.accept_comment_and_move_on()?;
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                // Don't quit while editing; let Esc cancel/close.
            }
            KeyCode::Up => self.editor_buffer.move_up(),
            KeyCode::Down => self.editor_buffer.move_down(),
            KeyCode::Left => self.editor_buffer.move_left(),
            KeyCode::Right => self.editor_buffer.move_right(),
            KeyCode::Home => self.editor_buffer.move_line_start(),
            KeyCode::End => self.editor_buffer.move_line_end(),
            KeyCode::Backspace => {
                self.editor_buffer.backspace();
            }
            KeyCode::Enter => self.editor_buffer.insert_newline(),
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    self.editor_buffer.insert_char(c);
                }
            }
            _ => {}
        }

        Ok(false)
    }

    fn handle_mouse(&mut self, m: MouseEvent, rects: crate::ui::LayoutRects) -> Result<()> {
        if self.mode != Mode::Browse {
            return Ok(());
        }

        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if rects.files.contains((m.column, m.row).into()) {
                    self.focus = Focus::Files;
                    let inner_y = m.row.saturating_sub(rects.files.y + 1) as usize;
                    let idx = self.file_scroll as usize + inner_y;
                    if idx < self.files.len() {
                        self.file_selected = idx;
                        self.reload_diff_for_selected()?;
                    }
                } else if rects.diff.contains((m.column, m.row).into()) {
                    self.focus = Focus::Diff;
                    let inner_y = m.row.saturating_sub(rects.diff.y + 1) as usize;
                    let visual = self.diff_scroll as u32 + inner_y as u32;
                    if !self.diff_rows.is_empty() {
                        self.diff_cursor = self.diff_row_at_visual_line(visual);
                        // Click in the left gutter to comment.
                        let inner_x = m.column.saturating_sub(rects.diff.x + 1);
                        if inner_x <= 12 {
                            self.begin_comment()?;
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp => match self.focus {
                Focus::Files => self.file_scroll = self.file_scroll.saturating_sub(3),
                Focus::Diff => self.diff_scroll = self.diff_scroll.saturating_sub(3),
            },
            MouseEventKind::ScrollDown => match self.focus {
                Focus::Files => self.file_scroll = self.file_scroll.saturating_add(3),
                Focus::Diff => self.diff_scroll = self.diff_scroll.saturating_add(3),
            },
            _ => {}
        }

        Ok(())
    }

    fn try_set_view(&mut self, v: ViewKind) -> Result<()> {
        if self.view == v {
            return Ok(());
        }
        self.view = v;
        self.reload_view()?;
        Ok(())
    }

    fn reload_view(&mut self) -> Result<()> {
        self.mode = Mode::Browse;
        self.focus = Focus::Files;
        self.show_help = false;
        self.show_prompt = false;
        self.status.clear();
        self.file_selected = 0;
        self.file_scroll = 0;
        self.diff_cursor = 0;
        self.diff_scroll = 0;
        self.editor_target = None;
        self.editor_buffer = crate::ui::NoteBuffer::new();

        if self.view == ViewKind::Base {
            let base = self
                .base_ref
                .clone()
                .or_else(|| crate::git::default_base_ref(&self.repo));
            self.base_ref = base;
            if self.base_ref.is_none() {
                self.status = "No base ref set (pass --base <ref>)".to_string();
            }
        }

        self.head_commit_oid = crate::git::head_commit_oid(&self.repo).ok();

        self.files = self.list_files_for_view()?;
        if self.view == ViewKind::Base && self.base_ref.is_none() {
            self.diff_rows.clear();
            self.review = Review::new();
            self.review.files.clear();
            return Ok(());
        }
        if self.files.is_empty() {
            self.diff_rows.clear();
            self.status = "No changes".to_string();
            self.review = Review::new();
            self.review.files.clear();
            return Ok(());
        }

        // Build an in-memory review by loading per-file notes for the current HEAD commit.
        // Notes are treated as view-agnostic (staged/unstaged/all) for worktree reviews.
        self.review = Review::new();
        self.review.files.clear();
        if let Some(head) = self.head_commit_oid {
            let base_for_key = self.base_ref_for_key().map(|s| s.to_string());
            for e in &self.files {
                let mut merged: Option<FileReview> = None;

                let mut views_to_scan = vec![ViewKind::All, ViewKind::Staged, ViewKind::Unstaged];
                if self.view == ViewKind::Base {
                    views_to_scan.push(ViewKind::Base);
                }

                for view in views_to_scan {
                    let base_for_key = match view {
                        ViewKind::Base => base_for_key.as_deref(),
                        _ => None,
                    };
                    let oid = crate::git::note_file_key_oid(
                        &self.repo,
                        head,
                        view,
                        base_for_key,
                        &e.path,
                    )?;
                    let note = crate::notes::read(&self.repo, &self.notes_ref, &oid)
                        .with_context(|| format!("read file note for '{}'", e.path))?;
                    let Some(text) = note.as_deref() else {
                        continue;
                    };
                    let Some(fr) = crate::review::decode_file_note(text) else {
                        continue;
                    };
                    merged = Some(match merged {
                        None => fr,
                        Some(mut existing) => {
                            merge_file_review(&mut existing, fr);
                            existing
                        }
                    });
                }

                if let Some(fr) = merged {
                    self.review.files.insert(e.path.clone(), fr);
                }
            }
        } else {
            self.status = "No HEAD commit (unborn branch) — notes disabled".to_string();
        }

        self.reload_diff_for_selected()?;
        Ok(())
    }

    fn reload_file_list(&mut self) -> Result<()> {
        let keep_path = self.files.get(self.file_selected).map(|e| e.path.clone());
        let keep_line = self.keep_cursor_line();

        self.head_commit_oid = crate::git::head_commit_oid(&self.repo).ok();
        self.files = self.list_files_for_view()?;

        if self.view == ViewKind::Base && self.base_ref.is_none() {
            self.diff_rows.clear();
            self.review = Review::new();
            self.review.files.clear();
            self.status = "No base ref set (pass --base <ref>)".to_string();
            return Ok(());
        }
        if self.files.is_empty() {
            self.diff_rows.clear();
            self.review = Review::new();
            self.review.files.clear();
            self.status = "No changes".to_string();
            self.file_selected = 0;
            self.file_scroll = 0;
            self.diff_cursor = 0;
            self.diff_scroll = 0;
            return Ok(());
        }

        if let Some(path) = keep_path.as_deref() {
            if let Some(idx) = self.files.iter().position(|e| e.path == path) {
                self.file_selected = idx;
            } else {
                self.file_selected = self.file_selected.min(self.files.len().saturating_sub(1));
            }
        } else {
            self.file_selected = self.file_selected.min(self.files.len().saturating_sub(1));
        }

        // Rebuild an in-memory view of notes (in case an external process edited notes).
        self.review = Review::new();
        self.review.files.clear();
        if let Some(head) = self.head_commit_oid {
            let base_for_key = self.base_ref_for_key().map(|s| s.to_string());
            for e in &self.files {
                let mut merged: Option<FileReview> = None;

                let mut views_to_scan = vec![ViewKind::All, ViewKind::Staged, ViewKind::Unstaged];
                if self.view == ViewKind::Base {
                    views_to_scan.push(ViewKind::Base);
                }

                for view in views_to_scan {
                    let base_for_key = match view {
                        ViewKind::Base => base_for_key.as_deref(),
                        _ => None,
                    };
                    let oid = crate::git::note_file_key_oid(
                        &self.repo,
                        head,
                        view,
                        base_for_key,
                        &e.path,
                    )?;
                    let note = crate::notes::read(&self.repo, &self.notes_ref, &oid)
                        .with_context(|| format!("read file note for '{}'", e.path))?;
                    let Some(text) = note.as_deref() else {
                        continue;
                    };
                    let Some(fr) = crate::review::decode_file_note(text) else {
                        continue;
                    };
                    merged = Some(match merged {
                        None => fr,
                        Some(mut existing) => {
                            merge_file_review(&mut existing, fr);
                            existing
                        }
                    });
                }

                if let Some(fr) = merged {
                    self.review.files.insert(e.path.clone(), fr);
                }
            }
        }

        self.reload_diff_for_selected()?;
        if let Some(k) = keep_line
            && let Some(idx) = self.find_row_for_keep_line(k)
        {
            self.diff_cursor = idx;
        }
        self.status = "Reloaded".to_string();
        Ok(())
    }

    fn keep_cursor_line(&self) -> Option<KeepLine> {
        let row = self.diff_rows.get(self.diff_cursor)?;
        match row {
            RenderRow::Unified(r) => match r.kind {
                crate::diff::Kind::Remove => r.old_line.map(KeepLine::Old),
                crate::diff::Kind::Add | crate::diff::Kind::Context => {
                    r.new_line.map(KeepLine::New)
                }
                _ => None,
            },
            RenderRow::SideBySide(r) => {
                if matches!(
                    r.right_kind,
                    Some(crate::diff::Kind::Add) | Some(crate::diff::Kind::Context)
                ) {
                    r.new_line.map(KeepLine::New)
                } else if matches!(r.left_kind, Some(crate::diff::Kind::Remove)) {
                    r.old_line.map(KeepLine::Old)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn find_row_for_keep_line(&self, k: KeepLine) -> Option<usize> {
        self.diff_rows.iter().position(|r| match (k, r) {
            (KeepLine::New(n), RenderRow::Unified(r)) => r.new_line == Some(n),
            (KeepLine::Old(n), RenderRow::Unified(r)) => r.old_line == Some(n),
            (KeepLine::New(n), RenderRow::SideBySide(r)) => r.new_line == Some(n),
            (KeepLine::Old(n), RenderRow::SideBySide(r)) => r.old_line == Some(n),
            _ => false,
        })
    }

    fn base_ref_for_key(&self) -> Option<&str> {
        match self.view {
            ViewKind::Base => self.base_ref.as_deref(),
            _ => None,
        }
    }

    fn list_files_for_view(&self) -> Result<Vec<FileEntry>> {
        let (mut paths, staged_set, unstaged_set) = match self.view {
            ViewKind::All => {
                let staged = crate::git::list_staged_paths(&self.repo)?;
                let unstaged = crate::git::list_unstaged_paths(&self.repo)?;

                let staged_set: std::collections::BTreeSet<String> =
                    staged.iter().cloned().collect();
                let unstaged_set: std::collections::BTreeSet<String> =
                    unstaged.iter().cloned().collect();

                let mut out = staged;
                out.extend(unstaged);
                (out, staged_set, unstaged_set)
            }
            ViewKind::Unstaged => (
                crate::git::list_unstaged_paths(&self.repo)?,
                Default::default(),
                Default::default(),
            ),
            ViewKind::Staged => (
                crate::git::list_staged_paths(&self.repo)?,
                Default::default(),
                Default::default(),
            ),
            ViewKind::Base => {
                let Some(base) = &self.base_ref else {
                    return Ok(Vec::new());
                };
                (
                    crate::git::list_base_paths(&self.repo, base)?,
                    Default::default(),
                    Default::default(),
                )
            }
        };

        paths.sort();
        paths.dedup();

        let base_tree = if self.view == ViewKind::Base {
            self.base_ref
                .as_deref()
                .map(|b| crate::git::merge_base_tree(&self.repo, b))
                .transpose()?
        } else {
            None
        };

        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let (before, after) = self.read_before_after(base_tree.as_ref(), &path)?;
            let change = match (before.is_some(), after.is_some()) {
                (false, true) => FileChangeKind::Added,
                (true, false) => FileChangeKind::Deleted,
                _ => FileChangeKind::Modified,
            };
            let stage = match self.view {
                ViewKind::All => match (staged_set.contains(&path), unstaged_set.contains(&path)) {
                    (true, true) => FileStageKind::Partial,
                    (true, false) => FileStageKind::Staged,
                    (false, true) => FileStageKind::Unstaged,
                    (false, false) => FileStageKind::None,
                },
                ViewKind::Unstaged => FileStageKind::Unstaged,
                ViewKind::Staged => FileStageKind::Staged,
                ViewKind::Base => FileStageKind::None,
            };
            out.push(FileEntry {
                path,
                change,
                stage,
            });
        }
        Ok(out)
    }

    fn reload_diff_for_selected(&mut self) -> Result<()> {
        let Some(path) = self.files.get(self.file_selected).map(|e| e.path.clone()) else {
            self.diff_rows.clear();
            return Ok(());
        };

        let base_tree = if self.view == ViewKind::Base {
            self.base_ref
                .as_deref()
                .map(|b| crate::git::merge_base_tree(&self.repo, b))
                .transpose()?
        } else {
            None
        };

        let (before, after) = self.read_before_after(base_tree.as_ref(), &path)?;
        drop(base_tree);

        let before_label = if before.is_some() {
            format!("a/{path}")
        } else {
            "/dev/null".to_string()
        };
        let after_label = if after.is_some() {
            format!("b/{path}")
        } else {
            "/dev/null".to_string()
        };

        // Build unified diff lines.
        let diff_lines_all = crate::diff::unified_file_diff(
            &before_label,
            &after_label,
            before.as_deref(),
            after.as_deref(),
        )?;
        let diff_lines: Vec<crate::diff::Line> = diff_lines_all
            .into_iter()
            .filter(|l| l.kind != crate::diff::Kind::FileHeader)
            .collect();

        let mut valid_old = HashSet::new();
        let mut valid_new = HashSet::new();
        for line in &diff_lines {
            match line.kind {
                crate::diff::Kind::Remove => {
                    if let Some(n) = line.old_line {
                        valid_old.insert(n);
                    }
                }
                crate::diff::Kind::Add | crate::diff::Kind::Context => {
                    if let Some(n) = line.new_line {
                        valid_new.insert(n);
                    }
                }
                _ => {}
            }
        }
        if self
            .review
            .prune_line_comments(&path, &valid_old, &valid_new)
            && self.head_commit_oid.is_some()
        {
            self.persist_file_note(&path)?;
        }

        let raw = diff_lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";

        let mut rows = match self.diff_view_mode {
            DiffViewMode::Unified => self.build_unified_rows(&path, &diff_lines, &raw)?,
            DiffViewMode::SideBySide => self.build_side_by_side_rows(&path, &diff_lines, &raw)?,
        };

        rows.insert(0, RenderRow::FileHeader { path: path.clone() });

        self.diff_rows = rows;
        self.diff_cursor = self
            .diff_rows
            .iter()
            .position(|r| matches!(r, RenderRow::Unified(_) | RenderRow::SideBySide(_)))
            .unwrap_or(0);
        self.diff_scroll = 0;
        self.recompute_diff_metrics(self.diff_viewport_width);
        self.status = path;
        Ok(())
    }

    fn select_file(&mut self, delta: i32) -> Result<()> {
        if self.files.is_empty() {
            self.file_selected = 0;
            return Ok(());
        }
        let cur = self.file_selected as i32;
        let max = (self.files.len() - 1) as i32;
        self.file_selected = (cur + delta).clamp(0, max) as usize;
        self.reload_diff_for_selected()?;
        Ok(())
    }

    fn ensure_file_visible(&mut self, viewport_height: u16) {
        if self.files.is_empty() {
            self.file_scroll = 0;
            self.file_selected = 0;
            return;
        }
        let viewport_height = viewport_height.max(1) as usize;
        let scroll = self.file_scroll as usize;
        if self.file_selected < scroll {
            self.file_scroll = self.file_selected as u16;
            return;
        }
        if self.file_selected >= scroll + viewport_height {
            self.file_scroll = (self.file_selected + 1 - viewport_height) as u16;
        }
    }

    fn move_diff_cursor(&mut self, delta: i32) {
        if self.diff_rows.is_empty() {
            self.diff_cursor = 0;
            return;
        }
        let cur = self.diff_cursor as i32;
        let max = (self.diff_rows.len() - 1) as i32;
        self.diff_cursor = (cur + delta).clamp(0, max) as usize;
    }

    fn ensure_diff_visible(&mut self, viewport_height: u16) {
        if self.diff_rows.is_empty() {
            self.diff_scroll = 0;
            self.diff_cursor = 0;
            return;
        }
        let viewport = viewport_height.max(1) as u32;
        let cursor_top = self
            .diff_row_offsets
            .get(self.diff_cursor)
            .copied()
            .unwrap_or(0);
        let cursor_h = self
            .diff_row_heights
            .get(self.diff_cursor)
            .copied()
            .unwrap_or(1)
            .max(1) as u32;
        let cursor_bottom = cursor_top.saturating_add(cursor_h.saturating_sub(1));

        let mut scroll = self.diff_scroll as u32;
        if cursor_top < scroll {
            scroll = cursor_top;
        } else if cursor_bottom >= scroll.saturating_add(viewport) {
            scroll = cursor_bottom.saturating_add(1).saturating_sub(viewport);
        }

        let max_scroll = self
            .diff_total_visual_lines
            .saturating_sub(viewport)
            .min(u16::MAX as u32);
        if scroll > max_scroll {
            scroll = max_scroll;
        }
        self.diff_scroll = scroll as u16;
    }

    fn clamp_diff_scroll(&mut self) {
        let viewport = self.diff_viewport_height.max(1) as u32;
        let max_scroll = self
            .diff_total_visual_lines
            .saturating_sub(viewport)
            .min(u16::MAX as u32);
        if (self.diff_scroll as u32) > max_scroll {
            self.diff_scroll = max_scroll as u16;
        }
    }

    fn diff_row_at_visual_line(&self, visual: u32) -> usize {
        if self.diff_rows.is_empty() {
            return 0;
        }
        if visual >= self.diff_total_visual_lines {
            return self.diff_rows.len().saturating_sub(1);
        }
        let i = self.diff_row_offsets.partition_point(|&off| off <= visual);
        i.saturating_sub(1)
            .min(self.diff_rows.len().saturating_sub(1))
    }

    fn recompute_diff_metrics(&mut self, inner_width: u16) {
        let width = inner_width.max(1);
        let old_max = self
            .diff_rows
            .iter()
            .filter_map(|r| match r {
                RenderRow::Unified(r) => r.old_line,
                RenderRow::SideBySide(r) => r.old_line,
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let new_max = self
            .diff_rows
            .iter()
            .filter_map(|r| match r {
                RenderRow::Unified(r) => r.new_line,
                RenderRow::SideBySide(r) => r.new_line,
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let old_w = old_max.to_string().len().max(4);
        let new_w = new_max.to_string().len().max(4);

        self.diff_row_offsets.clear();
        self.diff_row_heights.clear();
        self.diff_row_offsets.reserve(self.diff_rows.len());
        self.diff_row_heights.reserve(self.diff_rows.len());

        let mut offset: u32 = 0;
        for row in &self.diff_rows {
            // Build a single plain-text line that matches how we render (styles don't affect wrapping).
            let rendered = match row {
                RenderRow::FileHeader { path } => {
                    let old_s = " ".repeat(old_w);
                    let new_s = " ".repeat(new_w);
                    format!("  {old_s} {new_s} │ {path}")
                }
                RenderRow::Section { text } => {
                    // We render section lines padded to fit, so they never wrap.
                    let mut s = format!("┄┄ {text} ┄┄");
                    let w = width as usize;
                    let len = s.width();
                    if len < w {
                        s.push_str(&" ".repeat(w - len));
                    }
                    s
                }
                RenderRow::SideBySide(_) => String::new(),
                RenderRow::Unified(r) => {
                    let diff_prefix = match r.kind {
                        crate::diff::Kind::Add => '+',
                        crate::diff::Kind::Remove => '-',
                        crate::diff::Kind::Context => ' ',
                        _ => ' ',
                    };
                    let old_s = r
                        .old_line
                        .map(|n| format!("{n:>old_w$}"))
                        .unwrap_or_else(|| " ".repeat(old_w));
                    let new_s = r
                        .new_line
                        .map(|n| format!("{n:>new_w$}"))
                        .unwrap_or_else(|| " ".repeat(new_w));
                    let code = r
                        .spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>();
                    format!(" {diff_prefix} {old_s} {new_s} │ {code}")
                }
            };

            let h = match row {
                RenderRow::SideBySide(_) => 1usize,
                _ => word_wrap_line_count(&rendered, width),
            }
            .max(1);

            let h_u16 = h.min(u16::MAX as usize) as u16;
            self.diff_row_offsets.push(offset);
            self.diff_row_heights.push(h_u16);
            offset = offset.saturating_add(h_u16 as u32);
        }
        self.diff_total_visual_lines = offset;
    }

    fn current_comment_target(&self) -> Option<CommentTarget> {
        let path = self
            .files
            .get(self.file_selected)
            .map(|e| e.path.as_str())?;
        let row = self.diff_rows.get(self.diff_cursor)?;
        let locator = match row {
            RenderRow::FileHeader { .. } => CommentLocator::File,
            RenderRow::Unified(r) => match r.kind {
                crate::diff::Kind::Remove => CommentLocator::Line {
                    side: LineSide::Old,
                    line: r.old_line?,
                },
                crate::diff::Kind::Add | crate::diff::Kind::Context => CommentLocator::Line {
                    side: LineSide::New,
                    line: r.new_line?,
                },
                _ => return None,
            },
            RenderRow::SideBySide(r) => {
                if matches!(
                    r.right_kind,
                    Some(crate::diff::Kind::Add) | Some(crate::diff::Kind::Context)
                ) {
                    CommentLocator::Line {
                        side: LineSide::New,
                        line: r.new_line?,
                    }
                } else if matches!(r.left_kind, Some(crate::diff::Kind::Remove)) {
                    CommentLocator::Line {
                        side: LineSide::Old,
                        line: r.old_line?,
                    }
                } else {
                    return None;
                }
            }
            RenderRow::Section { .. } => return None,
        };
        Some(CommentTarget {
            path: path.to_string(),
            locator,
        })
    }

    fn begin_comment(&mut self) -> Result<()> {
        let Some(target) = self.current_comment_target() else {
            self.status = "Not a commentable line".to_string();
            return Ok(());
        };
        let existing = match target.locator {
            CommentLocator::File => self
                .review
                .file_comment(&target.path)
                .map(|c| c.body.as_str())
                .unwrap_or(""),
            CommentLocator::Line { side, line } => self
                .review
                .line_comment(&target.path, side, line)
                .map(|c| c.body.as_str())
                .unwrap_or(""),
        };
        self.editor_target = Some(target);
        self.editor_buffer = crate::ui::NoteBuffer::from_string(existing.to_string());
        self.show_help = false;
        self.show_prompt = false;
        self.mode = Mode::EditComment;
        Ok(())
    }

    fn accept_comment_and_move_on(&mut self) -> Result<()> {
        let Some(target) = self.editor_target.clone() else {
            self.mode = Mode::Browse;
            return Ok(());
        };

        let comment = self.editor_buffer.as_string();
        if comment.trim().is_empty() {
            let removed = match target.locator {
                CommentLocator::File => self.review.remove_file_comment(&target.path),
                CommentLocator::Line { side, line } => {
                    self.review.remove_line_comment(&target.path, side, line)
                }
            };
            if removed {
                self.persist_file_note(&target.path)?;
                self.status = "Saved".to_string();
            }
        } else {
            match target.locator {
                CommentLocator::File => self.review.set_file_comment(&target.path, comment),
                CommentLocator::Line { side, line } => {
                    self.review
                        .set_line_comment(&target.path, side, line, comment)
                }
            }
            self.persist_file_note(&target.path)?;
            self.status = "Saved".to_string();
        }

        self.mode = Mode::Browse;
        self.editor_target = None;
        self.editor_buffer = crate::ui::NoteBuffer::new();

        for i in (self.diff_cursor + 1)..self.diff_rows.len() {
            let commentable = match &self.diff_rows[i] {
                RenderRow::FileHeader { .. } => true,
                RenderRow::Unified(r) => match r.kind {
                    crate::diff::Kind::Remove => r.old_line.is_some(),
                    crate::diff::Kind::Add | crate::diff::Kind::Context => r.new_line.is_some(),
                    _ => false,
                },
                RenderRow::SideBySide(r) => {
                    (matches!(
                        r.right_kind,
                        Some(crate::diff::Kind::Add) | Some(crate::diff::Kind::Context)
                    ) && r.new_line.is_some())
                        || (matches!(r.left_kind, Some(crate::diff::Kind::Remove))
                            && r.old_line.is_some())
                }
                RenderRow::Section { .. } => false,
            };
            if commentable {
                self.diff_cursor = i;
                return Ok(());
            }
        }
        self.status = "End of file".to_string();
        Ok(())
    }

    fn delete_comment(&mut self) -> Result<()> {
        let Some(target) = self.current_comment_target() else {
            self.status = "Not a commentable line".to_string();
            return Ok(());
        };
        let removed = match target.locator {
            CommentLocator::File => self.review.remove_file_comment(&target.path),
            CommentLocator::Line { side, line } => {
                self.review.remove_line_comment(&target.path, side, line)
            }
        };
        if removed {
            self.status = "Deleted comment".to_string();
            self.persist_file_note(&target.path)?;
        } else {
            self.status = "No comment on this line".to_string();
        }
        Ok(())
    }

    fn persist_file_note(&mut self, path: &str) -> Result<()> {
        let Some(head) = self.head_commit_oid else {
            self.status = "No HEAD commit — cannot write notes".to_string();
            return Ok(());
        };
        self.persist_file_note_with_head(head, path)?;
        Ok(())
    }

    fn persist_file_note_with_head(&mut self, head: ObjectId, path: &str) -> Result<bool> {
        // Worktree review notes are view-agnostic: always store them under the `all` key.
        // Base-diff notes stay under the `base` key because line anchors are relative to that diff.
        let (view_for_key, base_for_key) = match self.view {
            ViewKind::Base => (ViewKind::Base, self.base_ref_for_key()),
            _ => (ViewKind::All, None),
        };

        let oid =
            crate::git::note_file_key_oid(&self.repo, head, view_for_key, base_for_key, path)?;

        match self.review.files.get(path) {
            Some(file) if !file.comments.is_empty() || file.file_comment.is_some() => {
                let note = crate::review::encode_file_note(file);
                crate::notes::write(&self.repo, &self.notes_ref, &oid, Some(&note))
                    .with_context(|| format!("write file note for '{path}'"))?;
                Ok(true)
            }
            _ => {
                crate::notes::write(&self.repo, &self.notes_ref, &oid, None)
                    .with_context(|| format!("delete file note for '{path}'"))?;
                Ok(false)
            }
        }
    }
}

fn next_focus(f: Focus) -> Focus {
    match f {
        Focus::Files => Focus::Diff,
        Focus::Diff => Focus::Files,
    }
}

fn prev_focus(f: Focus) -> Focus {
    match f {
        Focus::Files => Focus::Diff,
        Focus::Diff => Focus::Files,
    }
}

fn merge_file_review(target: &mut FileReview, incoming: FileReview) {
    match (&target.file_comment, &incoming.file_comment) {
        (None, Some(_)) => target.file_comment = incoming.file_comment,
        (Some(t), Some(i)) if t.resolved && !i.resolved => {
            target.file_comment = incoming.file_comment
        }
        _ => {}
    }

    for (k, v) in incoming.comments {
        match target.comments.get(&k) {
            None => {
                target.comments.insert(k, v);
            }
            Some(existing) if existing.resolved && !v.resolved => {
                target.comments.insert(k, v);
            }
            _ => {}
        }
    }
}

impl App {
    fn read_before_after(
        &self,
        base_tree: Option<&gix::Tree<'_>>,
        path: &str,
    ) -> Result<(Option<String>, Option<String>)> {
        let out = match self.view {
            ViewKind::All => (
                crate::git::try_read_head(&self.repo, path)?,
                crate::git::try_read_worktree(&self.repo, path)?,
            ),
            ViewKind::Unstaged => (
                crate::git::try_read_index(&self.repo, path)?,
                crate::git::try_read_worktree(&self.repo, path)?,
            ),
            ViewKind::Staged => (
                crate::git::try_read_head(&self.repo, path)?,
                crate::git::try_read_index(&self.repo, path)?,
            ),
            ViewKind::Base => (
                match base_tree {
                    Some(t) => crate::git::try_read_tree(t, path)?,
                    None => None,
                },
                crate::git::try_read_head(&self.repo, path)?,
            ),
        };
        Ok(out)
    }

    fn toggle_diff_view_mode(&mut self) -> Result<()> {
        let keep_new_line = self.diff_rows.get(self.diff_cursor).and_then(|r| match r {
            RenderRow::Unified(r) => r.new_line,
            RenderRow::SideBySide(r) => r.new_line,
            RenderRow::Section { .. } => None,
            RenderRow::FileHeader { .. } => None,
        });

        self.diff_view_mode = match self.diff_view_mode {
            DiffViewMode::Unified => DiffViewMode::SideBySide,
            DiffViewMode::SideBySide => DiffViewMode::Unified,
        };

        self.reload_diff_for_selected()?;

        if let Some(n) = keep_new_line
            && let Some(idx) = self.diff_rows.iter().position(|r| match r {
                RenderRow::Unified(r) => r.new_line == Some(n),
                RenderRow::SideBySide(r) => r.new_line == Some(n),
                RenderRow::Section { .. } => false,
                RenderRow::FileHeader { .. } => false,
            })
        {
            self.diff_cursor = idx;
        }

        Ok(())
    }

    fn build_unified_rows(
        &self,
        path: &str,
        diff_lines: &[crate::diff::Line],
        raw: &str,
    ) -> Result<Vec<RenderRow>> {
        let mut diff_hl = self.highlighter.highlight_diff(raw)?;
        if diff_hl.len() > diff_lines.len() {
            diff_hl.truncate(diff_lines.len());
        }

        let lang = self.highlighter.detect_file_lang(&self.repo, path);
        let mut code_block = String::new();
        let mut code_map: Vec<(usize, usize)> = Vec::new(); // (diff_idx, code_idx)
        for (idx, dl) in diff_lines.iter().enumerate() {
            match dl.kind {
                crate::diff::Kind::Context | crate::diff::Kind::Add | crate::diff::Kind::Remove => {
                    let code = dl.text.get(1..).unwrap_or("").to_string();
                    let code_idx = code_map.len();
                    code_map.push((idx, code_idx));
                    code_block.push_str(&code);
                    code_block.push('\n');
                }
                _ => {}
            }
        }

        let code_hl: Vec<Vec<ratatui::text::Span<'static>>> = match lang {
            Some(lang) => self.highlighter.highlight_lang(lang, &code_block)?,
            None => Vec::new(),
        };

        let mut code_by_diff: Vec<Option<Vec<ratatui::text::Span<'static>>>> =
            vec![None; diff_lines.len()];
        for (diff_idx, code_idx) in code_map {
            if let Some(line) = code_hl.get(code_idx).cloned() {
                code_by_diff[diff_idx] = Some(line);
            }
        }

        let mut rows = Vec::with_capacity(diff_lines.len());
        for (idx, dl) in diff_lines.iter().enumerate() {
            if dl.kind == crate::diff::Kind::HunkHeader {
                rows.push(RenderRow::Section {
                    text: dl.text.clone(),
                });
                continue;
            }

            let spans = match dl.kind {
                crate::diff::Kind::Context | crate::diff::Kind::Add | crate::diff::Kind::Remove => {
                    code_by_diff[idx].clone().unwrap_or_else(|| {
                        vec![ratatui::text::Span::raw(
                            dl.text.get(1..).unwrap_or("").to_string(),
                        )]
                    })
                }
                _ => diff_hl
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| vec![ratatui::text::Span::raw(dl.text.clone())]),
            };
            rows.push(RenderRow::Unified(DiffRow {
                kind: dl.kind,
                old_line: dl.old_line,
                new_line: dl.new_line,
                spans,
            }));
        }

        Ok(rows)
    }

    fn build_side_by_side_rows(
        &self,
        path: &str,
        diff_lines: &[crate::diff::Line],
        _raw: &str,
    ) -> Result<Vec<RenderRow>> {
        struct Temp {
            left_code: Option<String>,
            right_code: Option<String>,
        }

        let mut rows: Vec<RenderRow> = Vec::new();
        let mut temps: Vec<Option<Temp>> = Vec::new(); // parallel to `rows`

        let mut i = 0usize;
        while i < diff_lines.len() {
            let dl = &diff_lines[i];
            match dl.kind {
                crate::diff::Kind::HunkHeader => {
                    rows.push(RenderRow::Section {
                        text: dl.text.clone(),
                    });
                    temps.push(None);
                    i += 1;
                }
                crate::diff::Kind::Context => {
                    let code = dl.text.get(1..).unwrap_or("").to_string();
                    rows.push(RenderRow::SideBySide(SideBySideRow {
                        left_kind: Some(crate::diff::Kind::Context),
                        right_kind: Some(crate::diff::Kind::Context),
                        old_line: dl.old_line,
                        new_line: dl.new_line,
                        left_spans: vec![ratatui::text::Span::raw(code.clone())],
                        right_spans: vec![ratatui::text::Span::raw(code.clone())],
                    }));
                    temps.push(Some(Temp {
                        left_code: Some(code.clone()),
                        right_code: Some(code),
                    }));
                    i += 1;
                }
                crate::diff::Kind::Remove | crate::diff::Kind::Add => {
                    let mut removes: Vec<crate::diff::Line> = Vec::new();
                    let mut adds: Vec<crate::diff::Line> = Vec::new();
                    while i < diff_lines.len() {
                        let k = diff_lines[i].kind;
                        if k != crate::diff::Kind::Remove && k != crate::diff::Kind::Add {
                            break;
                        }
                        if k == crate::diff::Kind::Remove {
                            removes.push(diff_lines[i].clone());
                        } else {
                            adds.push(diff_lines[i].clone());
                        }
                        i += 1;
                    }

                    let max = removes.len().max(adds.len());
                    for j in 0..max {
                        let left = removes.get(j);
                        let right = adds.get(j);
                        let left_code = left.map(|l| l.text.get(1..).unwrap_or("").to_string());
                        let right_code = right.map(|l| l.text.get(1..).unwrap_or("").to_string());
                        rows.push(RenderRow::SideBySide(SideBySideRow {
                            left_kind: left.map(|_| crate::diff::Kind::Remove),
                            right_kind: right.map(|_| crate::diff::Kind::Add),
                            old_line: left.and_then(|l| l.old_line),
                            new_line: right.and_then(|l| l.new_line),
                            left_spans: vec![ratatui::text::Span::raw(
                                left_code.clone().unwrap_or_default(),
                            )],
                            right_spans: vec![ratatui::text::Span::raw(
                                right_code.clone().unwrap_or_default(),
                            )],
                        }));
                        temps.push(Some(Temp {
                            left_code,
                            right_code,
                        }));
                    }
                }
                _ => {
                    i += 1;
                }
            }
        }

        let Some(lang) = self.highlighter.detect_file_lang(&self.repo, path) else {
            return Ok(rows);
        };

        let mut left_block = String::new();
        let mut left_map: Vec<(usize, usize)> = Vec::new(); // (row_idx, line_idx)
        let mut right_block = String::new();
        let mut right_map: Vec<(usize, usize)> = Vec::new();

        let mut left_line = 0usize;
        let mut right_line = 0usize;
        for (row_idx, t) in temps.iter().enumerate() {
            let Some(t) = t else { continue };
            if let Some(code) = &t.left_code {
                left_map.push((row_idx, left_line));
                left_line += 1;
                left_block.push_str(code);
                left_block.push('\n');
            }
            if let Some(code) = &t.right_code {
                right_map.push((row_idx, right_line));
                right_line += 1;
                right_block.push_str(code);
                right_block.push('\n');
            }
        }

        let left_hl = self.highlighter.highlight_lang(lang, &left_block)?;
        let right_hl = self.highlighter.highlight_lang(lang, &right_block)?;

        for (row_idx, line_idx) in left_map {
            if let Some(line) = left_hl.get(line_idx).cloned()
                && let RenderRow::SideBySide(r) = &mut rows[row_idx]
            {
                r.left_spans = line;
            }
        }
        for (row_idx, line_idx) in right_map {
            if let Some(line) = right_hl.get(line_idx).cloned()
                && let RenderRow::SideBySide(r) = &mut rows[row_idx]
            {
                r.right_spans = line;
            }
        }

        Ok(rows)
    }
}

fn word_wrap_line_count(s: &str, width: u16) -> usize {
    let width = width.max(1) as usize;
    if s.is_empty() {
        return 1;
    }

    let mut lines = 1usize;
    let mut cur = 0usize;

    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.peek().copied() {
        if ch.is_whitespace() {
            // Whitespace is a break opportunity; it can span multiple lines.
            let mut ws = String::new();
            while let Some(c) = chars.peek().copied() {
                if !c.is_whitespace() {
                    break;
                }
                ws.push(c);
                chars.next();
            }
            let mut ws_w = ws.width();
            while ws_w > 0 {
                let space_left = width.saturating_sub(cur);
                if space_left == 0 {
                    lines += 1;
                    cur = 0;
                    continue;
                }
                let take = ws_w.min(space_left);
                ws_w -= take;
                cur += take;
                if ws_w > 0 && cur == width {
                    lines += 1;
                    cur = 0;
                }
            }
        } else {
            // Consume a non-whitespace run ("word").
            let mut word = String::new();
            while let Some(c) = chars.peek().copied() {
                if c.is_whitespace() {
                    break;
                }
                word.push(c);
                chars.next();
            }
            let mut w = word.width();
            if w == 0 {
                continue;
            }
            loop {
                let space_left = width.saturating_sub(cur);
                if space_left == 0 {
                    lines += 1;
                    cur = 0;
                    continue;
                }
                if w <= space_left {
                    cur += w;
                    break;
                }
                // Word doesn't fit.
                if cur > 0 {
                    lines += 1;
                    cur = 0;
                    continue;
                }
                // Word is longer than a full line; split.
                lines += w / width;
                w %= width;
                cur = 0;
                if w == 0 {
                    break;
                }
                cur = w;
                break;
            }
        }
    }

    lines
}
