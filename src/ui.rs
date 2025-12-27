use std::io::{Stdout, stdout};

use anyhow::{Context, Result};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{Focus, Mode};
use crate::git::ViewKind;

pub struct Ui {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Ui {
    pub fn new() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        execute!(stdout(), EnterAlternateScreen).context("enter alt screen")?;
        let backend = CrosstermBackend::new(stdout());
        let terminal = Terminal::new(backend).context("create terminal")?;
        Ok(Self { terminal })
    }

    pub fn restore(&mut self) -> Result<()> {
        disable_raw_mode().ok();
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen).ok();
        self.terminal.show_cursor().ok();
        Ok(())
    }
}

pub struct DrawState<'a> {
    pub view: ViewKind,
    pub base_ref: Option<&'a str>,
    pub focus: Focus,
    pub mode: Mode,
    pub notes_ref: &'a str,
    pub review_dirty: bool,

    pub files: &'a [String],
    pub selected_file: usize,

    pub file_path: &'a str,
    pub file_lines: &'a [String],
    pub file_cursor_line: usize,
    pub file_scroll: u16,
    pub commented_lines: &'a [u32],
    pub current_comment: &'a str,

    pub comment_buffer: &'a NoteBuffer,
    pub comment_dirty: bool,

    pub status: &'a str,
    pub show_help: bool,
    pub show_prompt: bool,
    pub prompt_text: &'a str,
}

