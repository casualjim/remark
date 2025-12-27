use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use gix::ObjectId;

use crate::git::{CommitEntry, GitBackend};
use crate::highlight::Highlighter;
use crate::ui::{NoteBuffer, Ui};

const DEFAULT_NOTES_REF: &str = "refs/notes/review";
const DEFAULT_COMMIT_LIMIT: usize = 50;

#[derive(Debug, Clone)]
struct Config {
    notes_ref: String,
    commit_limit: usize,
}

impl Config {
    fn from_env() -> Self {
        let mut notes_ref = DEFAULT_NOTES_REF.to_string();
        let mut commit_limit = DEFAULT_COMMIT_LIMIT;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--ref" => {
                    if let Some(v) = args.next() {
                        notes_ref = v;
                    }
                }
                "--limit" => {
                    if let Some(v) = args.next() {
                        commit_limit = v.parse().unwrap_or(DEFAULT_COMMIT_LIMIT);
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
            commit_limit,
        }
    }
}

fn print_help_and_exit() -> ! {
    eprintln!(
        "git-review\n\nUSAGE:\n  git-review [--ref <notes-ref>] [--limit <n>]\n\nOPTIONS:\n  --ref <notes-ref>   Notes ref to store reviews (default: {DEFAULT_NOTES_REF})\n  --limit <n>         Number of commits to show (default: {DEFAULT_COMMIT_LIMIT})\n"
    );
    std::process::exit(2);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Browse,
    EditNote,
}

#[derive(Debug, Clone)]
struct NoteDraft {
    buffer: NoteBuffer,
    dirty: bool,
}

pub fn run() -> Result<()> {
    let config = Config::from_env();

    let repo = gix::discover(std::env::current_dir().context("get current directory")?)
        .context("discover git repository")?;
    let workdir = repo
        .workdir()
        .context("repository has no working directory")?
        .to_path_buf();

    let git = GitBackend::new(workdir, config.notes_ref.clone());
    let commits = crate::git::list_commits(&repo, config.commit_limit)?;
    if commits.is_empty() {
        bail!("No commits found");
    }

    let highlighter = Highlighter::new()?;

    let mut app = App {
        git,
        commits,
        selected: 0,
        mode: Mode::Browse,
        status: String::new(),
        show_help: false,
        diff_scroll: 0,
        drafts: HashMap::new(),
        diff_cache: HashMap::new(),
        highlighter,
    };

    app.ensure_loaded_current(120).ok();

    let mut ui = Ui::new()?;
    let res = app.run_loop(&mut ui);
    ui.restore().ok();
    res
}

struct DiffCacheEntry {
    rendered: ratatui::text::Text<'static>,
}

struct App {
    git: GitBackend,
    commits: Vec<CommitEntry>,
    selected: usize,
    mode: Mode,
    status: String,
    show_help: bool,
    diff_scroll: u16,
    drafts: HashMap<ObjectId, NoteDraft>,
    diff_cache: HashMap<ObjectId, DiffCacheEntry>,
    highlighter: Highlighter,
}

