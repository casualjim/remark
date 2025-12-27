use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use gix::ObjectId;

use crate::git::ViewKind;
use crate::review::Review;

const DEFAULT_NOTES_REF: &str = "refs/notes/review";

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

        Self { notes_ref, base_ref }
    }
}

fn print_help_and_exit() -> ! {
    eprintln!(
        "git-review\n\nUSAGE:\n  git-review [--ref <notes-ref>] [--base <ref>]\n\nOPTIONS:\n  --ref <notes-ref>   Notes ref to store reviews (default: {DEFAULT_NOTES_REF})\n  --base <ref>        Base ref for base view (default: @{{upstream}} / main / master)\n\nKEYS:\n  2/3/4               unstaged/staged/base\n  Tab                 change focus\n  Up/Down             navigate\n  c                   add/edit comment at line\n  d                   delete comment at line\n  Ctrl+S              save review note\n  p                   preview collated prompt\n  ?                   help\n  q                   quit\n"
    );
    std::process::exit(2);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Files,
    Lines,
    Comment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Browse,
    EditComment,
}

#[derive(Debug, Clone)]
struct FileViewState {
    path: String,
    lines: Vec<String>,
    cursor_line: usize,
    scroll: u16,
}

impl FileViewState {
    fn empty() -> Self {
        Self {
            path: String::new(),
            lines: vec!["(no file selected)".to_string()],
            cursor_line: 0,
            scroll: 0,
        }
    }

    fn set_content(&mut self, path: String, content: String) {
        self.path = path;
        self.lines = content
            .lines()
            .map(|l| l.to_string())
            .collect::<Vec<_>>();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_line = 0;
        self.scroll = 0;
    }

    fn current_line_1_based(&self) -> u32 {
        (self.cursor_line + 1) as u32
    }

    fn move_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let new = (self.scroll as i32 + delta).max(0) as u16;
        self.scroll = new;
    }

    fn ensure_cursor_visible(&mut self, viewport_height: u16) {
        let viewport_height = viewport_height.max(1) as usize;
        let scroll = self.scroll as usize;
        if self.cursor_line < scroll {
            self.scroll = self.cursor_line as u16;
            return;
        }
        if self.cursor_line >= scroll + viewport_height {
            self.scroll = (self.cursor_line + 1 - viewport_height) as u16;
        }
    }
}

pub fn run() -> Result<()> {
    let repo = gix::discover(std::env::current_dir().context("get current directory")?)
        .context("discover git repository")?;
    let config = Config::from_env(&repo);

    let mut app = App::new(repo, config.notes_ref, config.base_ref)?;
    let mut ui = crate::ui::Ui::new()?;
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

    review_oid: Option<ObjectId>,
    review: Review,
    review_dirty: bool,

    files: Vec<String>,
    selected_file: usize,
    file_view: FileViewState,

    comment_buffer: crate::ui::NoteBuffer,
    comment_dirty: bool,

    status: String,
    show_help: bool,
    show_prompt: bool,
}

impl App {
    fn new(repo: gix::Repository, notes_ref: String, base_ref: Option<String>) -> Result<Self> {
        let mut app = Self {
            repo,
            notes_ref,
            base_ref,
            view: ViewKind::Unstaged,
            focus: Focus::Files,
            mode: Mode::Browse,
            review_oid: None,
            review: Review::new("unstaged", None),
            review_dirty: false,
            files: Vec::new(),
            selected_file: 0,
            file_view: FileViewState::empty(),
            comment_buffer: crate::ui::NoteBuffer::new(),
            comment_dirty: false,
            status: String::new(),
            show_help: false,
            show_prompt: false,
        };

        app.reload_view()?;
        Ok(app)
    }