pub fn draw(f: &mut ratatui::Frame, s: DrawState<'_>) {
    let outer = f.area();
    let [main, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .areas(outer);

    let [left, right] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(42), Constraint::Min(1)])
        .areas(main);

    let [comment_area, file_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(10), Constraint::Min(1)])
        .areas(right);

    draw_files(f, left, s.files, s.selected_file, s.view, s.base_ref, s.focus);
    draw_comment_panel(
        f,
        comment_area,
        s.mode,
        s.focus,
        s.comment_buffer,
        s.comment_dirty,
        s.current_comment,
        s.notes_ref,
        s.review_dirty,
    );
    draw_file_view(
        f,
        file_area,
        s.file_path,
        s.file_lines,
        s.file_cursor_line,
        s.file_scroll,
        s.commented_lines,
        s.focus,
    );
    draw_footer(f, footer, s.view, s.base_ref, s.status, s.review_dirty);

    if s.show_help {
        draw_help(f, outer);
    }

    if s.show_prompt {
        draw_prompt(f, outer, s.prompt_text);
    }

    if s.mode == Mode::EditComment && s.focus == Focus::Comment {
        // Cursor position within comment box.
        let cursor_y = comment_area.y + 1 + s.comment_buffer.cursor_row as u16;
        let cursor_x = comment_area.x + 1 + s.comment_buffer.cursor_col as u16;
        if cursor_y < comment_area.y + comment_area.height && cursor_x < comment_area.x + comment_area.width {
            f.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

fn draw_files(
    f: &mut ratatui::Frame,
    area: Rect,
    files: &[String],
    selected: usize,
    view: ViewKind,
    base_ref: Option<&str>,
    focus: Focus,
) {
    let title = match (view, base_ref) {
        (ViewKind::Unstaged, _) => "Unstaged",
        (ViewKind::Staged, _) => "Staged",
        (ViewKind::Base, Some(b)) => {
            // Keep it short.
            if b.len() > 34 {
                "Base"
            } else {
                "Base"
            }
        }
        (ViewKind::Base, None) => "Base (unset)",
    };

    let items = files
        .iter()
        .map(|p| ListItem::new(Line::from(Span::raw(p.clone()))))
        .collect::<Vec<_>>();

    let mut block = Block::default().borders(Borders::ALL).title(title);
    if focus == Focus::Files {
        block = block.border_style(Style::default().fg(Color::Cyan));
    }

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");

    let mut state = ListState::default();
    if !files.is_empty() {
        state.select(Some(selected));
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_comment_panel(
    f: &mut ratatui::Frame,
    area: Rect,
    mode: Mode,
    focus: Focus,
    buffer: &NoteBuffer,
    dirty: bool,
    current_comment: &str,
    notes_ref: &str,
    review_dirty: bool,
) {
    let title = match (mode, dirty) {
        (Mode::EditComment, true) => format!("Comment (editing, unsaved) — {notes_ref}"),
        (Mode::EditComment, false) => format!("Comment (editing) — {notes_ref}"),
        (_, _) if review_dirty => format!("Comment — {notes_ref} (review unsaved)"),
        _ => format!("Comment — {notes_ref}"),
    };

    let mut block = Block::default().borders(Borders::ALL).title(title);
    if focus == Focus::Comment {
        block = block.border_style(Style::default().fg(Color::Cyan));
    }

    let text = if mode == Mode::EditComment {
        Text::from(
            buffer
                .lines
                .iter()
                .map(|l| Line::from(Span::raw(l.clone())))
                .collect::<Vec<_>>(),
        )
    } else if current_comment.is_empty() {
        Text::raw("No comment on this line. Press 'c' to add one.")
    } else {
        Text::raw(current_comment)
    };

    let para = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((0, 0));
    f.render_widget(para, area);
}

fn draw_file_view(
    f: &mut ratatui::Frame,
    area: Rect,
    path: &str,
    lines: &[String],
    cursor_line: usize,
    scroll: u16,
    commented_lines: &[u32],
    focus: Focus,
) {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(if path.is_empty() { "File" } else { path });
    if focus == Focus::Lines {
        block = block.border_style(Style::default().fg(Color::Cyan));
    }

    let view_height = area.height.saturating_sub(2).max(1) as usize;
    let scroll_usize = scroll as usize;
    let end = (scroll_usize + view_height).min(lines.len());

    let mut rendered = Vec::with_capacity(end.saturating_sub(scroll_usize));
    for (idx, line) in lines[scroll_usize..end].iter().enumerate() {
        let actual = scroll_usize + idx;
        let line_no = (actual + 1) as u32;

        let has_comment = commented_lines.binary_search(&line_no).is_ok();
        let marker = if has_comment { "●" } else { " " };
        let gutter = format!("{marker} {:4} ", line_no);

        let mut spans = Vec::new();
        spans.push(Span::styled(
            gutter,
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::raw(line.clone()));

        let mut line_style = Style::default();
        if focus == Focus::Lines && actual == cursor_line {
            line_style = line_style.add_modifier(Modifier::REVERSED);
        }
        rendered.push(Line::from(spans).style(line_style));
    }

    let para = Paragraph::new(Text::from(rendered))
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn draw_footer(
    f: &mut ratatui::Frame,
    area: Rect,
    view: ViewKind,
    base_ref: Option<&str>,
    status: &str,
    review_dirty: bool,
) {
    let view_label = match (view, base_ref) {
        (ViewKind::Unstaged, _) => "unstaged",
        (ViewKind::Staged, _) => "staged",
        (ViewKind::Base, Some(b)) => b,
        (ViewKind::Base, None) => "base",
    };

    let mut left = format!(
        "view={}  (2/3/4)  Tab focus  c comment  d delete  Ctrl+S save  p prompt  ? help  q quit",
        view_label
    );
    if review_dirty {
        left.push_str("  [unsaved]");
    }
    if !status.is_empty() {
        left.push_str("  |  ");
        left.push_str(status);
    }

    let para = Paragraph::new(left)
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Left);
    f.render_widget(para, area);
}

fn draw_help(f: &mut ratatui::Frame, area: Rect) {
    let help = Text::from(vec![
        Line::from("Views"),
        Line::from("  2         Unstaged changes (worktree)"),
        Line::from("  3         Staged changes (index)"),
        Line::from("  4         Base changes (merge-base..HEAD)"),
        Line::from(""),
        Line::from("Navigation"),
        Line::from("  Tab       Switch focus (files/lines)"),
        Line::from("  Up/Down   Move selection / cursor"),
        Line::from("  PgUp/Dn   Scroll file view"),
        Line::from(""),
        Line::from("Comments"),
        Line::from("  c         Edit comment at current line"),
        Line::from("  d         Delete comment at current line"),
        Line::from("  Esc       Apply comment (in editor)"),
        Line::from("  Ctrl+S    Save review note (git-notes)"),
        Line::from(""),
        Line::from("Other"),
        Line::from("  p         Toggle collated prompt preview"),
        Line::from("  ?         Toggle this help"),
        Line::from("  q         Quit"),
    ]);

    let popup = centered_rect(76, 80, area);
    f.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).title("Help");
    let para = Paragraph::new(help).block(block).wrap(Wrap { trim: false });
    f.render_widget(para, popup);
}

fn draw_prompt(f: &mut ratatui::Frame, area: Rect, prompt: &str) {
    let popup = centered_rect(82, 82, area);
    f.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).title("LLM Prompt Preview (from note)");
    let para = Paragraph::new(Text::raw(prompt))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((0, 0));
    f.render_widget(para, popup);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[derive(Debug, Clone)]
pub struct NoteBuffer {
    pub(crate) lines: Vec<String>,
    pub(crate) cursor_row: usize,
    pub(crate) cursor_col: usize,
}

impl NoteBuffer {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    pub fn from_string(s: String) -> Self {
        let mut lines = s
            .trim_end_matches(['\r', '\n'])
            .split('\n')
            .map(|l| l.to_string())
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            lines,
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    pub fn as_string(&self) -> String {
        self.lines.join("\n")
    }

    pub fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_row];
        let idx = byte_index(line, self.cursor_col);
        line.insert(idx, c);
        self.cursor_col += 1;
    }

    pub fn insert_newline(&mut self) {
        let line = &mut self.lines[self.cursor_row];
        let idx = byte_index(line, self.cursor_col);
        let rest = line[idx..].to_string();
        line.truncate(idx);
        self.lines.insert(self.cursor_row + 1, rest);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    pub fn backspace(&mut self) -> bool {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            let remove_at = byte_index(line, self.cursor_col - 1);
            let next = byte_index(line, self.cursor_col);
            line.replace_range(remove_at..next, "");
            self.cursor_col -= 1;
            return true;
        }

        if self.cursor_row > 0 {
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            let prev = &mut self.lines[self.cursor_row];
            let prev_len = prev.chars().count();
            prev.push_str(&current);
            self.cursor_col = prev_len;
            return true;
        }

        false
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            return;
        }
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
        }
    }

    pub fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < line_len {
            self.cursor_col += 1;
            return;
        }
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor_row == 0 {
            return;
        }
        self.cursor_row -= 1;
        self.cursor_col = self
            .cursor_col
            .min(self.lines[self.cursor_row].chars().count());
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 >= self.lines.len() {
            return;
        }
        self.cursor_row += 1;
        self.cursor_col = self
            .cursor_col
            .min(self.lines[self.cursor_row].chars().count());
    }

    pub fn move_line_start(&mut self) {
        self.cursor_col = 0;
    }

    pub fn move_line_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }
}

fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

