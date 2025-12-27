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

use crate::app::Mode;
use crate::git::CommitEntry;

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

pub fn estimated_diff_width(total_width: u16) -> u16 {
    total_width.saturating_sub(44).max(20)
}

pub fn draw(
    f: &mut ratatui::Frame,
    commits: &[CommitEntry],
    selected: usize,
    note: (&NoteBuffer, bool),
    diff: Option<&Text<'static>>,
    diff_scroll: u16,
    mode: Mode,
    status: &str,
    show_help: bool,
    notes_ref: &str,
) {
    let outer = f.area();
    let [main, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .areas(outer);

    let [left, right] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(42), Constraint::Min(1)])
        .areas(main);

    let [note_area, diff_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(10), Constraint::Min(1)])
        .areas(right);

    draw_commits(f, left, commits, selected);
    draw_note(f, note_area, note.0, note.1, mode, notes_ref);
    draw_diff(f, diff_area, diff, diff_scroll);
    draw_footer(f, footer, commits, selected, mode, status);

    if show_help {
        draw_help(f, outer);
    }
}

fn draw_commits(f: &mut ratatui::Frame, area: Rect, commits: &[CommitEntry], selected: usize) {
    let items = commits
        .iter()
        .map(|c| {
            let line = Line::from(vec![
                Span::styled(
                    format!("{:8}", c.short_id),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(" "),
                Span::raw(c.summary.clone()),
            ]);
            ListItem::new(line)
        })
        .collect::<Vec<_>>();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Commits"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");

    let mut state = ListState::default();
    state.select(Some(selected));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_note(
    f: &mut ratatui::Frame,
    area: Rect,
    note: &NoteBuffer,
    dirty: bool,
    mode: Mode,
    notes_ref: &str,
) {
    let title = match (mode, dirty) {
        (Mode::EditNote, true) => format!("Note (editing, unsaved) — {notes_ref}"),
        (Mode::EditNote, false) => format!("Note (editing) — {notes_ref}"),
        (Mode::Browse, true) => format!("Note (unsaved) — {notes_ref}"),
        (Mode::Browse, false) => format!("Note — {notes_ref}"),
    };

    let text = Text::from(
        note.lines
            .iter()
            .map(|l| Line::from(Span::raw(l.clone())))
            .collect::<Vec<_>>(),
    );

    let note_height = area.height.saturating_sub(2).max(1);
    let scroll = note
        .cursor_row
        .saturating_sub(note_height as usize - 1) as u16;

    let para = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, area);

    if mode == Mode::EditNote {
        let cursor_y = area.y.saturating_add(1).saturating_add(note.cursor_row as u16);
        let cursor_y = cursor_y.saturating_sub(scroll);
        let cursor_x = area.x.saturating_add(1).saturating_add(note.cursor_col as u16);
        if cursor_y < area.y + area.height && cursor_x < area.x + area.width {
            f.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

fn draw_diff(f: &mut ratatui::Frame, area: Rect, diff: Option<&Text<'static>>, scroll: u16) {
    let text = diff.cloned().unwrap_or_else(|| Text::raw("No diff"));
    let para = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title("Diff (difftastic)"))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, area);
}

fn draw_footer(
    f: &mut ratatui::Frame,
    area: Rect,
    commits: &[CommitEntry],
    selected: usize,
    mode: Mode,
    status: &str,
) {
    let current = commits.get(selected);
    let left = match (mode, current) {
        (Mode::Browse, Some(c)) => format!("{}  {}  (e edit, r refresh, ? help, q quit)", c.short_id, c.summary),
        (Mode::EditNote, Some(c)) => format!("{}  Editing note (Ctrl+S save, Esc leave)", c.short_id),
        (_, None) => "(no commit)".to_string(),
    };

    let mut line = left;
    if !status.is_empty() {
        line.push_str("  |  ");
        line.push_str(status);
    }

    let para = Paragraph::new(line)
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Left);
    f.render_widget(para, area);
}

fn draw_help(f: &mut ratatui::Frame, area: Rect) {
    let help = Text::from(vec![
        Line::from("Keys"),
        Line::from("  Up/Down   select commit"),
        Line::from("  PgUp/PgDn scroll diff"),
        Line::from("  e         edit note"),
        Line::from("  Ctrl+S    save note"),
        Line::from("  Esc       leave edit mode"),
        Line::from("  r         refresh current commit"),
        Line::from("  ?         toggle help"),
        Line::from("  q         quit (browse mode)"),
    ]);

    let popup = centered_rect(70, 60, area);
    f.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).title("Help");
    let para = Paragraph::new(help).block(block).wrap(Wrap { trim: false });
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
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
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

