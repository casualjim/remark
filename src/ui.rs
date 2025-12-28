use std::io::{Stdout, stdout};

use anyhow::{Context, Result};
use crossterm::execute;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{
    CommentLocator, CommentTarget, DiffViewMode, FileChangeKind, FileEntry, FileStageKind, Focus,
    Mode, RenderRow, SideBySideRow,
};
use crate::git::ViewKind;
use crate::review::Review;

pub struct Ui {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Ui {
    pub fn new() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        execute!(stdout(), EnterAlternateScreen, EnableMouseCapture).context("enter alt screen")?;
        let backend = CrosstermBackend::new(stdout());
        let terminal = Terminal::new(backend).context("create terminal")?;
        Ok(Self { terminal })
    }

    pub fn restore(&mut self) -> Result<()> {
        disable_raw_mode().ok();
        execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        )
        .ok();
        self.terminal.show_cursor().ok();
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutRects {
    pub files: Rect,
    pub diff: Rect,
}

pub fn layout(area: Rect) -> LayoutRects {
    let [files, diff] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(40), Constraint::Min(1)])
        .areas(area);
    LayoutRects { files, diff }
}

pub struct DrawState<'a> {
    pub view: ViewKind,
    pub base_ref: Option<&'a str>,
    pub focus: Focus,
    pub mode: Mode,
    pub diff_view_mode: DiffViewMode,

    pub review_dirty: bool,
    pub review: &'a Review,

    pub files: &'a [FileEntry],
    pub file_selected: usize,
    pub file_scroll: u16,

    pub diff_rows: &'a [RenderRow],
    pub diff_cursor: usize,
    pub diff_scroll: u16,

    pub editor_target: Option<&'a CommentTarget>,
    pub editor_buffer: &'a NoteBuffer,

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

    let rects = layout(main);
    draw_files(f, rects.files, &s);
    draw_diff(f, rects.diff, &s);
    draw_footer(f, footer, &s);

    if s.show_help && s.mode == Mode::Browse {
        draw_help(f, outer);
    }
    if s.show_prompt && s.mode == Mode::Browse {
        draw_prompt(f, outer, s.prompt_text);
    }
    if s.mode == Mode::EditComment {
        draw_comment_editor(f, rects.diff, &s);
    }
}

fn draw_files(f: &mut ratatui::Frame, area: Rect, s: &DrawState<'_>) {
    let title = match (s.view, s.base_ref) {
        (ViewKind::All, _) => "Files (all)",
        (ViewKind::Unstaged, _) => "Files (unstaged)",
        (ViewKind::Staged, _) => "Files (staged)",
        (ViewKind::Base, Some(_)) => "Files (base)",
        (ViewKind::Base, None) => "Files (base: unset)",
    };

    let mut block = Block::default().borders(Borders::ALL).title(title);
    if s.mode == Mode::Browse && s.focus == Focus::Files {
        block = block.border_style(Style::default().fg(Color::Cyan));
    }

    let inner = block.inner(area);
    f.render_widget(block, area);

    let view_height = inner.height.max(1) as usize;
    let scroll = s.file_scroll as usize;
    let end = (scroll + view_height).min(s.files.len());

    let mut items = Vec::with_capacity(end.saturating_sub(scroll));
    for e in &s.files[scroll..end] {
        let (st, st_color) = match e.stage {
            FileStageKind::Staged => ("S", Color::Blue),
            FileStageKind::Unstaged => ("U", Color::Yellow),
            FileStageKind::Partial => ("P", Color::Magenta),
            FileStageKind::None => (" ", Color::DarkGray),
        };
        let (tag, color) = match e.change {
            FileChangeKind::Added => ("A", Color::Green),
            FileChangeKind::Deleted => ("D", Color::Red),
            FileChangeKind::Modified => ("M", Color::Yellow),
        };
        let has_any = s.review.has_any_comments(&e.path);
        let cm = if has_any { "💬" } else { " " };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("{st} "), Style::default().fg(st_color)),
            Span::styled(format!("{tag} "), Style::default().fg(color)),
            Span::styled(format!("{cm} "), Style::default().fg(if has_any { Color::Yellow } else { Color::DarkGray })),
            Span::raw(e.path.clone()),
        ])));
    }

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");

    let mut state = ListState::default();
    if !s.files.is_empty() {
        let visible_selected = s.file_selected.saturating_sub(scroll);
        if visible_selected < view_height {
            state.select(Some(visible_selected));
        }
    }
    f.render_stateful_widget(list, inner, &mut state);
}