    fn run_loop(&mut self, ui: &mut crate::ui::Ui) -> Result<()> {
        let tick_rate = Duration::from_millis(33);

        loop {
            let size = ui.terminal.size().context("read terminal size")?;
            let file_view_height = size.height.saturating_sub(1).saturating_sub(12).max(1);
            self.file_view.ensure_cursor_visible(file_view_height);

            let current_path = self.files.get(self.selected_file).cloned();
            let current_line = self.file_view.current_line_1_based();
            let commented_lines = current_path
                .as_deref()
                .and_then(|p| self.review.files.get(p))
                .map(|f| f.comments.keys().copied().collect::<Vec<_>>())
                .unwrap_or_default();

            ui.terminal
                .draw(|f| {
                    crate::ui::draw(
                        f,
                        crate::ui::DrawState {
                            view: self.view,
                            base_ref: self.base_ref.as_deref(),
                            focus: self.focus,
                            mode: self.mode,
                            notes_ref: &self.notes_ref,
                            review_dirty: self.review_dirty,
                            files: &self.files,
                            selected_file: self.selected_file,
                            file_path: &self.file_view.path,
                            file_lines: &self.file_view.lines,
                            file_cursor_line: self.file_view.cursor_line,
                            file_scroll: self.file_view.scroll,
                            commented_lines: &commented_lines,
                            current_comment: current_path
                                .as_deref()
                                .and_then(|p| self.review.comment(p, current_line))
                                .unwrap_or(""),
                            comment_buffer: &self.comment_buffer,
                            comment_dirty: self.comment_dirty,
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
                    _ => {}
                }
            }
        }

        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.code == KeyCode::Char('q') && self.mode == Mode::Browse {
            if self.review_dirty {
                self.status = "Unsaved review (Ctrl+S to save)".to_string();
                return Ok(false);
            }
            return Ok(true);
        }

        if key.code == KeyCode::Char('?') && key.modifiers.is_empty() {
            self.show_help = !self.show_help;
            return Ok(false);
        }

        if key.code == KeyCode::Char('p') && key.modifiers.is_empty() {
            self.show_prompt = !self.show_prompt;
            return Ok(false);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.save_review()?;
            return Ok(false);
        }

        match self.mode {
            Mode::Browse => self.handle_browse_key(key),
            Mode::EditComment => self.handle_comment_key(key),
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Char('2') => {
                self.view = ViewKind::Unstaged;
                self.reload_view()?;
            }
            KeyCode::Char('3') => {
                self.view = ViewKind::Staged;
                self.reload_view()?;
            }
            KeyCode::Char('4') => {
                self.view = ViewKind::Base;
                self.reload_view()?;
            }
            KeyCode::Tab => self.focus = next_focus(self.focus),
            _ => match self.focus {
                Focus::Files => self.handle_files_focus(key)?,
                Focus::Lines => self.handle_lines_focus(key)?,
                Focus::Comment => {}
            },
        }
        Ok(false)
    }

    fn handle_files_focus(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Up => self.select_prev_file()?,
            KeyCode::Down => self.select_next_file()?,
            KeyCode::Enter => {
                if !self.files.is_empty() {
                    self.focus = Focus::Lines;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_lines_focus(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Up => self.file_view.move_up(),
            KeyCode::Down => self.file_view.move_down(),
            KeyCode::PageUp => self.file_view.scroll_by(-20),
            KeyCode::PageDown => self.file_view.scroll_by(20),
            KeyCode::Char('c') => self.begin_edit_comment()?,
            KeyCode::Char('d') => self.delete_comment()?,
            KeyCode::Esc => self.focus = Focus::Files,
            _ => {}
        }
        Ok(())
    }

    fn handle_comment_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.code == KeyCode::Esc {
            self.finish_edit_comment();
            self.mode = Mode::Browse;
            self.focus = Focus::Lines;
            self.status = "Comment updated".to_string();
            return Ok(false);
        }

        match key.code {
            KeyCode::Up => self.comment_buffer.move_up(),
            KeyCode::Down => self.comment_buffer.move_down(),
            KeyCode::Left => self.comment_buffer.move_left(),
            KeyCode::Right => self.comment_buffer.move_right(),
            KeyCode::Home => self.comment_buffer.move_line_start(),
            KeyCode::End => self.comment_buffer.move_line_end(),
            KeyCode::Backspace => {
                if self.comment_buffer.backspace() {
                    self.comment_dirty = true;
                }
            }
            KeyCode::Enter => {
                self.comment_buffer.insert_newline();
                self.comment_dirty = true;
            }
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    self.comment_buffer.insert_char(c);
                    self.comment_dirty = true;
                }
            }
            _ => {}
        }

        Ok(false)
    }

    fn reload_view(&mut self) -> Result<()> {
        self.focus = Focus::Files;
        self.mode = Mode::Browse;
        self.show_prompt = false;
        self.show_help = false;
        self.selected_file = 0;
        self.file_view = FileViewState::empty();
        self.comment_buffer = crate::ui::NoteBuffer::new();
        self.comment_dirty = false;

        let (review_oid, review) = self.load_review_for_view()?;
        self.review_oid = review_oid;
        self.review = review;
        self.review_dirty = false;

        self.files = self.list_files_for_view()?;
        if self.files.is_empty() {
            self.status = "No changes".to_string();
        } else {
            self.status.clear();
            self.load_selected_file()?;
        }

        Ok(())
    }

    fn load_review_for_view(&mut self) -> Result<(Option<ObjectId>, Review)> {
        let (oid, kind, base_ref) = match self.view {
            ViewKind::Unstaged => {
                let oid = crate::git::note_key_oid(&self.repo, ViewKind::Unstaged, None)?;
                (Some(oid), "unstaged".to_string(), None)
            }
            ViewKind::Staged => {
                let oid = crate::git::note_key_oid(&self.repo, ViewKind::Staged, None)?;
                (Some(oid), "staged".to_string(), None)
            }
            ViewKind::Base => {
                let base = self
                    .base_ref
                    .clone()
                    .or_else(|| crate::git::default_base_ref(&self.repo));
                self.base_ref = base.clone();
                let Some(base) = base else {
                    self.status = "No base ref set (pass --base <ref>)".to_string();
                    return Ok((None, Review::new("base", None)));
                };
                let oid = crate::git::note_key_oid(&self.repo, ViewKind::Base, Some(&base))?;
                (Some(oid), "base".to_string(), Some(base))
            }
        };

        let Some(oid) = oid else {
            return Ok((None, Review::new(kind, base_ref)));
        };

        let note = crate::notes::read(&self.repo, &self.notes_ref, &oid).context("read review note")?;
        let review = note
            .as_deref()
            .and_then(crate::review::decode_note)
            .unwrap_or_else(|| Review::new(kind, base_ref));
        Ok((Some(oid), review))
    }

    fn list_files_for_view(&self) -> Result<Vec<String>> {
        match self.view {
            ViewKind::Unstaged => crate::git::list_unstaged_paths(&self.repo),
            ViewKind::Staged => crate::git::list_staged_paths(&self.repo),
            ViewKind::Base => {
                let Some(base) = &self.base_ref else { return Ok(Vec::new()) };
                crate::git::list_base_paths(&self.repo, base)
            }
        }
    }

    fn load_selected_file(&mut self) -> Result<()> {
        let Some(path) = self.files.get(self.selected_file).cloned() else {
            self.file_view = FileViewState::empty();
            return Ok(());
        };

        let src = crate::git::view_file_source(self.view);
        let content = crate::git::read_file(&self.repo, src, &path)
            .unwrap_or_else(|e| format!("(unable to read {path}: {e})\n"));
        self.file_view.set_content(path, content);
        Ok(())
    }

    fn select_prev_file(&mut self) -> Result<()> {
        if self.files.is_empty() || self.selected_file == 0 {
            return Ok(());
        }
        self.selected_file -= 1;
        self.load_selected_file()?;
        Ok(())
    }

    fn select_next_file(&mut self) -> Result<()> {
        if self.files.is_empty() || self.selected_file + 1 >= self.files.len() {
            return Ok(());
        }
        self.selected_file += 1;
        self.load_selected_file()?;
        Ok(())
    }

    fn begin_edit_comment(&mut self) -> Result<()> {
        let Some(path) = self.files.get(self.selected_file).cloned() else {
            self.status = "No file selected".to_string();
            return Ok(());
        };
        let line = self.file_view.current_line_1_based();
        let existing = self.review.comment(&path, line).unwrap_or("");
        self.comment_buffer = crate::ui::NoteBuffer::from_string(existing.to_string());
        self.comment_dirty = false;
        self.focus = Focus::Comment;
        self.mode = Mode::EditComment;
        self.status = format!("Editing comment at {path}:{line} (Esc to apply)");
        Ok(())
    }

    fn finish_edit_comment(&mut self) {
        let Some(path) = self.files.get(self.selected_file).cloned() else {
            return;
        };
        let line = self.file_view.current_line_1_based();
        let comment = self.comment_buffer.as_string();
        let before = self.review.comment(&path, line).unwrap_or("").to_string();
        if comment.trim_end() == before.trim_end() {
            return;
        }
        self.review.set_comment(&path, line, comment);
        self.review_dirty = true;
    }

    fn delete_comment(&mut self) -> Result<()> {
        let Some(path) = self.files.get(self.selected_file).cloned() else {
            return Ok(());
        };
        let line = self.file_view.current_line_1_based();
        if self.review.remove_comment(&path, line) {
            self.review_dirty = true;
            self.status = format!("Deleted comment at {path}:{line}");
        } else {
            self.status = "No comment on this line".to_string();
        }
        Ok(())
    }

    fn save_review(&mut self) -> Result<()> {
        let Some(oid) = self.review_oid else {
            self.status = "No review target (missing base ref?)".to_string();
            return Ok(());
        };
        let note = crate::review::encode_note(&self.review);
        crate::notes::write(&self.repo, &self.notes_ref, &oid, Some(&note)).context("write review note")?;
        self.review_dirty = false;
        self.status = "Saved review note".to_string();
        Ok(())
    }
}

fn next_focus(f: Focus) -> Focus {
    match f {
        Focus::Files => Focus::Lines,
        Focus::Lines => Focus::Files,
        Focus::Comment => Focus::Files,
    }
}
