use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
  Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use gix::ObjectId;
use gix_hash::{Kind, hasher};
use tui_textarea::TextArea;

use crate::file_tree::FileTreeView;
use crate::git::ViewKind;
use crate::highlight::Highlighter;
use crate::review::{FileReview, LineKey, LineSide, Review};
use unicode_width::UnicodeWidthStr;

const CONFIG_DIFF_CONTEXT_KEY: &str = "remark.diffContext";
const DEFAULT_DIFF_CONTEXT: u32 = 3;
const MIN_DIFF_CONTEXT: u32 = 0;
const MAX_DIFF_CONTEXT: u32 = 20;

#[derive(Debug, Clone)]
pub(crate) struct JumpTarget {
  path: String,
  line: Option<LineKey>,
}

#[derive(Debug, Clone)]
pub(crate) struct UiOptions {
  pub(crate) notes_ref: String,
  pub(crate) base_ref: Option<String>,
  pub(crate) show_ignored: bool,
  pub(crate) view: ViewKind,
  pub(crate) jump_target: Option<JumpTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
  Browse,
  EditComment,
  EditPrompt,
  CommentList,
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
pub(crate) struct CommentListEntry {
  pub(crate) path: String,
  pub(crate) locator: CommentLocator,
  pub(crate) body: String,
  pub(crate) resolved: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DiffRow {
  pub(crate) kind: crate::diff::Kind,
  pub(crate) old_line: Option<u32>,
  pub(crate) new_line: Option<u32>,
  pub(crate) spans: Vec<ratatui::text::Span<'static>>,
}

#[derive(Debug, Clone)]
pub(crate) struct DecoratedRow {
  pub(crate) status: crate::diff::LineStatus,
  pub(crate) line_number: u32,
  pub(crate) old_line_number: Option<u32>,
  pub(crate) spans: Vec<ratatui::text::Span<'static>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffViewMode {
  Decorated,
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
  Decorated(DecoratedRow),
}

#[derive(Debug, Clone, Copy)]
enum KeepLine {
  Old(u32),
  New(u32),
}

pub fn run(repo: gix::Repository, options: UiOptions) -> Result<()> {
  let mut ui = crate::ui::Ui::new()?;

  let mut app = App::new(
    repo,
    options.notes_ref,
    options.base_ref,
    options.show_ignored,
    options.view,
    options.jump_target,
  )?;
  let res = app.run_loop(&mut ui);

  ui.restore().ok();
  res
}

pub(crate) fn build_jump_target(
  repo: &gix::Repository,
  jump_path: Option<String>,
  jump_line: Option<u32>,
  jump_side: Option<LineSide>,
) -> Result<Option<JumpTarget>> {
  if (jump_line.is_some() || jump_side.is_some()) && jump_path.is_none() {
    anyhow::bail!("--line/--side requires --file <path>");
  }
  if jump_line.is_none() && jump_side.is_some() {
    anyhow::bail!("--side requires --line <n>");
  }

  let mut jump_target = jump_path.map(|mut path| {
    if let Some(stripped) = path.strip_prefix("./") {
      path = stripped.to_string();
    }
    if let Some(wd) = repo.workdir()
      && std::path::Path::new(&path).is_absolute()
      && let Ok(rel) = std::path::Path::new(&path).strip_prefix(wd)
    {
      path = rel.to_string_lossy().to_string();
    }

    let line = jump_line.map(|line| LineKey {
      side: jump_side.unwrap_or(LineSide::New),
      line,
    });

    JumpTarget { path, line }
  });

  if let Some(target) = jump_target.as_mut()
    && target.path.is_empty()
  {
    anyhow::bail!("--file <path> must not be empty");
  }

  Ok(jump_target)
}

struct App {
  repo: gix::Repository,
  notes_ref: String,
  base_ref: Option<String>,
  show_ignored: bool,
  jump_target: Option<JumpTarget>,

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
  editor_buffer: TextArea<'static>,
  prompt_buffer: TextArea<'static>,

  status: String,
  show_help: bool,
  show_prompt: bool,
  show_diff_popup: bool,
  comment_list: Vec<CommentListEntry>,
  comment_list_selected: usize,
  comment_list_marked: HashSet<usize>,

  // Store diff data for popup display
  current_before: Option<String>,
  current_after: Option<String>,
  current_diff_lines: Vec<crate::diff::Line>,
  current_file_path: Option<String>,

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
  fn build_notes_review(&self, head: ObjectId) -> Result<Review> {
    let mut review = Review::new();
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
        let oid = crate::git::note_file_key_oid(&self.repo, head, view, base_for_key, &e.path)?;
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
        review.files.insert(e.path.clone(), fr);
      }
    }
    Ok(review)
  }

  fn merge_draft_into_notes(&self, mut notes: Review, draft: Review) -> Review {
    for file in notes.files.values_mut() {
      file.file_comment = None;
      file.comments.clear();
    }

    for (path, draft_file) in draft.files {
      let entry = notes.files.entry(path).or_default();
      entry.file_comment = draft_file.file_comment;
      entry.comments = draft_file.comments;
    }

    notes
  }