fn draw_diff(f: &mut ratatui::Frame, area: Rect, s: &DrawState<'_>) {
    let title = if s.mode == Mode::EditComment {
        "Diff (locked while editing)".to_string()
    } else {
        s.files
            .get(s.file_selected)
            .map(|e| {
                let has_file = s
                    .review
                    .file_comment(&e.path)
                    .map(|c| !c.trim().is_empty())
                    .unwrap_or(false);
                format!("Diff — {}{}", e.path, if has_file { " 💬" } else { "" })
            })
            .unwrap_or_else(|| "Diff".to_string())
    };

    let mut block = Block::default().borders(Borders::ALL).title(title);
    if s.mode == Mode::Browse && s.focus == Focus::Diff {
        block = block.border_style(Style::default().fg(Color::Cyan));
    }

    let inner = block.inner(area);
    f.render_widget(block, area);

    let view_height = inner.height.max(1) as usize;
    let scroll = s.diff_scroll as usize;
    let end = (scroll + view_height).min(s.diff_rows.len());

    let old_max = s
        .diff_rows
        .iter()
        .filter_map(|r| match r {
            RenderRow::Unified(r) => r.old_line,
            RenderRow::SideBySide(r) => r.old_line,
            RenderRow::Section { .. } => None,
            RenderRow::FileHeader { .. } => None,
        })
        .max()
        .unwrap_or(0);
    let new_max = s
        .diff_rows
        .iter()
        .filter_map(|r| match r {
            RenderRow::Unified(r) => r.new_line,
            RenderRow::SideBySide(r) => r.new_line,
            RenderRow::Section { .. } => None,
            RenderRow::FileHeader { .. } => None,
        })
        .max()
        .unwrap_or(0);
    let old_w = old_max.to_string().len().max(4);
    let new_w = new_max.to_string().len().max(4);

    let selected_change = s.files.get(s.file_selected).map(|e| e.change);
    let is_new_file = matches!(selected_change, Some(FileChangeKind::Added));
    let is_deleted_file = matches!(selected_change, Some(FileChangeKind::Deleted));

    let mut rendered: Vec<Line<'static>> = Vec::with_capacity(end.saturating_sub(scroll));
    for (i, row) in s.diff_rows[scroll..end].iter().enumerate() {
        let abs_idx = scroll + i;

        let path = s.files.get(s.file_selected).map(|e| e.path.as_str());
        let locator = match (row, path) {
            (RenderRow::FileHeader { .. }, Some(_)) => Some(CommentLocator::File),
            (RenderRow::Unified(r), Some(_)) => match r.kind {
                crate::diff::Kind::Remove => r
                    .old_line
                    .map(|line| CommentLocator::Line { side: crate::review::LineSide::Old, line }),
                crate::diff::Kind::Add | crate::diff::Kind::Context => r
                    .new_line
                    .map(|line| CommentLocator::Line { side: crate::review::LineSide::New, line }),
                _ => None,
            },
            (RenderRow::SideBySide(r), Some(_)) => {
                if matches!(
                    r.right_kind,
                    Some(crate::diff::Kind::Add) | Some(crate::diff::Kind::Context)
                ) {
                    r.new_line.map(|line| CommentLocator::Line {
                        side: crate::review::LineSide::New,
                        line,
                    })
                } else if matches!(r.left_kind, Some(crate::diff::Kind::Remove)) {
                    r.old_line.map(|line| CommentLocator::Line {
                        side: crate::review::LineSide::Old,
                        line,
                    })
                } else {
                    None
                }
            }
            _ => None,
        };

        let commentable = locator.is_some();
        let has_comment = match (path, locator) {
            (Some(p), Some(CommentLocator::File)) => s
                .review
                .file_comment(p)
                .map(|c| !c.trim().is_empty())
                .unwrap_or(false),
            (Some(p), Some(CommentLocator::Line { side, line })) => s
                .review
                .line_comment(p, side, line)
                .map(|c| !c.trim().is_empty())
                .unwrap_or(false),
            _ => false,
        };

        match row {
            RenderRow::FileHeader { path } => {
                // Keep this gutter column a fixed width (emoji are often 2 cells).
                let marker = if has_comment { "💬" } else { "  " };
                let old_s = " ".repeat(old_w);
                let new_s = " ".repeat(new_w);

                let mut spans: Vec<Span<'static>> = Vec::with_capacity(8);
                spans.push(Span::styled(
                    marker.to_string(),
                    Style::default().fg(if has_comment { Color::Yellow } else { Color::Reset }),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(old_s, Style::default().fg(Color::DarkGray)));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(new_s, Style::default().fg(Color::DarkGray)));
                spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
                spans.push(Span::styled(
                    path.clone(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ));

                let mut style = Style::default().bg(Color::Rgb(25, 25, 25));
                if abs_idx == s.diff_cursor {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                rendered.push(Line::from(spans).style(style));
            }
            RenderRow::Section { text } => {
                let mut deco = format!("┄┄ {text} ┄┄");
                let w = inner.width.max(1) as usize;
                let len = deco.chars().count();
                if len < w {
                    deco.push_str(&" ".repeat(w - len));
                }
                let mut style = Style::default()
                    .fg(Color::Cyan)
                    .bg(Color::Rgb(30, 30, 30))
                    .add_modifier(Modifier::BOLD);
                if abs_idx == s.diff_cursor {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                rendered.push(Line::from(Span::styled(deco, style)));
            }
            RenderRow::Unified(r) => {
                // Keep this gutter column a fixed width (emoji are often 2 cells).
                let marker = if has_comment { "💬" } else { "  " };
                let diff_prefix = match r.kind {
                    crate::diff::Kind::Add => '+',
                    crate::diff::Kind::Remove => '-',
                    crate::diff::Kind::Context => ' ',
                    _ => ' ',
                };
                let diff_prefix_style = match r.kind {
                    crate::diff::Kind::Add => Style::default().fg(Color::Green),
                    crate::diff::Kind::Remove => Style::default().fg(Color::Red),
                    _ => Style::default().fg(Color::DarkGray),
                };
                let old_s = r
                    .old_line
                    .map(|n| format!("{n:>old_w$}"))
                    .unwrap_or_else(|| " ".repeat(old_w));
                let new_s = r
                    .new_line
                    .map(|n| format!("{n:>new_w$}"))
                    .unwrap_or_else(|| " ".repeat(new_w));

                let mut spans: Vec<Span<'static>> = Vec::with_capacity(10 + r.spans.len());
                spans.push(Span::styled(
                    marker.to_string(),
                    Style::default().fg(if has_comment { Color::Yellow } else { Color::Reset }),
                ));
                spans.push(Span::styled(diff_prefix.to_string(), diff_prefix_style));
                spans.push(Span::raw(if commentable { " " } else { " " }));
                spans.push(Span::styled(old_s, Style::default().fg(Color::DarkGray)));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(new_s, Style::default().fg(Color::DarkGray)));
                spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
                spans.extend(r.spans.iter().cloned());

                let mut style = match r.kind {
                    crate::diff::Kind::Add if is_new_file => Style::default(),
                    crate::diff::Kind::Remove if is_deleted_file => Style::default(),
                    crate::diff::Kind::Add => Style::default().bg(Color::Rgb(0, 50, 0)),
                    crate::diff::Kind::Remove => Style::default().bg(Color::Rgb(60, 0, 0)),
                    _ => Style::default(),
                };
                if abs_idx == s.diff_cursor {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                rendered.push(Line::from(spans).style(style));
            }
            RenderRow::SideBySide(r) => {
                rendered.push(render_side_by_side_line(
                    r,
                    abs_idx == s.diff_cursor,
                    commentable,
                    has_comment,
                    old_w,
                    new_w,
                    is_new_file,
                    is_deleted_file,
                ));
            }
        }
    }

    let para = Paragraph::new(Text::from(rendered)).wrap(Wrap { trim: false });
    f.render_widget(para, inner);
}

fn render_side_by_side_line(
    row: &SideBySideRow,
    selected: bool,
    commentable: bool,
    has_comment: bool,
    old_w: usize,
    new_w: usize,
    is_new_file: bool,
    is_deleted_file: bool,
) -> Line<'static> {
    fn tint(spans: &[Span<'static>], bg: Color) -> Vec<Span<'static>> {
        spans
            .iter()
            .map(|s| Span::styled(s.content.to_string(), s.style.bg(bg)))
            .collect()
    }

    let marker = if has_comment { "💬" } else { "  " };

    let old_s = row
        .old_line
        .map(|n| format!("{n:>old_w$}"))
        .unwrap_or_else(|| " ".repeat(old_w));
    let new_s = row
        .new_line
        .map(|n| format!("{n:>new_w$}"))
        .unwrap_or_else(|| " ".repeat(new_w));

    let left_prefix = match row.left_kind {
        Some(crate::diff::Kind::Remove) => '-',
        Some(crate::diff::Kind::Context) => ' ',
        _ => ' ',
    };
    let right_prefix = match row.right_kind {
        Some(crate::diff::Kind::Add) => '+',
        Some(crate::diff::Kind::Context) => ' ',
        _ => ' ',
    };

    let left_prefix_style = match row.left_kind {
        Some(crate::diff::Kind::Remove) => Style::default().fg(Color::Red),
        Some(crate::diff::Kind::Context) => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::DarkGray),
    };
    let right_prefix_style = match row.right_kind {
        Some(crate::diff::Kind::Add) => Style::default().fg(Color::Green),
        Some(crate::diff::Kind::Context) => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::DarkGray),
    };

    let mut left_spans = row.left_spans.clone();
    let mut right_spans = row.right_spans.clone();
    if !is_deleted_file && matches!(row.left_kind, Some(crate::diff::Kind::Remove)) {
        left_spans = tint(&left_spans, Color::Rgb(60, 0, 0));
    }
    if !is_new_file && matches!(row.right_kind, Some(crate::diff::Kind::Add)) {
        right_spans = tint(&right_spans, Color::Rgb(0, 50, 0));
    }

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(12 + left_spans.len() + right_spans.len());
    spans.push(Span::styled(
        marker.to_string(),
        Style::default().fg(if has_comment { Color::Yellow } else { Color::Reset }),
    ));
    spans.push(Span::raw(if commentable { " " } else { " " }));

    spans.push(Span::styled(old_s, Style::default().fg(Color::DarkGray)));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(left_prefix.to_string(), left_prefix_style));
    spans.push(Span::raw(" "));
    spans.extend(left_spans);

    spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));

    spans.push(Span::styled(new_s, Style::default().fg(Color::DarkGray)));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(right_prefix.to_string(), right_prefix_style));
    spans.push(Span::raw(" "));
    spans.extend(right_spans);

    let mut style = Style::default();
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Line::from(spans).style(style)
}