impl App {
    fn run_loop(&mut self, ui: &mut Ui) -> Result<()> {
        let tick_rate = Duration::from_millis(33);

        loop {
            let term_size = ui.terminal.size().context("read terminal size")?;
            let diff_width = crate::ui::estimated_diff_width(term_size.width);
            ui.terminal
                .draw(|f| {
                    crate::ui::draw(
                        f,
                        &self.commits,
                        self.selected,
                        self.current_draft(),
                        self.current_diff(),
                        self.diff_scroll,
                        self.mode,
                        &self.status,
                        self.show_help,
                        &self.git.notes_ref,
                    )
                })
                .context("draw ui")?;

            if crossterm::event::poll(tick_rate).context("poll terminal events")? {
                match crossterm::event::read().context("read terminal event")? {
                    Event::Key(key) => {
                        if self.handle_key(key, diff_width)? {
                            break;
                        }
                    }
                    Event::Resize(_, _) => {
                        self.status = "Resized (press r to refresh diff width)".to_string();
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent, diff_width: u16) -> Result<bool> {
        if key.code == KeyCode::Char('q') && self.mode == Mode::Browse {
            if self.current_draft().1 {
                self.status = "Unsaved note (Ctrl+S to save, Q to quit anyway)".to_string();
                return Ok(false);
            }
            return Ok(true);
        }

        if key.code == KeyCode::Char('?') && key.modifiers.is_empty() {
            self.show_help = !self.show_help;
            return Ok(false);
        }

        if key.code == KeyCode::Char('r') && key.modifiers.is_empty() {
            self.reload_current(diff_width)?;
            return Ok(false);
        }

        match self.mode {
            Mode::Browse => self.handle_browse_key(key, diff_width),
            Mode::EditNote => self.handle_edit_key(key),
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent, diff_width: u16) -> Result<bool> {
        match key.code {
            KeyCode::Up => self.select_prev(diff_width)?,
            KeyCode::Down => self.select_next(diff_width)?,
            KeyCode::PageUp => self.scroll_diff(-20),
            KeyCode::PageDown => self.scroll_diff(20),
            KeyCode::Char('k') => self.scroll_diff(-1),
            KeyCode::Char('j') => self.scroll_diff(1),
            KeyCode::Char('e') => {
                self.mode = Mode::EditNote;
                self.status = "Edit mode (Ctrl+S to save, Esc to leave)".to_string();
            }
            KeyCode::Char('Q') => return Ok(true),
            _ => {}
        }
        Ok(false)
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> Result<bool> {
        let oid = self.current_oid().to_owned();
        let draft = self.drafts.entry(oid).or_insert_with(|| NoteDraft {
            buffer: NoteBuffer::new(),
            dirty: false,
        });

        if key.code == KeyCode::Esc {
            self.mode = Mode::Browse;
            self.status = "Browse mode".to_string();
            return Ok(false);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            let text = draft.buffer.as_string();
            self.git
                .write_note(&oid, &text)
                .with_context(|| format!("write note for {oid}"))?;
            draft.dirty = false;
            self.status = "Saved note".to_string();
            return Ok(false);
        }

        match key.code {
            KeyCode::Up => draft.buffer.move_up(),
            KeyCode::Down => draft.buffer.move_down(),
            KeyCode::Left => draft.buffer.move_left(),
            KeyCode::Right => draft.buffer.move_right(),
            KeyCode::Home => draft.buffer.move_line_start(),
            KeyCode::End => draft.buffer.move_line_end(),
            KeyCode::Backspace => {
                if draft.buffer.backspace() {
                    draft.dirty = true;
                }
            }
            KeyCode::Enter => {
                draft.buffer.insert_newline();
                draft.dirty = true;
            }
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    draft.buffer.insert_char(c);
                    draft.dirty = true;
                }
            }
            _ => {}
        }

        Ok(false)
    }

    fn scroll_diff(&mut self, delta: i32) {
        let new = (self.diff_scroll as i32 + delta).max(0) as u16;
        self.diff_scroll = new;
    }

    fn select_prev(&mut self, diff_width: u16) -> Result<()> {
        if self.selected == 0 {
            return Ok(());
        }
        self.selected -= 1;
        self.diff_scroll = 0;
        self.ensure_loaded_current(diff_width).ok();
        Ok(())
    }

    fn select_next(&mut self, diff_width: u16) -> Result<()> {
        if self.selected + 1 >= self.commits.len() {
            return Ok(());
        }
        self.selected += 1;
        self.diff_scroll = 0;
        self.ensure_loaded_current(diff_width).ok();
        Ok(())
    }

    fn reload_current(&mut self, diff_width: u16) -> Result<()> {
        let oid = self.current_oid().to_owned();

        self.diff_cache.remove(&oid);
        self.drafts.remove(&oid);
        self.ensure_loaded_current(diff_width)?;
        self.status = "Reloaded".to_string();
        Ok(())
    }

    fn ensure_loaded_current(&mut self, diff_width: u16) -> Result<()> {
        let oid = self.current_oid().to_owned();

        if !self.drafts.contains_key(&oid) {
            let note = self.git.read_note(&oid).with_context(|| format!("read note for {oid}"))?;
            let buffer = NoteBuffer::from_string(note.unwrap_or_default());
            self.drafts.insert(
                oid,
                NoteDraft {
                    buffer,
                    dirty: false,
                },
            );
        }

        if !self.diff_cache.contains_key(&oid) {
            let raw = self
                .git
                .diff_commit(&oid, diff_width.saturating_sub(2))
                .with_context(|| format!("diff commit {oid}"))?;
            let rendered = self
                .highlighter
                .highlight_diff(&raw)
                .unwrap_or_else(|_| ratatui::text::Text::raw(raw.clone()));
            self.diff_cache.insert(oid, DiffCacheEntry { rendered });
        }

        Ok(())
    }

    fn current_oid(&self) -> &ObjectId {
        &self.commits[self.selected].id
    }

    fn current_draft(&self) -> (&NoteBuffer, bool) {
        let oid = self.current_oid();
        let d = self.drafts.get(oid).expect("note draft loaded");
        (&d.buffer, d.dirty)
    }

    fn current_diff(&self) -> Option<&ratatui::text::Text<'static>> {
        self.diff_cache.get(self.current_oid()).map(|d| &d.rendered)
    }
}
