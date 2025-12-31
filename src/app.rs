use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use gix::ObjectId;

use crate::file_tree::FileTreeView;
use crate::git::ViewKind;
use crate::highlight::Highlighter;
use crate::review::{FileReview, LineSide, Review};
use unicode_width::UnicodeWidthStr;

const DEFAULT_NOTES_REF: &str = "refs/notes/remark";
const CONFIG_DIFF_VIEW_KEY: &str = "remark.diffView";
const CONFIG_DIFF_CONTEXT_KEY: &str = "remark.diffContext";
const DEFAULT_DIFF_CONTEXT: u32 = 3;
const MIN_DIFF_CONTEXT: u32 = 0;
const MAX_DIFF_CONTEXT: u32 = 20;

#[derive(Debug, Clone)]
struct Config {
    notes_ref: String,
    base_ref: Option<String>,
    show_ignored: bool,
}

impl Config {
    fn from_env(repo: &gix::Repository) -> Self {
        let mut notes_ref = DEFAULT_NOTES_REF.to_string();
        let mut base_ref: Option<String> = crate::git::default_base_ref(repo);
        let mut show_ignored = false;

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
                "--ignored" => {
                    show_ignored = true;
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
            show_ignored,
        }
    }
}

fn print_help_and_exit() -> ! {
    eprintln!(
        "remark\n\nUSAGE:\n  remark [--ref <notes-ref>] [--base <ref>] [--ignored]\n\nOPTIONS:\n  --ref <notes-ref>   Notes ref to store reviews (default: {DEFAULT_NOTES_REF})\n  --base <ref>        Base ref for base view (default: @{{upstream}} / main / master)\n  --ignored           Include gitignored files in the file list\n\nKEYS (browse):\n  h / l or Left/Right focus files/diff\n  1/2/3/4             all/unstaged/staged/base\n  i                   toggle unified/side-by-side diff\n  [ / ]               less/more diff context\n  I                   toggle showing ignored files\n  R                   reload file list\n  Up/Down, j/k        navigate (focused pane)\n  PgUp/PgDn           scroll (focused pane)\n  Ctrl+U / Ctrl+D     page up/down (focused pane)\n  Ctrl+N / Ctrl+P     next/prev unreviewed file (diff pane)\n  v                   toggle reviewed (selected file)\n  Enter               focus diff (from files)\n  c                   add/edit comment (file or line)\n  d                   delete comment (file or line)\n  r                   resolve/unresolve comment\n  p                   open prompt editor\n  ?                   help\n  Esc                 dismiss overlay or quit\n\nTIP:\n  With focus on Files, press `c` to add/edit a file-level comment for the selected file.\n\nKEYS (comment editor):\n  Enter               newline\n  Shift+Enter / Ctrl+S accept comment and close\n  Esc                 cancel\n\nKEYS (prompt editor):\n  Enter               newline\n  Shift+Enter / Ctrl+S copy prompt and close\n  Esc                 close prompt\n"
    );
    std::process::exit(2);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Browse,
    EditComment,
    EditPrompt,
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

    let mut app = App::new(repo, config.notes_ref, config.base_ref, config.show_ignored)?;
    let res = app.run_loop(&mut ui);

    ui.restore().ok();
    res
}

struct App {
    repo: gix::Repository,
    notes_ref: String,
    base_ref: Option<String>,
    show_ignored: bool,

    view: ViewKind,
    focus: Focus,
    mode: Mode,
    diff_view_mode: DiffViewMode,
    diff_context: u32,

    head_commit_oid: Option<ObjectId>,
    review: Review,

    files: Vec<FileEntry>,
    file_tree: FileTreeView,
    file_selected: usize,
    file_scroll: u16,
    files_viewport_height: u16,
    manual_scroll: bool,

    diff_rows: Vec<RenderRow>,
    diff_cursor: usize,
    diff_scroll: u16,
    diff_viewport_height: u16,
    diff_viewport_width: u16,
    diff_row_offsets: Vec<u32>,
    diff_row_heights: Vec<u16>,
    diff_total_visual_lines: u32,