  fn refresh_review_from_sources(&mut self) -> Result<()> {
    if self.head_commit_oid.is_some() {
      crate::add_cmd::sync_draft_notes(&self.repo, &self.notes_ref, self.base_ref.as_deref())?;
    }
    let draft_review = crate::add_cmd::load_review_from_draft(
      &self.repo,
      &self.notes_ref,
      self.base_ref.as_deref(),
    )?;
    let notes_review = if let Some(head) = self.head_commit_oid {
      self.build_notes_review(head)?
    } else {
      self.status = "No HEAD commit (unborn branch) — notes disabled".to_string();
      Review::new()
    };
    self.review = self.merge_draft_into_notes(notes_review, draft_review);
    Ok(())
  }

  fn read_prompt_from_draft(&mut self) -> String {
    if self.head_commit_oid.is_some() {
      if let Err(e) =
        crate::add_cmd::sync_draft_notes(&self.repo, &self.notes_ref, self.base_ref.as_deref())
      {
        self.status = format!("Failed to sync draft: {e}");
      }
    } else if let Err(e) =
      crate::add_cmd::ensure_draft_exists(&self.repo, &self.notes_ref, self.base_ref.as_deref())
    {
      self.status = format!("Failed to create draft: {e}");
    }

    let path = match crate::add_cmd::draft_path(&self.repo) {
      Ok(p) => p,
      Err(e) => {
        self.status = format!("Failed to find draft: {e}");
        return "No comments.\n".to_string();
      }
    };
    match std::fs::read_to_string(&path) {
      Ok(content) => {
        if content.trim().is_empty() {
          "No comments.\n".to_string()
        } else {
          content
        }
      }
      Err(e) => {
        self.status = format!("Failed to read draft: {e}");
        "No comments.\n".to_string()
      }
    }
  }

  fn refresh_prompt_buffer_from_draft(&mut self) {
    let prompt = self.read_prompt_from_draft();
    self.prompt_buffer = crate::ui::textarea_from_string(&prompt);
  }