fn draw_footer(f: &mut ratatui::Frame, area: Rect, s: &DrawState<'_>) {
    let view_label = match (s.view, s.base_ref) {
        (ViewKind::All, _) => "all".to_string(),
        (ViewKind::Unstaged, _) => "unstaged".to_string(),
        (ViewKind::Staged, _) => "staged".to_string(),
        (ViewKind::Base, Some(b)) => format!("base({b})"),
        (ViewKind::Base, None) => "base(unset)".to_string(),
    };

    let focus = match s.focus {
        Focus::Files => "files",
        Focus::Diff => "diff",
    };

    let diff_mode = match s.diff_view_mode {
        DiffViewMode::Unified => "unified",
        DiffViewMode::SideBySide => "side-by-side",
    };

    let show_copy = s.show_prompt && s.mode == Mode::Browse;

    let mut left = match s.mode {
        Mode::Browse => {
            // Keep this single-line footer readable on narrow terminals by progressively compacting.
            let wide = format!(
                "focus={}  view={}  diff={}  Tab  1/2/3/4 views  i mode  ↑/↓ PgUp/PgDn  c comment  d delete  Ctrl+S save  p prompt{}  ? help  q quit",
                focus,
                view_label,
                diff_mode,
                if show_copy { "  y copy" } else { "" }
            );
            let mid = format!(
                "view={} diff={}  Tab  1-4 views  i mode  ↑/↓ PgUp/PgDn  c comment  Ctrl+S save  ? help  q quit",
                view_label, diff_mode
            );
            let narrow =
                "Tab focus  1-4 view  i mode  c comment  ? help  q quit".to_string();

            let w = area.width as usize;
            if wide.chars().count() <= w {
                wide
            } else if mid.chars().count() <= w {
                mid
            } else {
                fit_with_ellipsis(&narrow, w)
            }
        }
        Mode::EditComment => {
            let s = "comment editor  (F2 accept)  (Esc cancel)".to_string();
            fit_with_ellipsis(&s, area.width as usize)
        }
    };

    if s.review_dirty {
        // Append only if it fits; otherwise the footer becomes useless.
        let suffix = "  [unsaved]";
        if left.chars().count() + suffix.chars().count() <= area.width as usize {
            left.push_str(suffix);
        }
    }
    if !s.status.is_empty() {
        let suffix = format!("  |  {}", s.status);
        if left.chars().count() + suffix.chars().count() <= area.width as usize {
            left.push_str(&suffix);
        }
    }

    let para = Paragraph::new(left)
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Left);
    f.render_widget(para, area);
}