    reviewed_files: HashSet<String>,

    editor_target: Option<CommentTarget>,
    editor_buffer: crate::ui::NoteBuffer,
    prompt_buffer: crate::ui::NoteBuffer,
    prompt_scroll: u16,
    prompt_viewport_height: u16,

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

#[derive(Debug, Clone)]
pub(crate) struct FileEntry {
    pub(crate) path: String,
    pub(crate) change: FileChangeKind,
    pub(crate) git_xy: [char; 2],
}

impl App {
    fn new(
        repo: gix::Repository,
        notes_ref: String,
        base_ref: Option<String>,
        show_ignored: bool,
    ) -> Result<Self> {
        let highlighter = Highlighter::new()?;
        let diff_view_mode = match crate::git::read_local_config_value(&repo, CONFIG_DIFF_VIEW_KEY)
            .ok()
            .flatten()
            .as_deref()
        {
            Some(v) => parse_diff_view_mode(v).unwrap_or(DiffViewMode::Unified),
            None => DiffViewMode::Unified,
        };
        let diff_context = match crate::git::read_local_config_value(&repo, CONFIG_DIFF_CONTEXT_KEY)
            .ok()
            .flatten()
            .as_deref()
        {
            Some(v) => parse_diff_context(v).unwrap_or(DEFAULT_DIFF_CONTEXT),
            None => DEFAULT_DIFF_CONTEXT,
        };
        let mut app = Self {
            repo,
            notes_ref,
            base_ref,
            show_ignored,
            view: ViewKind::All,
            focus: Focus::Files,
            mode: Mode::Browse,
            diff_view_mode,
            diff_context,
            head_commit_oid: None,
            review: Review::new(),
            files: Vec::new(),
            file_tree: FileTreeView::default(),
            file_selected: 0,
            file_scroll: 0,
            files_viewport_height: 1,
            manual_scroll: false,
            diff_rows: Vec::new(),
            diff_cursor: 0,
            diff_scroll: 0,
            diff_viewport_height: 1,
            diff_viewport_width: 1,
            diff_row_offsets: Vec::new(),
            diff_row_heights: Vec::new(),
            diff_total_visual_lines: 0,
            reviewed_files: HashSet::new(),
            editor_target: None,
            editor_buffer: crate::ui::NoteBuffer::new(),
            prompt_buffer: crate::ui::NoteBuffer::new(),
            prompt_scroll: 0,
            prompt_viewport_height: 1,
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
            if self.show_prompt {
                let popup = crate::ui::prompt_popup_rect(outer);
                self.prompt_viewport_height = popup.height.saturating_sub(2).max(1);
            } else {
                self.prompt_viewport_height = 1;
            }
            self.recompute_diff_metrics(self.diff_viewport_width);
            if !self.manual_scroll {
                self.ensure_file_visible(self.files_viewport_height);
                self.ensure_diff_visible(self.diff_viewport_height);
            }
            self.clamp_file_scroll();
            self.clamp_diff_scroll();
            if self.mode == Mode::EditPrompt {
                self.ensure_prompt_visible();
            }

            ui.terminal
                .draw(|f| {
                    crate::ui::draw(
                        f,
                        crate::ui::DrawState {
                            view: self.view,
                            base_ref: self.base_ref.as_deref(),
                            focus: self.focus,
                            mode: self.mode,
                            diff_view_mode: self.effective_diff_view_mode(),
                            diff_context: self.diff_context,
                            review: &self.review,
                            files: &self.files,
                            file_rows: &self.file_tree.rows,
                            file_selected: self.file_selected,
                            file_row_selected: self.file_tree.selected_row(self.file_selected),
                            file_scroll: self.file_scroll,
                            diff_rows: &self.diff_rows,
                            diff_cursor: self.diff_cursor,
                            diff_scroll: self.diff_scroll,
                            diff_cursor_visual: self
                                .diff_row_offsets
                                .get(self.diff_cursor)
                                .copied()
                                .unwrap_or(0),
                            reviewed_files: &self.reviewed_files,
                            editor_target: self.editor_target.as_ref(),
                            editor_buffer: &self.editor_buffer,
                            prompt_buffer: &self.prompt_buffer,
                            prompt_scroll: self.prompt_scroll,
                            status: &self.status,
                            show_help: self.show_help,
                            show_prompt: self.show_prompt,
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
            Mode::EditPrompt => self.handle_prompt_key(key),
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> Result<bool> {
        // Keyboard interaction implies "cursor-following" again.
        self.manual_scroll = false;

        let no_ctrl_alt = !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT);
        if no_ctrl_alt {
            match key.code {
                KeyCode::Char('Q') => return Ok(true),
                KeyCode::Char('q') => {
                    return Ok(true);
                }
                KeyCode::Char('I') => {
                    self.show_ignored = !self.show_ignored;
                    self.reload_file_list()?;
                    self.status = if self.show_ignored {
                        "Showing ignored files".to_string()
                    } else {
                        "Hiding ignored files".to_string()
                    };
                    return Ok(false);
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

        if key.code == KeyCode::Esc {
            return Ok(true);
        }

        if key.code == KeyCode::Char('p') && key.modifiers.is_empty() {
            if self.show_prompt {
                self.show_prompt = false;
                self.mode = Mode::Browse;
            } else {
                self.show_prompt = true;
                self.prompt_buffer =
                    crate::ui::NoteBuffer::from_string(crate::review::render_prompt(&self.review));
                self.prompt_scroll = 0;
                self.mode = Mode::EditPrompt;
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Char('1') => self.try_set_view(ViewKind::All)?,
            KeyCode::Char('2') => self.try_set_view(ViewKind::Unstaged)?,
            KeyCode::Char('3') => self.try_set_view(ViewKind::Staged)?,
            KeyCode::Char('4') => self.try_set_view(ViewKind::Base)?,
            KeyCode::Char('i') if key.modifiers.is_empty() => self.toggle_diff_view_mode()?,
            KeyCode::Char('R') if no_ctrl_alt => self.reload_file_list()?,
            KeyCode::Char('h') if key.modifiers.is_empty() => self.focus = Focus::Files,
            KeyCode::Char('l') if key.modifiers.is_empty() => self.focus = Focus::Diff,
            KeyCode::Left if no_ctrl_alt => self.focus = Focus::Files,
            KeyCode::Right if no_ctrl_alt => self.focus = Focus::Diff,
            KeyCode::Char('v') if key.modifiers.is_empty() => self.toggle_reviewed_for_selected(),
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
            KeyCode::Char('k') if key.modifiers.is_empty() => self.select_file(-1)?,
            KeyCode::Char('j') if key.modifiers.is_empty() => self.select_file(1)?,
            KeyCode::PageUp => self.select_file(-page)?,
            KeyCode::PageDown => self.select_file(page)?,
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_file(-page)?
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_file(page)?
            }
            KeyCode::Enter => {
                self.focus = Focus::Diff;
            }
            KeyCode::Char('c') if key.modifiers.is_empty() => self.begin_file_comment()?,
            _ => {}
        }
        Ok(())
    }

    fn handle_diff_key(&mut self, key: KeyEvent) -> Result<()> {
        let page = self.diff_viewport_height.saturating_sub(1).max(1) as i32;
        match key.code {
            KeyCode::Up => self.move_diff_cursor(-1),
            KeyCode::Down => self.move_diff_cursor(1),
            KeyCode::Char('k') if key.modifiers.is_empty() => self.move_diff_cursor(-1),
            KeyCode::Char('j') if key.modifiers.is_empty() => self.move_diff_cursor(1),
            KeyCode::PageUp => self.move_diff_cursor(-page),
            KeyCode::PageDown => self.move_diff_cursor(page),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_diff_cursor(-page)
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_diff_cursor(page)
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_next_unreviewed(1)?;
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_next_unreviewed(-1)?;
            }
            KeyCode::Char('c') if key.modifiers.is_empty() => self.begin_comment()?,
            KeyCode::Char('d') if key.modifiers.is_empty() => self.delete_comment()?,
            KeyCode::Char('r') if key.modifiers.is_empty() => self.toggle_resolved()?,
            KeyCode::Char('[') if key.modifiers.is_empty() => self.adjust_diff_context(-1)?,
            KeyCode::Char(']') if key.modifiers.is_empty() => self.adjust_diff_context(1)?,
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

        if key.code == KeyCode::Char('s')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            self.accept_comment_and_move_on()?;
            return Ok(false);
        }

        match key.code {
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
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

    fn handle_prompt_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.code == KeyCode::Esc {
            self.mode = Mode::Browse;
            self.show_prompt = false;
            self.status = "Closed prompt preview".to_string();
            return Ok(false);
        }

        if key.code == KeyCode::Char('s')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            let prompt = self.prompt_buffer.as_string();
            match crate::clipboard::copy(&prompt) {
                Ok(method) => self.status = format!("Copied prompt to clipboard ({method})"),
                Err(e) => self.status = format!("Clipboard failed: {e}"),
            }
            self.mode = Mode::Browse;
            self.show_prompt = false;
            return Ok(false);
        }

        if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::SHIFT) {
            let prompt = self.prompt_buffer.as_string();
            match crate::clipboard::copy(&prompt) {
                Ok(method) => self.status = format!("Copied prompt to clipboard ({method})"),
                Err(e) => self.status = format!("Clipboard failed: {e}"),
            }
            self.mode = Mode::Browse;
            self.show_prompt = false;
            return Ok(false);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                // Don't quit while editing; let Esc close.
            }
            KeyCode::Up => self.prompt_buffer.move_up(),
            KeyCode::Down => self.prompt_buffer.move_down(),
            KeyCode::Left => self.prompt_buffer.move_left(),
            KeyCode::Right => self.prompt_buffer.move_right(),
            KeyCode::Home => self.prompt_buffer.move_line_start(),
            KeyCode::End => self.prompt_buffer.move_line_end(),
            KeyCode::Backspace => {
                self.prompt_buffer.backspace();
            }
            KeyCode::Enter => self.prompt_buffer.insert_newline(),
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    self.prompt_buffer.insert_char(c);
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

        let pos = (m.column, m.row).into();
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.manual_scroll = false;
                if rects.files.contains(pos) {
                    self.focus = Focus::Files;
                    let inner_y = m.row.saturating_sub(rects.files.y + 1) as usize;
                    let row_idx = self.file_scroll as usize + inner_y;
                    if let Some(file_index) = self.file_tree.file_at_row(row_idx) {
                        self.file_selected = file_index;
                        self.reload_diff_for_selected()?;
                    }
                } else if rects.diff.contains(pos) {
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
            MouseEventKind::ScrollUp => {
                self.manual_scroll = true;
                if rects.files.contains(pos) {
                    self.file_scroll = self.file_scroll.saturating_sub(3);
                    self.clamp_file_scroll();
                } else if rects.diff.contains(pos) {
                    self.diff_scroll = self.diff_scroll.saturating_sub(3);
                    self.clamp_diff_scroll();
                } else {
                    match self.focus {
                        Focus::Files => {
                            self.file_scroll = self.file_scroll.saturating_sub(3);
                            self.clamp_file_scroll();
                        }
                        Focus::Diff => {
                            self.diff_scroll = self.diff_scroll.saturating_sub(3);
                            self.clamp_diff_scroll();
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                self.manual_scroll = true;
                if rects.files.contains(pos) {
                    self.file_scroll = self.file_scroll.saturating_add(3);
                    self.clamp_file_scroll();
                } else if rects.diff.contains(pos) {
                    self.diff_scroll = self.diff_scroll.saturating_add(3);
                    self.clamp_diff_scroll();
                } else {
                    match self.focus {
                        Focus::Files => {
                            self.file_scroll = self.file_scroll.saturating_add(3);
                            self.clamp_file_scroll();
                        }
                        Focus::Diff => {
                            self.diff_scroll = self.diff_scroll.saturating_add(3);
                            self.clamp_diff_scroll();
                        }
                    }
                }
            }
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
        self.manual_scroll = false;
        self.show_help = false;
        self.show_prompt = false;
        self.prompt_buffer = crate::ui::NoteBuffer::new();
        self.prompt_scroll = 0;
        self.reviewed_files.clear();
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
        self.file_tree = FileTreeView::build(&self.files);
        if self.view == ViewKind::Base && self.base_ref.is_none() {
            self.diff_rows.clear();
            self.review = Review::new();
            self.review.files.clear();
            self.file_selected = 0;
            self.file_scroll = 0;
            self.file_tree = FileTreeView::default();
            return Ok(());
        }
        if self.files.is_empty() {
            self.diff_rows.clear();
            self.status = "No changes".to_string();
            self.review = Review::new();
            self.review.files.clear();
            self.file_selected = 0;
            self.file_scroll = 0;
            self.file_tree = FileTreeView::default();
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

        self.reviewed_files = self
            .review
            .files
            .iter()
            .filter(|(_, f)| f.reviewed)
            .map(|(path, _)| path.clone())
            .collect();

        self.reload_diff_for_selected()?;
        Ok(())
    }

    fn reload_file_list(&mut self) -> Result<()> {
        let keep_path = self.files.get(self.file_selected).map(|e| e.path.clone());
        let keep_line = self.keep_cursor_line();
        self.manual_scroll = false;
        self.prompt_scroll = 0;
        self.reviewed_files.clear();

        self.head_commit_oid = crate::git::head_commit_oid(&self.repo).ok();
        self.files = self.list_files_for_view()?;
        self.file_tree = FileTreeView::build(&self.files);

        if self.view == ViewKind::Base && self.base_ref.is_none() {
            self.diff_rows.clear();
            self.review = Review::new();
            self.review.files.clear();
            self.status = "No base ref set (pass --base <ref>)".to_string();
            self.file_selected = 0;
            self.file_scroll = 0;
            self.file_tree = FileTreeView::default();
            return Ok(());
        }
        if self.files.is_empty() {
            self.diff_rows.clear();
            self.review = Review::new();
            self.review.files.clear();
            self.status = "No changes".to_string();
            self.file_selected = 0;
            self.file_scroll = 0;
            self.file_tree = FileTreeView::default();
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
        // Ensure the tree mapping is up-to-date for the selected file.
        self.file_tree = FileTreeView::build(&self.files);

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

        self.reviewed_files = self
            .review
            .files
            .iter()
            .filter(|(_, f)| f.reviewed)
            .map(|(path, _)| path.clone())
            .collect();

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
        let (paths, staged_status, unstaged_status) = match self.view {
            ViewKind::All => {
                let staged_status = crate::git::list_staged_status(&self.repo)?;
                let unstaged_vec = crate::git::list_unstaged(&self.repo, self.show_ignored)?;

                let mut unstaged_status = std::collections::BTreeMap::new();
                for (p, st) in unstaged_vec {
                    unstaged_status.insert(p, st);
                }

                let mut paths: Vec<String> = staged_status
                    .keys()
                    .cloned()
                    .chain(unstaged_status.keys().cloned())
                    .collect();
                paths.sort();
                paths.dedup();

                (paths, staged_status, unstaged_status)
            }
            ViewKind::Unstaged => {
                let unstaged_vec = crate::git::list_unstaged(&self.repo, self.show_ignored)?;
                let mut unstaged_status = std::collections::BTreeMap::new();
                for (p, st) in unstaged_vec {
                    unstaged_status.insert(p, st);
                }
                let mut paths: Vec<String> = unstaged_status.keys().cloned().collect();
                paths.sort();
                (paths, Default::default(), unstaged_status)
            }
            ViewKind::Staged => {
                let staged_status = crate::git::list_staged_status(&self.repo)?;
                let mut paths: Vec<String> = staged_status.keys().cloned().collect();
                paths.sort();
                (paths, staged_status, Default::default())
            }
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

            let git_xy = match self.view {
                ViewKind::All => {
                    let x = staged_status.get(&path).copied().unwrap_or('-');
                    match unstaged_status.get(&path) {
                        Some(crate::git::UnstagedStatus::Untracked) => ['-', 'N'],
                        Some(crate::git::UnstagedStatus::Ignored) => ['-', 'I'],
                        Some(crate::git::UnstagedStatus::Changed(y)) => [x, *y],
                        None => [x, '-'],
                    }
                }
                ViewKind::Unstaged => match unstaged_status.get(&path) {
                    Some(crate::git::UnstagedStatus::Untracked) => ['-', 'N'],
                    Some(crate::git::UnstagedStatus::Ignored) => ['-', 'I'],
                    Some(crate::git::UnstagedStatus::Changed(y)) => ['-', *y],
                    None => ['-', '-'],
                },
                ViewKind::Staged => {
                    let x = staged_status.get(&path).copied().unwrap_or('-');
                    [x, '-']
                }
                ViewKind::Base => {
                    let x = match change {
                        FileChangeKind::Added => 'A',
                        FileChangeKind::Deleted => 'D',
                        FileChangeKind::Modified => 'M',
                    };
                    [x, '-']
                }
            };
            out.push(FileEntry {
                path,
                change,
                git_xy,
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
            self.diff_context,
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

        let mut rows = match self.effective_diff_view_mode() {
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
            self.file_tree = FileTreeView::default();
            return;
        }
        let viewport_height = viewport_height.max(1) as usize;
        let selected_row = self.file_tree.selected_row(self.file_selected).unwrap_or(0);
        let scroll = self.file_scroll as usize;
        if selected_row < scroll {
            self.file_scroll = selected_row as u16;
            return;
        }
        if selected_row >= scroll + viewport_height {
            self.file_scroll = (selected_row + 1 - viewport_height) as u16;
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

    fn effective_diff_view_mode(&self) -> DiffViewMode {
        let change = self.files.get(self.file_selected).map(|e| e.change);
        match change {
            Some(FileChangeKind::Added | FileChangeKind::Deleted) => DiffViewMode::Unified,
            _ => self.diff_view_mode,
        }
    }

    fn select_next_unreviewed(&mut self, dir: i32) -> Result<()> {
        if self.files.is_empty() {
            self.status = "No files".to_string();
            return Ok(());
        }
        let len = self.files.len() as i32;
        let mut idx = self.file_selected as i32;
        loop {
            idx += dir;
            if idx < 0 || idx >= len {
                self.status = "No unreviewed files".to_string();
                return Ok(());
            }
            let path = &self.files[idx as usize].path;
            if !self.reviewed_files.contains(path) {
                self.file_selected = idx as usize;
                self.reload_diff_for_selected()?;
                return Ok(());
            }
        }
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

    fn clamp_file_scroll(&mut self) {
        if self.file_tree.rows.is_empty() {
            self.file_scroll = 0;
            return;
        }
        let viewport = self.files_viewport_height.max(1) as u32;
        let total = self.file_tree.rows.len() as u32;
        let max_scroll = total.saturating_sub(viewport).min(u16::MAX as u32);
        if (self.file_scroll as u32) > max_scroll {
            self.file_scroll = max_scroll as u16;
        }
    }

    fn toggle_reviewed_for_selected(&mut self) {
        let Some(path) = self.files.get(self.file_selected).map(|e| e.path.clone()) else {
            self.status = "No file selected".to_string();
            return;
        };
        let entry = self.review.files.entry(path.clone()).or_default();
        entry.reviewed = !entry.reviewed;
        if entry.reviewed {
            self.reviewed_files.insert(path.clone());
            self.status = "Marked reviewed".to_string();
        } else {
            self.reviewed_files.remove(&path);
            if entry.file_comment.is_none() && entry.comments.is_empty() {
                self.review.files.remove(&path);
            }
            self.status = "Marked unreviewed".to_string();
        }
        if let Err(e) = self.persist_file_note(&path) {
            self.status = format!("Failed to save reviewed state: {e}");
        }
    }

    fn ensure_prompt_visible(&mut self) {
        let viewport = self.prompt_viewport_height.max(1) as usize;
        let cur = self.prompt_buffer.cursor_row;
        let scroll = self.prompt_scroll as usize;
        if cur < scroll {
            self.prompt_scroll = cur as u16;
            return;
        }
        if cur >= scroll + viewport {
            self.prompt_scroll = (cur + 1 - viewport) as u16;
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
        self.begin_comment_for_target(target)
    }

    fn begin_file_comment(&mut self) -> Result<()> {
        let Some(path) = self.files.get(self.file_selected).map(|e| e.path.clone()) else {
            self.status = "No file selected".to_string();
            return Ok(());
        };
        self.begin_comment_for_target(CommentTarget {
            path,
            locator: CommentLocator::File,
        })
    }

    fn begin_comment_for_target(&mut self, target: CommentTarget) -> Result<()> {
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
            Some(file)
                if !file.comments.is_empty() || file.file_comment.is_some() || file.reviewed =>
            {
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

fn merge_file_review(target: &mut FileReview, incoming: FileReview) {
    if incoming.reviewed {
        target.reviewed = true;
    }
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

fn parse_diff_view_mode(raw: &str) -> Option<DiffViewMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "side-by-side" | "side_by_side" | "side" => Some(DiffViewMode::SideBySide),
        "unified" => Some(DiffViewMode::Unified),
        _ => None,
    }
}

fn parse_diff_context(raw: &str) -> Option<u32> {
    let parsed = raw.trim().parse::<u32>().ok()?;
    Some(parsed.clamp(MIN_DIFF_CONTEXT, MAX_DIFF_CONTEXT))
}

fn diff_view_mode_value(mode: DiffViewMode) -> &'static str {
    match mode {
        DiffViewMode::Unified => "unified",
        DiffViewMode::SideBySide => "side-by-side",
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

        if let Err(e) = crate::git::write_local_config_value(
            &self.repo,
            CONFIG_DIFF_VIEW_KEY,
            diff_view_mode_value(self.diff_view_mode),
        ) {
            self.status = format!("Failed to save diff view: {e}");
        }

        Ok(())
    }

    fn adjust_diff_context(&mut self, delta: i32) -> Result<()> {
        let cur = i32::try_from(self.diff_context).unwrap_or(i32::MAX);
        let min = i32::try_from(MIN_DIFF_CONTEXT).unwrap_or(i32::MIN);
        let max = i32::try_from(MAX_DIFF_CONTEXT).unwrap_or(i32::MAX);
        let next = (cur + delta).clamp(min, max);
        let next = u32::try_from(next).unwrap_or(self.diff_context);
        if next == self.diff_context {
            return Ok(());
        }
        let keep_line = self.keep_cursor_line();
        self.diff_context = next;
        self.reload_diff_for_selected()?;
        if let Some(k) = keep_line
            && let Some(idx) = self.find_row_for_keep_line(k)
        {
            self.diff_cursor = idx;
        }
        if let Err(e) = crate::git::write_local_config_value(
            &self.repo,
            CONFIG_DIFF_CONTEXT_KEY,
            &self.diff_context.to_string(),
        ) {
            self.status = format!("Failed to save diff context: {e}");
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