  fn new(
    repo: gix::Repository,
    notes_ref: String,
    base_ref: Option<String>,
    show_ignored: bool,
    view: ViewKind,
    jump_target: Option<JumpTarget>,
  ) -> Result<Self> {
    let highlighter = Highlighter::new()?;
    // Always default to Decorated view; users can switch with 'd' key
    let diff_view_mode = DiffViewMode::Decorated;
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
      jump_target,
      view,
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
      editor_buffer: crate::ui::empty_textarea(),
      prompt_buffer: crate::ui::empty_textarea(),
      status: String::new(),
      show_help: false,
      show_prompt: false,
      show_diff_popup: false,
      comment_list: Vec::new(),
      comment_list_selected: 0,
      comment_list_marked: HashSet::new(),
      current_before: None,
      current_after: None,
      current_diff_lines: Vec::new(),
      current_file_path: None,
      highlighter,
    };
    app.reload_view()?;
    app.apply_jump_target()?;
    Ok(app)
  }

  fn apply_jump_target(&mut self) -> Result<()> {
    let Some(target) = self.jump_target.take() else {
      return Ok(());
    };

    let Some(idx) = self.files.iter().position(|e| e.path == target.path) else {
      self.status = format!("File not found in view: {}", target.path);
      return Ok(());
    };

    self.file_selected = idx;
    self.reload_diff_for_selected()?;

    if let Some(line_key) = target.line {
      let keep = match line_key.side {
        LineSide::Old => KeepLine::Old(line_key.line),
        LineSide::New => KeepLine::New(line_key.line),
      };
      if let Some(row) = self.find_row_for_keep_line(keep) {
        self.diff_cursor = row;
      } else {
        self.status = format!("Line {} not found in diff", line_key.line);
      }
      self.focus = Focus::Diff;
    }

    Ok(())
  }

  fn run_loop(&mut self, ui: &mut crate::ui::Ui) -> Result<()> {
    let tick_rate = Duration::from_millis(33);

    loop {
      let size = ui.terminal.size().context("read terminal size")?;
      let outer = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: size.width,
        height: size.height,
      };
      let layout = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
          ratatui::layout::Constraint::Min(1),
          ratatui::layout::Constraint::Length(1),
        ])
        .split(outer);
      let main = layout[0];

      let rects = crate::ui::layout(main);
      self.files_viewport_height = rects.files.height.saturating_sub(2).max(1);
      self.diff_viewport_height = rects.diff.height.saturating_sub(2).max(1);
      self.diff_viewport_width = rects.diff.width.saturating_sub(2).max(1);
      self.recompute_diff_metrics(self.diff_viewport_width);
      if !self.manual_scroll {
        self.ensure_file_visible(self.files_viewport_height);
        self.ensure_diff_visible(self.diff_viewport_height);
      }
      self.clamp_file_scroll();
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
              status: &self.status,
              show_help: self.show_help,
              show_prompt: self.show_prompt,
              show_diff_popup: self.show_diff_popup,
              comment_list: &self.comment_list,
              comment_list_selected: self.comment_list_selected,
              comment_list_marked: &self.comment_list_marked,
              current_diff_lines: &self.current_diff_lines,
              diff_cursor_line: self.get_current_line_number(),
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
      Mode::CommentList => self.handle_comment_list_key(key),
    }
  }

  fn handle_browse_key(&mut self, key: KeyEvent) -> Result<bool> {
    // Keyboard interaction implies "cursor-following" again.
    self.manual_scroll = false;

    let no_ctrl_alt =
      !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT);
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
      if self.show_diff_popup {
        self.show_diff_popup = false;
        self.status.clear();
        return Ok(false);
      }
      return Ok(true);
    }

    let is_shift_c = matches!(key.code, KeyCode::Char('C'))
      || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::SHIFT));
    if is_shift_c && no_ctrl_alt {
      return self.open_comment_list();
    }

    if key.code == KeyCode::Char('p') && key.modifiers.is_empty() {
      if self.show_prompt {
        self.show_prompt = false;
        self.mode = Mode::Browse;
      } else {
        self.show_prompt = true;
        self.refresh_prompt_buffer_from_draft();
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
      KeyCode::Char('n') if key.modifiers.is_empty() => {
        self.jump_next_hunk();
      }
      KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
        self.select_next_unreviewed(-1)?;
      }
      KeyCode::Char('c') if key.modifiers.is_empty() => self.begin_comment()?,
      KeyCode::Char('d') if key.modifiers.is_empty() => self.delete_comment()?,
      KeyCode::Char('H') => self.toggle_diff_popup()?,
      KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::SHIFT) => {
        self.toggle_diff_popup()?
      }
      KeyCode::Char('r') if key.modifiers.is_empty() => self.toggle_resolved()?,
      KeyCode::Char('[') if key.modifiers.is_empty() => self.adjust_diff_context(-1)?,
      KeyCode::Char(']') if key.modifiers.is_empty() => self.adjust_diff_context(1)?,
      _ => {}
    }
    Ok(())
  }

  fn get_current_line_number(&self) -> u32 {
    self
      .diff_rows
      .get(self.diff_cursor)
      .and_then(|r| match r {
        RenderRow::Decorated(dr) => Some(dr.line_number),
        _ => None,
      })
      .unwrap_or(0)
  }

  fn toggle_diff_popup(&mut self) -> Result<()> {
    self.show_diff_popup = !self.show_diff_popup;
    if self.show_diff_popup {
      self.status = format!(
        "Diff popup open ({} lines), press ESC or 'H' to close",
        self.current_diff_lines.len()
      );
    } else {
      self.status.clear();
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
        self
          .review
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
      self.editor_buffer = crate::ui::empty_textarea();
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

    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::SHIFT) {
      self.accept_comment_and_move_on()?;
      return Ok(false);
    }

    self.editor_buffer.input(key);
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
      let prompt = self.read_prompt_from_draft();
      match crate::clipboard::copy(&prompt) {
        Ok(method) => self.status = format!("Copied prompt to clipboard ({method})"),
        Err(e) => self.status = format!("Clipboard failed: {e}"),
      }
      self.mode = Mode::Browse;
      self.show_prompt = false;
      return Ok(false);
    }

    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::SHIFT) {
      let prompt = self.read_prompt_from_draft();
      match crate::clipboard::copy(&prompt) {
        Ok(method) => self.status = format!("Copied prompt to clipboard ({method})"),
        Err(e) => self.status = format!("Clipboard failed: {e}"),
      }
      self.mode = Mode::Browse;
      self.show_prompt = false;
      return Ok(false);
    }

    self.prompt_buffer.input(key);
    Ok(false)
  }

  fn handle_comment_list_key(&mut self, key: KeyEvent) -> Result<bool> {
    if key.code == KeyCode::Esc {
      self.close_comment_list();
      return Ok(false);
    }

    if self.comment_list.is_empty() {
      return Ok(false);
    }

    let no_ctrl_alt =
      !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT);

    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::SHIFT) {
      self.jump_to_comment_list_entry()?;
      return Ok(false);
    }

    let is_shift_r = matches!(key.code, KeyCode::Char('R'))
      || (key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::SHIFT));
    if is_shift_r && no_ctrl_alt {
      self.resolve_comment_list_selection()?;
      return Ok(false);
    }

    if key.code == KeyCode::Delete && no_ctrl_alt {
      self.discard_comment_list_selection()?;
      return Ok(false);
    }

    match key.code {
      KeyCode::Up | KeyCode::Char('k') => self.move_comment_list_selection(-1),
      KeyCode::Down | KeyCode::Char('j') => self.move_comment_list_selection(1),
      KeyCode::PageUp => self.move_comment_list_selection(-10),
      KeyCode::PageDown => self.move_comment_list_selection(10),
      KeyCode::Home => self.comment_list_selected = 0,
      KeyCode::End => self.comment_list_selected = self.comment_list.len().saturating_sub(1),
      KeyCode::Enter if no_ctrl_alt => {
        self.toggle_comment_list_mark();
      }
      _ => {}
    }

    Ok(false)
  }

  fn open_comment_list(&mut self) -> Result<bool> {
    self.comment_list = self.build_comment_list();
    if self.comment_list.is_empty() {
      self.status = "No comments".to_string();
      return Ok(false);
    }
    self.comment_list_selected = 0;
    self.comment_list_marked.clear();
    self.show_help = false;
    self.show_prompt = false;
    self.mode = Mode::CommentList;
    Ok(false)
  }

  fn close_comment_list(&mut self) {
    self.mode = Mode::Browse;
    self.comment_list_marked.clear();
  }

  fn move_comment_list_selection(&mut self, delta: i32) {
    if self.comment_list.is_empty() {
      self.comment_list_selected = 0;
      return;
    }
    let max = (self.comment_list.len() - 1) as i32;
    let cur = self.comment_list_selected as i32;
    self.comment_list_selected = (cur + delta).clamp(0, max) as usize;
  }

  fn toggle_comment_list_mark(&mut self) {
    if self.comment_list.is_empty() {
      return;
    }
    let idx = self.comment_list_selected;
    if !self.comment_list_marked.insert(idx) {
      self.comment_list_marked.remove(&idx);
    }
  }

  fn comment_list_selected_paths(&self) -> Vec<String> {
    if self.comment_list.is_empty() {
      return Vec::new();
    }
    let indices: Vec<usize> = if self.comment_list_marked.is_empty() {
      vec![self.comment_list_selected]
    } else {
      self.comment_list_marked.iter().copied().collect()
    };
    let mut paths = std::collections::BTreeSet::new();
    for idx in indices {
      if let Some(entry) = self.comment_list.get(idx) {
        paths.insert(entry.path.clone());
      }
    }
    paths.into_iter().collect()
  }

  fn resolve_comment_list_selection(&mut self) -> Result<()> {
    let paths = self.comment_list_selected_paths();
    if paths.is_empty() {
      return Ok(());
    }
    for path in &paths {
      if let Some(file) = self.review.files.get_mut(path) {
        if let Some(c) = file.file_comment.as_mut() {
          c.resolved = true;
        }
        for c in file.comments.values_mut() {
          c.resolved = true;
        }
        if file.file_comment.is_none() && file.comments.is_empty() && !file.reviewed {
          self.review.files.remove(path);
        }
      }
      self.persist_file_note(path)?;
    }

    self.comment_list = self.build_comment_list();
    self.comment_list_selected = self
      .comment_list_selected
      .min(self.comment_list.len().saturating_sub(1));
    self.comment_list_marked.clear();
    self.status = format!("Resolved comments ({})", paths.len());
    Ok(())
  }

  fn discard_comment_list_selection(&mut self) -> Result<()> {
    let paths = self.comment_list_selected_paths();
    if paths.is_empty() {
      return Ok(());
    }
    for path in &paths {
      if let Some(file) = self.review.files.get_mut(path) {
        file.file_comment = None;
        file.comments.clear();
        if !file.reviewed {
          self.review.files.remove(path);
        }
      }
      self.persist_file_note(path)?;
    }

    self.comment_list = self.build_comment_list();
    self.comment_list_selected = self
      .comment_list_selected
      .min(self.comment_list.len().saturating_sub(1));
    self.comment_list_marked.clear();
    self.status = format!("Discarded comments ({})", paths.len());
    Ok(())
  }

  fn jump_to_comment_list_entry(&mut self) -> Result<()> {
    if self.comment_list.is_empty() {
      return Ok(());
    }
    let entry = self.comment_list[self.comment_list_selected].clone();
    let Some(idx) = self.files.iter().position(|e| e.path == entry.path) else {
      self.status = format!("File not found: {}", entry.path);
      return Ok(());
    };
    self.file_selected = idx;
    self.reload_diff_for_selected()?;
    match entry.locator {
      CommentLocator::File => {
        self.diff_cursor = 0;
        self.focus = Focus::Diff;
        self.close_comment_list();
        return Ok(());
      }
      CommentLocator::Line { side, line } => {
        let keep = match side {
          LineSide::Old => KeepLine::Old(line),
          LineSide::New => KeepLine::New(line),
        };
        if let Some(idx) = self.find_row_for_keep_line(keep) {
          self.diff_cursor = idx;
          self.focus = Focus::Diff;
          self.close_comment_list();
        } else {
          self.status = format!("Line {line} not found in diff");
        }
      }
    }
    Ok(())
  }

  fn build_comment_list(&self) -> Vec<CommentListEntry> {
    let mut out = Vec::new();
    for (path, file) in &self.review.files {
      if let Some(c) = file
        .file_comment
        .as_ref()
        .filter(|c| !c.body.trim().is_empty())
      {
        out.push(CommentListEntry {
          path: path.clone(),
          locator: CommentLocator::File,
          body: c.body.clone(),
          resolved: c.resolved,
        });
      }
      for (k, c) in &file.comments {
        if c.body.trim().is_empty() {
          continue;
        }
        out.push(CommentListEntry {
          path: path.clone(),
          locator: CommentLocator::Line {
            side: k.side,
            line: k.line,
          },
          body: c.body.clone(),
          resolved: c.resolved,
        });
      }
    }
    out
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
    self.prompt_buffer = crate::ui::empty_textarea();
    self.reviewed_files.clear();
    self.status.clear();
    self.file_selected = 0;
    self.file_scroll = 0;
    self.diff_cursor = 0;
    self.diff_scroll = 0;
    self.editor_target = None;
    self.editor_buffer = crate::ui::empty_textarea();

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

    self.refresh_review_from_sources()?;

    self.reviewed_files = self
      .review
      .files
      .iter()
      .filter(|(_, f)| f.reviewed)
      .map(|(path, _)| path.clone())
      .collect();

    self.apply_reviewed_invalidation()?;

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

    self.refresh_review_from_sources()?;

    self.reviewed_files = self
      .review
      .files
      .iter()
      .filter(|(_, f)| f.reviewed)
      .map(|(path, _)| path.clone())
      .collect();

    self.apply_reviewed_invalidation()?;

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

  fn apply_reviewed_invalidation(&mut self) -> Result<()> {
    if self.review.files.is_empty() {
      return Ok(());
    }

    let review_checks = {
      let base_tree = if self.view == ViewKind::Base {
        self
          .base_ref
          .as_deref()
          .map(|b| crate::git::merge_base_tree(&self.repo, b))
          .transpose()?
      } else {
        None
      };

      let mut checks = Vec::new();
      for (path, file) in self.review.files.iter() {
        if !file.reviewed {
          continue;
        }
        let current = self.review_hash_for_current_view_with_base(base_tree.as_ref(), path)?;
        let all_hash = if self.view == ViewKind::Base {
          self.review_hash_for_view(ViewKind::All, None, path)?
        } else {
          None
        };
        checks.push((path.clone(), file.reviewed_hash.clone(), current, all_hash));
      }
      checks
    };

    let mut changed = Vec::new();
    for (path, stored, current, all_hash) in review_checks {
      let Some(file) = self.review.files.get_mut(&path) else {
        continue;
      };
      match stored.as_deref() {
        None => {
          if let Some(hash) = current {
            file.reviewed_hash = Some(hash);
            changed.push(path.clone());
          }
        }
        Some(prev) => {
          let mut valid = current.as_deref() == Some(prev);
          if !valid {
            valid = all_hash.as_deref() == Some(prev);
          }
          if !valid {
            file.reviewed = false;
            file.reviewed_hash = None;
            changed.push(path.clone());
          }
        }
      }
    }

    if !changed.is_empty() && self.head_commit_oid.is_some() {
      for path in changed {
        self.persist_file_note(&path)?;
      }
    }

    Ok(())
  }

  fn keep_cursor_line(&self) -> Option<KeepLine> {
    let row = self.diff_rows.get(self.diff_cursor)?;
    match row {
      RenderRow::Unified(r) => match r.kind {
        crate::diff::Kind::Remove => r.old_line.map(KeepLine::Old),
        crate::diff::Kind::Add | crate::diff::Kind::Context => r.new_line.map(KeepLine::New),
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
      self
        .base_ref
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
      self
        .base_ref
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
      .iter()
      .filter(|&l| l.kind != crate::diff::Kind::FileHeader)
      .cloned()
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
      DiffViewMode::Decorated => {
        self.build_decorated_rows(&path, before.as_deref(), after.as_deref())?
      }
      DiffViewMode::Unified => self.build_unified_rows(&path, &diff_lines, &raw)?,
      DiffViewMode::SideBySide => self.build_side_by_side_rows(&path, &diff_lines)?,
    };

    rows.insert(0, RenderRow::FileHeader { path: path.clone() });

    self.diff_rows = rows;
    // Store diff data for popup display
    self.current_before = before;
    self.current_after = after;
    self.current_diff_lines = diff_lines_all; // Store full diff including headers
    self.current_file_path = Some(path.clone());
    self.diff_cursor = self
      .diff_rows
      .iter()
      .position(|r| {
        matches!(
          r,
          RenderRow::Unified(_) | RenderRow::SideBySide(_) | RenderRow::Decorated(_)
        )
      })
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
    // Decorated mode handles all file types (added, deleted, modified)
    if self.diff_view_mode == DiffViewMode::Decorated {
      return DiffViewMode::Decorated;
    }

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
    let was_reviewed = self
      .review
      .files
      .get(&path)
      .map(|f| f.reviewed)
      .unwrap_or(false);
    let new_reviewed = !was_reviewed;
    let hash = if new_reviewed {
      match self.review_hash_for_current_view(&path) {
        Ok(v) => v,
        Err(e) => {
          self.status = format!("Failed to hash reviewed file: {e}");
          return;
        }
      }
    } else {
      None
    };

    let entry = self.review.files.entry(path.clone()).or_default();
    entry.reviewed = new_reviewed;
    entry.reviewed_hash = hash;
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
          // Marker is fixed-width (2 cells), plus two spaces before line numbers.
          format!("    {old_s} {new_s} │ {path}")
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
        RenderRow::Decorated(r) => {
          let git_marker = match r.status {
            crate::diff::LineStatus::Unchanged => ' ',
            crate::diff::LineStatus::Added => '+',
            crate::diff::LineStatus::Removed => '-',
            crate::diff::LineStatus::Modified => '~',
          };
          let line_s = if r.line_number > 0 {
            format!("{:>new_w$}", r.line_number)
          } else {
            " ".repeat(new_w)
          };
          let code = r
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
          // Marker is fixed-width (2 cells).
          format!("  {line_s} {git_marker} │ {code}")
        }
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
          // Marker is fixed-width (2 cells).
          format!("  {diff_prefix} {old_s} {new_s} │ {code}")
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
      RenderRow::Decorated(r) => {
        if r.line_number > 0 {
          CommentLocator::Line {
            side: LineSide::New,
            line: r.line_number,
          }
        } else if let Some(old_line) = r.old_line_number {
          CommentLocator::Line {
            side: LineSide::Old,
            line: old_line,
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
    self.editor_buffer = crate::ui::textarea_from_string(existing);
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

    let comment = crate::ui::textarea_contents(&self.editor_buffer);
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
          self
            .review
            .set_line_comment(&target.path, side, line, comment)
        }
      }
      if let CommentLocator::Line { side, line } = target.locator
        && let Some(hash) = crate::add_cmd::current_snippet_hash(
          &self.repo,
          self.base_ref.as_deref(),
          &target.path,
          crate::review::LineKey { side, line },
        )
      {
        self
          .review
          .set_line_comment_snippet_hash(&target.path, side, line, Some(hash));
      }
      self.persist_file_note(&target.path)?;
      self.status = "Saved".to_string();
    }

    self.mode = Mode::Browse;
    self.editor_target = None;
    self.editor_buffer = crate::ui::empty_textarea();

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
            || (matches!(r.left_kind, Some(crate::diff::Kind::Remove)) && r.old_line.is_some())
        }
        RenderRow::Decorated(r) => {
          // Can comment on any line with a valid line number
          r.line_number > 0 || r.old_line_number.is_some()
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
    crate::add_cmd::write_draft_from_review_no_meta(
      &self.repo,
      self.base_ref.as_deref(),
      &self.review,
    )?;

    if self.head_commit_oid.is_none() {
      self.status = "No HEAD commit — notes disabled".to_string();
      return Ok(());
    }

    self.persist_resolved_state(path)?;

    let report =
      crate::add_cmd::sync_draft_notes(&self.repo, &self.notes_ref, self.base_ref.as_deref())?;
    if report.draft_updated {
      self.refresh_review_from_sources()?;
    }

    self.persist_reviewed_state(path)?;
    Ok(())
  }

  fn persist_resolved_state(&mut self, path: &str) -> Result<()> {
    let Some(head) = self.head_commit_oid else {
      return Ok(());
    };
    let (view_for_key, base_for_key) = match self.view {
      ViewKind::Base => (ViewKind::Base, self.base_ref_for_key()),
      _ => (ViewKind::All, None),
    };

    let oid = crate::git::note_file_key_oid(&self.repo, head, view_for_key, base_for_key, path)?;
    let note = crate::notes::read(&self.repo, &self.notes_ref, &oid)
      .with_context(|| format!("read file note for '{path}'"))?;
    let Some(text) = note.as_deref() else {
      return Ok(());
    };
    let mut file = crate::review::decode_file_note(text).unwrap_or_default();

    if let Some(review_file) = self.review.files.get(path) {
      if let (Some(src), Some(dest)) = (&review_file.file_comment, &mut file.file_comment) {
        dest.resolved = src.resolved;
      }
      for (key, comment) in &review_file.comments {
        if let Some(existing) = file.comments.get_mut(key) {
          existing.resolved = comment.resolved;
        }
      }
    }

    if !file.comments.is_empty() || file.file_comment.is_some() || file.reviewed {
      let note = crate::review::encode_file_note(&file);
      crate::notes::write(&self.repo, &self.notes_ref, &oid, Some(&note))
        .with_context(|| format!("write file note for '{path}'"))?;
    }
    Ok(())
  }

  fn persist_reviewed_state(&mut self, path: &str) -> Result<()> {
    let Some(head) = self.head_commit_oid else {
      return Ok(());
    };
    // Worktree review notes are view-agnostic: always store them under the `all` key.
    // Base-diff notes stay under the `base` key because line anchors are relative to that diff.
    let (view_for_key, base_for_key) = match self.view {
      ViewKind::Base => (ViewKind::Base, self.base_ref_for_key()),
      _ => (ViewKind::All, None),
    };

    let oid = crate::git::note_file_key_oid(&self.repo, head, view_for_key, base_for_key, path)?;

    let note = crate::notes::read(&self.repo, &self.notes_ref, &oid)
      .with_context(|| format!("read file note for '{path}'"))?;
    let mut file = note
      .as_deref()
      .and_then(crate::review::decode_file_note)
      .unwrap_or_default();

    let (reviewed, reviewed_hash) = self
      .review
      .files
      .get(path)
      .map(|f| (f.reviewed, f.reviewed_hash.clone()))
      .unwrap_or((false, None));

    file.reviewed = reviewed;
    file.reviewed_hash = reviewed_hash;

    if !file.comments.is_empty() || file.file_comment.is_some() || file.reviewed {
      let note = crate::review::encode_file_note(&file);
      crate::notes::write(&self.repo, &self.notes_ref, &oid, Some(&note))
        .with_context(|| format!("write file note for '{path}'"))?;
    } else {
      crate::notes::write(&self.repo, &self.notes_ref, &oid, None)
        .with_context(|| format!("delete file note for '{path}'"))?;
    }
    Ok(())
  }
}

fn merge_file_review(target: &mut FileReview, incoming: FileReview) {
  if incoming.reviewed {
    target.reviewed = true;
    if target.reviewed_hash.is_none() {
      target.reviewed_hash = incoming.reviewed_hash;
    }
  }
  match (&target.file_comment, &incoming.file_comment) {
    (None, Some(_)) => target.file_comment = incoming.file_comment,
    (Some(t), Some(i)) if t.resolved && !i.resolved => target.file_comment = incoming.file_comment,
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

fn parse_diff_context(raw: &str) -> Option<u32> {
  let parsed = raw.trim().parse::<u32>().ok()?;
  Some(parsed.clamp(MIN_DIFF_CONTEXT, MAX_DIFF_CONTEXT))
}

impl App {
  fn read_before_after(
    &self,
    base_tree: Option<&gix::Tree<'_>>,
    path: &str,
  ) -> Result<(Option<String>, Option<String>)> {
    self.read_before_after_for_view(self.view, base_tree, path)
  }

  fn read_before_after_for_view(
    &self,
    view: ViewKind,
    base_tree: Option<&gix::Tree<'_>>,
    path: &str,
  ) -> Result<(Option<String>, Option<String>)> {
    let out = match view {
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

  fn review_hash_for_view(
    &self,
    view: ViewKind,
    base_tree: Option<&gix::Tree<'_>>,
    path: &str,
  ) -> Result<Option<String>> {
    let (before, after) = self.read_before_after_for_view(view, base_tree, path)?;
    if before.is_none() && after.is_none() {
      return Ok(None);
    }
    let hash = review_fingerprint(before.as_deref(), after.as_deref())?;
    Ok(Some(hash))
  }

  fn review_hash_for_current_view(&self, path: &str) -> Result<Option<String>> {
    let base_tree = if self.view == ViewKind::Base {
      self
        .base_ref
        .as_deref()
        .map(|b| crate::git::merge_base_tree(&self.repo, b))
        .transpose()?
    } else {
      None
    };
    self.review_hash_for_current_view_with_base(base_tree.as_ref(), path)
  }

  fn review_hash_for_current_view_with_base(
    &self,
    base_tree: Option<&gix::Tree<'_>>,
    path: &str,
  ) -> Result<Option<String>> {
    let view = if self.view == ViewKind::Base {
      ViewKind::Base
    } else {
      ViewKind::All
    };
    self.review_hash_for_view(view, base_tree, path)
  }

  fn toggle_diff_view_mode(&mut self) -> Result<()> {
    let keep_new_line = self.diff_rows.get(self.diff_cursor).and_then(|r| match r {
      RenderRow::Unified(r) => r.new_line,
      RenderRow::SideBySide(r) => r.new_line,
      RenderRow::Decorated(r) => Some(r.line_number),
      RenderRow::Section { .. } => None,
      RenderRow::FileHeader { .. } => None,
    });

    self.diff_view_mode = match self.diff_view_mode {
      DiffViewMode::Decorated => DiffViewMode::SideBySide,
      DiffViewMode::SideBySide => DiffViewMode::Unified,
      DiffViewMode::Unified => DiffViewMode::Decorated,
    };

    self.reload_diff_for_selected()?;

    if let Some(n) = keep_new_line
      && let Some(idx) = self.diff_rows.iter().position(|r| match r {
        RenderRow::Unified(r) => r.new_line == Some(n),
        RenderRow::SideBySide(r) => r.new_line == Some(n),
        RenderRow::Decorated(r) => r.line_number == n,
        RenderRow::Section { .. } => false,
        RenderRow::FileHeader { .. } => false,
      })
    {
      self.diff_cursor = idx;
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

  fn jump_next_hunk(&mut self) {
    if self.diff_rows.is_empty() {
      return;
    }
    let start = self.diff_cursor.saturating_add(1);
    if start >= self.diff_rows.len() {
      return;
    }
    if let Some(rel) = self.diff_rows[start..]
      .iter()
      .position(|r| matches!(r, RenderRow::Section { .. }))
    {
      self.diff_cursor = start + rel;
    }
  }

  fn build_unified_rows(
    &self,
    path: &str,
    diff_lines: &[crate::diff::Line],
    _raw: &str,
  ) -> Result<Vec<RenderRow>> {
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
          // Merge inline emphasis with syntax highlighting
          if let Some(inline_spans) = &dl.inline_spans {
            let syntax_spans = code_by_diff[idx].as_deref();
            Self::merge_inline_with_syntax(inline_spans, syntax_spans, dl.kind)
          } else {
            code_by_diff[idx].clone().unwrap_or_else(|| {
              vec![ratatui::text::Span::raw(
                dl.text.get(1..).unwrap_or("").to_string(),
              )]
            })
          }
        }
        _ => {
          // File headers
          vec![ratatui::text::Span::raw(dl.text.clone())]
        }
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

  // Merge inline emphasis with syntax highlighting
  fn merge_inline_with_syntax(
    _inline_spans: &[crate::diff::InlineSpan],
    syntax_spans: Option<&[ratatui::text::Span<'static>]>,
    kind: crate::diff::Kind,
  ) -> Vec<ratatui::text::Span<'static>> {
    use ratatui::style::Color;

    // Base style based on diff kind (no emphasis, just color)
    let base_color = match kind {
      crate::diff::Kind::Remove => Color::Red,
      crate::diff::Kind::Add => Color::Green,
      _ => return vec![ratatui::text::Span::raw("")],
    };

    // If we have syntax highlighting, use it as-is
    if let Some(syntax) = syntax_spans {
      return syntax.to_vec();
    }

    // No syntax highlighting, just return spans with base color
    // Reconstruct text from inline spans without emphasis
    let text = _inline_spans
      .iter()
      .map(|s| s.text.as_str())
      .collect::<String>();
    vec![ratatui::text::Span::styled(
      text,
      ratatui::style::Style::default().fg(base_color),
    )]
  }

  fn build_side_by_side_rows(
    &self,
    path: &str,
    diff_lines: &[crate::diff::Line],
  ) -> Result<Vec<RenderRow>> {
    struct Temp {
      left_code: Option<String>,
      right_code: Option<String>,
      left_inline: Option<Vec<crate::diff::InlineSpan>>,
      right_inline: Option<Vec<crate::diff::InlineSpan>>,
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
            left_inline: None,
            right_inline: None,
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
              left_inline: left.and_then(|l| l.inline_spans.clone()),
              right_inline: right.and_then(|l| l.inline_spans.clone()),
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
        && let Some(t) = &temps[row_idx]
      {
        if let Some(inline) = &t.left_inline {
          r.left_spans =
            Self::merge_inline_with_syntax(inline, Some(&line), crate::diff::Kind::Remove);
        } else {
          r.left_spans = line;
        }
      }
    }
    for (row_idx, line_idx) in right_map {
      if let Some(line) = right_hl.get(line_idx).cloned()
        && let RenderRow::SideBySide(r) = &mut rows[row_idx]
        && let Some(t) = &temps[row_idx]
      {
        if let Some(inline) = &t.right_inline {
          r.right_spans =
            Self::merge_inline_with_syntax(inline, Some(&line), crate::diff::Kind::Add);
        } else {
          r.right_spans = line;
        }
      }
    }

    Ok(rows)
  }

  fn build_decorated_rows(
    &self,
    path: &str,
    before: Option<&str>,
    after: Option<&str>,
  ) -> Result<Vec<RenderRow>> {
    let decorated_lines = crate::diff::decorated_file_diff(before, after)?;

    // For syntax highlighting, we need to highlight the visible content
    // For deleted files, highlight old content. For others, highlight new content.
    let (syntax_content, is_deleted) = match (before, after) {
      (Some(b), None) => (b, true),
      (_, Some(a)) => (a, false),
      (None, None) => ("", false),
    };

    let lang = self.highlighter.detect_file_lang(&self.repo, path);
    let syntax_hl = match lang {
      Some(lang) => self.highlighter.highlight_lang(lang, syntax_content)?,
      None => Vec::new(),
    };

    let mut rows = Vec::with_capacity(decorated_lines.len());

    for dl in decorated_lines {
      // Get syntax spans for this line
      let syntax = if dl.line_number > 0 {
        // For added/modified/unchanged lines in the new file
        syntax_hl.get((dl.line_number - 1) as usize)
      } else if is_deleted {
        // For lines in a deleted file, use old_line_number
        dl.old_line_number
          .and_then(|n| syntax_hl.get((n - 1) as usize))
      } else {
        // For removed lines in a modified file - no direct syntax mapping
        None
      };

      // Determine the diff kind for styling (unused now, kept for potential future use)
      let _diff_kind = match dl.status {
        crate::diff::LineStatus::Added => crate::diff::Kind::Add,
        crate::diff::LineStatus::Removed => crate::diff::Kind::Remove,
        crate::diff::LineStatus::Modified => crate::diff::Kind::Add,
        crate::diff::LineStatus::Unchanged => crate::diff::Kind::Context,
      };

      let spans = syntax
        .cloned()
        .unwrap_or_else(|| vec![ratatui::text::Span::raw(dl.text.clone())]);

      rows.push(RenderRow::Decorated(DecoratedRow {
        status: dl.status,
        line_number: dl.line_number,
        old_line_number: dl.old_line_number,
        spans,
      }));
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

fn review_fingerprint(before: Option<&str>, after: Option<&str>) -> Result<String> {
  let mut h = hasher(Kind::Sha1);
  h.update(b"remark-review-v1\0");
  hash_part(&mut h, b"before", before);
  hash_part(&mut h, b"after", after);
  let oid = h.try_finalize().context("finalize review hash")?;
  Ok(oid.to_string())
}

fn hash_part(h: &mut gix_hash::Hasher, label: &[u8], value: Option<&str>) {
  h.update(label);
  h.update(b"\0");
  match value {
    Some(v) => {
      h.update(b"1\0");
      let len = v.len().to_string();
      h.update(len.as_bytes());
      h.update(b"\0");
      h.update(v.as_bytes());
    }
    None => {
      h.update(b"0\0");
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn test_app(repo: gix::Repository) -> App {
    App {
      repo,
      notes_ref: crate::git::DEFAULT_NOTES_REF.to_string(),
      base_ref: None,
      show_ignored: false,
      jump_target: None,
      view: ViewKind::All,
      focus: Focus::Files,
      mode: Mode::Browse,
      diff_view_mode: DiffViewMode::Decorated,
      diff_context: DEFAULT_DIFF_CONTEXT,
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
      editor_buffer: crate::ui::empty_textarea(),
      prompt_buffer: crate::ui::empty_textarea(),
      status: String::new(),
      show_help: false,
      show_prompt: false,
      show_diff_popup: false,
      comment_list: Vec::new(),
      comment_list_selected: 0,
      comment_list_marked: HashSet::new(),
      current_before: None,
      current_after: None,
      current_diff_lines: Vec::new(),
      current_file_path: None,
      highlighter: Highlighter::new().expect("highlighter"),
    }
  }

  #[test]
  fn diff_metrics_account_for_marker_width() {
    let td = tempfile::tempdir().expect("tempdir");
    let repo = gix::init(td.path()).expect("init repo");
    let mut app = test_app(repo);
    app.diff_rows.push(RenderRow::Unified(DiffRow {
      kind: crate::diff::Kind::Context,
      old_line: Some(1),
      new_line: Some(1),
      spans: vec![ratatui::text::Span::raw("x")],
    }));

    // This width is chosen so the marker width determines whether the row wraps.
    app.recompute_diff_metrics(16);

    assert_eq!(app.diff_row_heights, vec![2]);
    assert_eq!(app.diff_total_visual_lines, 2);
  }
}