fn fit_with_ellipsis(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if s.chars().count() <= width {
        return s.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    for ch in s.chars().take(width - 1) {
        out.push(ch);
    }
    out.push('…');
    out
}

fn draw_help(f: &mut ratatui::Frame, area: Rect) {
    let help = Text::from(vec![
        Line::from("Focus"),
        Line::from("  Tab / Shift+Tab   Switch between files and diff"),
        Line::from(""),
        Line::from("Views"),
        Line::from("  1 / 2 / 3 / 4     All / Unstaged / Staged / Base"),
        Line::from(""),
        Line::from("Files"),
        Line::from("  Up/Down           Select file"),
        Line::from("  Enter             Move focus to diff"),
        Line::from(""),
        Line::from("Diff"),
        Line::from("  Up/Down, PgUp/Dn  Navigate diff"),
        Line::from("  i                 Toggle unified / side-by-side"),
        Line::from("  c                 Add/edit comment (file or line)"),
        Line::from("  d                 Delete comment (file or line)"),
        Line::from(""),
        Line::from("Review"),
        Line::from("  Ctrl+S            Save review note"),
        Line::from("  p                Toggle prompt preview"),
        Line::from("  y                Copy prompt to clipboard (when open)"),
        Line::from("  q / Q            Quit"),
        Line::from(""),
        Line::from("Comment editor"),
        Line::from("  F2 / Alt+Enter    Accept and move on"),
        Line::from("  Esc               Cancel"),
    ]);

    let popup = centered_rect(78, 80, area);
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

fn draw_comment_editor(f: &mut ratatui::Frame, diff_area: Rect, s: &DrawState<'_>) {
    let Some(target) = s.editor_target else { return };
    let popup_h = 6u16.min(diff_area.height.saturating_sub(2)).max(3);
    let popup_w = (diff_area.width.saturating_sub(4)).min(90).max(20);

    let inner = diff_area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });
    let rel_y = s.diff_cursor.saturating_sub(s.diff_scroll as usize) as u16;
    let cursor_y = inner.y.saturating_add(rel_y);

    let mut y = cursor_y.saturating_add(1);
    if y + popup_h > inner.y + inner.height {
        y = cursor_y.saturating_sub(popup_h);
    }
    if y < inner.y {
        y = inner.y;
    }
    let x = inner.x + (inner.width.saturating_sub(popup_w)) / 2;

    let popup = Rect {
        x,
        y,
        width: popup_w,
        height: popup_h,
    };

    f.render_widget(Clear, popup);
    let title = match target.locator {
        CommentLocator::File => format!("Comment {} (file)  (F2/Alt+Enter accept)", target.path),
        CommentLocator::Line { side, line } => {
            let side = match side {
                crate::review::LineSide::Old => "old",
                crate::review::LineSide::New => "new",
            };
            format!(
                "Comment {}:{} ({side})  (F2/Alt+Enter accept)",
                target.path, line
            )
        }
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let text = Text::from(
        s.editor_buffer
            .lines
            .iter()
            .map(|l| Line::from(Span::raw(l.clone())))
            .collect::<Vec<_>>(),
    );
    let para = Paragraph::new(text).wrap(Wrap { trim: false });
    f.render_widget(para, inner);

    let cur_y = inner.y + s.editor_buffer.cursor_row as u16;
    let cur_x = inner.x + s.editor_buffer.cursor_col as u16;
    if cur_y < inner.y + inner.height && cur_x < inner.x + inner.width {
        f.set_cursor_position((cur_x, cur_y));
    }
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
