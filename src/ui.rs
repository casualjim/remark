use std::collections::HashSet;
use std::io::{Stdout, stdout};

use anyhow::{Context, Result};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
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
use unicode_width::UnicodeWidthChar;

use crate::app::{
    CommentLocator, CommentTarget, DiffViewMode, FileChangeKind, FileEntry, Focus, Mode, RenderRow,
    SideBySideRow,
};
use crate::file_tree::FileTreeRow;
use crate::git::ViewKind;
use crate::review::{CommentState, Review};

pub struct Ui {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    keyboard_enhancements: bool,
}

impl Ui {
    pub fn new() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        execute!(stdout(), EnterAlternateScreen, EnableMouseCapture).context("enter alt screen")?;
        let keyboard_enhancements = match crossterm::terminal::supports_keyboard_enhancement() {
            Ok(true) => {
                execute!(
                    stdout(),
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    )
                )
                .context("enable keyboard enhancements")?;
                true
            }
            _ => false,
        };
        let backend = CrosstermBackend::new(stdout());
        let terminal = Terminal::new(backend).context("create terminal")?;
        Ok(Self {
            terminal,
            keyboard_enhancements,
        })
    }

    pub fn restore(&mut self) -> Result<()> {
        disable_raw_mode().ok();
        if self.keyboard_enhancements {
            execute!(self.terminal.backend_mut(), PopKeyboardEnhancementFlags).ok();
        }
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
    pub diff_context: u32,
    pub review: &'a Review,

    pub files: &'a [FileEntry],
    pub file_rows: &'a [FileTreeRow],
    pub file_selected: usize,
    pub file_row_selected: Option<usize>,
    pub file_scroll: u16,

    pub diff_rows: &'a [RenderRow],
    pub diff_cursor: usize,
    pub diff_scroll: u16,
    pub diff_cursor_visual: u32,
    pub reviewed_files: &'a HashSet<String>,

    pub editor_target: Option<&'a CommentTarget>,
    pub editor_buffer: &'a NoteBuffer,
    pub prompt_buffer: &'a NoteBuffer,
    pub prompt_scroll: u16,

    pub status: &'a str,
    pub show_help: bool,
    pub show_prompt: bool,
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
    if s.show_prompt {
        draw_prompt(f, outer, &s);
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
    let end = (scroll + view_height).min(s.file_rows.len());

    let mut items = Vec::with_capacity(end.saturating_sub(scroll));
    for row in &s.file_rows[scroll..end] {
        if row.is_dir {
            items.push(ListItem::new(Line::from(vec![Span::styled(
                row.label.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )])));
            continue;
        }

        let Some(file_index) = row.file_index else {
            continue;
        };
        let Some(e) = s.files.get(file_index) else {
            continue;
        };

        let state = s.review.comment_state(&e.path);

        let mut name_style = git_status_style(e.git_xy);
        match state {
            CommentState::HasUnresolved => {
                name_style = name_style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            CommentState::ResolvedOnly => {
                name_style = name_style.add_modifier(Modifier::DIM);
            }
            CommentState::None => {}
        }
        if s.reviewed_files.contains(&e.path) {
            name_style = name_style.add_modifier(Modifier::DIM);
        }
        let label = if s.reviewed_files.contains(&e.path) {
            mark_reviewed_label(&row.label)
        } else {
            row.label.clone()
        };
        items.push(ListItem::new(Line::from(vec![Span::styled(
            label, name_style,
        )])));
    }

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");

    let mut state = ListState::default();
    if let Some(selected_row) = s.file_row_selected {
        let visible_selected = selected_row.saturating_sub(scroll);
        if visible_selected < view_height {
            state.select(Some(visible_selected));
        }
    }
    f.render_stateful_widget(list, inner, &mut state);
}

fn git_status_style(xy: [char; 2]) -> Style {
    let [x, y] = xy;
    if x == '-' && y == '-' {
        return Style::default().fg(Color::DarkGray);
    }
    if y == 'N' {
        return Style::default().fg(Color::Magenta);
    }
    if y == 'I' {
        return Style::default().fg(Color::DarkGray);
    }
    if x == 'A' || y == 'A' {
        return Style::default().fg(Color::Green);
    }
    if x == 'D' || y == 'D' {
        return Style::default().fg(Color::Red);
    }
    if x == 'R' || y == 'R' || x == 'C' || y == 'C' {
        return Style::default().fg(Color::Cyan);
    }
    if x == 'T' || y == 'T' {
        return Style::default().fg(Color::Blue);
    }
    if x == 'U' || y == 'U' {
        return Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
    }
    Style::default().fg(Color::Yellow)
}

fn mark_reviewed_label(label: &str) -> String {
    if let Some((before, after)) = label.rsplit_once("─ ") {
        format!("{before}─✓{after}")
    } else {
        format!("✓ {}", label)
    }
}

fn draw_diff(f: &mut ratatui::Frame, area: Rect, s: &DrawState<'_>) {
    let title = if s.mode == Mode::EditComment {
        "Diff (locked while editing)".to_string()
    } else {
        s.files
            .get(s.file_selected)
            .map(|e| match s.review.comment_state(&e.path) {
                CommentState::HasUnresolved => format!("Diff — {} 💬", e.path),
                CommentState::ResolvedOnly => format!("Diff — {} ✓", e.path),
                CommentState::None => format!("Diff — {}", e.path),
            })
            .unwrap_or_else(|| "Diff".to_string())
    };

    let mut block = Block::default().borders(Borders::ALL).title(title);
    if s.mode == Mode::Browse && s.focus == Focus::Diff {
        block = block.border_style(Style::default().fg(Color::Cyan));
    }

    let inner = block.inner(area);
    f.render_widget(block, area);

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

    let mut rendered: Vec<Line<'static>> = Vec::with_capacity(s.diff_rows.len());
    for (abs_idx, row) in s.diff_rows.iter().enumerate() {
        let path = s.files.get(s.file_selected).map(|e| e.path.as_str());
        let locator = match (row, path) {
            (RenderRow::FileHeader { .. }, Some(_)) => Some(CommentLocator::File),
            (RenderRow::Unified(r), Some(_)) => match r.kind {
                crate::diff::Kind::Remove => r.old_line.map(|line| CommentLocator::Line {
                    side: crate::review::LineSide::Old,
                    line,
                }),
                crate::diff::Kind::Add | crate::diff::Kind::Context => {
                    r.new_line.map(|line| CommentLocator::Line {
                        side: crate::review::LineSide::New,
                        line,
                    })
                }
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
        let marker_state = match (path, locator) {
            (Some(p), Some(CommentLocator::File)) => s.review.file_comment(p).and_then(|c| {
                if c.body.trim().is_empty() {
                    None
                } else if c.resolved {
                    Some(CommentState::ResolvedOnly)
                } else {
                    Some(CommentState::HasUnresolved)
                }
            }),
            (Some(p), Some(CommentLocator::Line { side, line })) => {
                s.review.line_comment(p, side, line).and_then(|c| {
                    if c.body.trim().is_empty() {
                        None
                    } else if c.resolved {
                        Some(CommentState::ResolvedOnly)
                    } else {
                        Some(CommentState::HasUnresolved)
                    }
                })
            }
            _ => None,
        };

        match row {
            RenderRow::FileHeader { path } => {
                // Keep this gutter column a fixed width (emoji are often 2 cells).
                let (marker, marker_style) = match marker_state {
                    Some(CommentState::HasUnresolved) => ("💬", Style::default().fg(Color::Yellow)),
                    Some(CommentState::ResolvedOnly) => ("✓ ", Style::default().fg(Color::Green)),
                    _ => ("  ", Style::default().fg(Color::Reset)),
                };
                let old_s = " ".repeat(old_w);
                let new_s = " ".repeat(new_w);

                let spans: Vec<Span<'static>> = vec![
                    Span::styled(marker.to_string(), marker_style),
                    Span::raw(" "),
                    Span::raw(" "),
                    Span::styled(old_s, Style::default().fg(Color::DarkGray)),
                    Span::raw(" "),
                    Span::styled(new_s, Style::default().fg(Color::DarkGray)),
                    Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        path.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ];

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
                let (marker, marker_style) = match marker_state {
                    Some(CommentState::HasUnresolved) => ("💬", Style::default().fg(Color::Yellow)),
                    Some(CommentState::ResolvedOnly) => ("✓ ", Style::default().fg(Color::Green)),
                    _ => ("  ", Style::default().fg(Color::Reset)),
                };
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
                spans.push(Span::styled(marker.to_string(), marker_style));
                spans.push(Span::styled(diff_prefix.to_string(), diff_prefix_style));
                spans.push(Span::raw(" "));
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
                    matches!(
                        marker_state,
                        Some(CommentState::HasUnresolved | CommentState::ResolvedOnly)
                    ),
                    matches!(marker_state, Some(CommentState::ResolvedOnly)),
                    old_w,
                    new_w,
                    is_new_file,
                    is_deleted_file,
                    inner.width as usize,
                ));
            }
        }
    }

    let para = Paragraph::new(Text::from(rendered))
        .wrap(Wrap { trim: false })
        .scroll((s.diff_scroll, 0));
    f.render_widget(para, inner);
}

#[allow(clippy::too_many_arguments)]
fn render_side_by_side_line(
    row: &SideBySideRow,
    selected: bool,
    _commentable: bool,
    has_comment: bool,
    comment_resolved: bool,
    old_w: usize,
    new_w: usize,
    is_new_file: bool,
    is_deleted_file: bool,
    total_width: usize,
) -> Line<'static> {
    fn tint(spans: &[Span<'static>], bg: Color) -> Vec<Span<'static>> {
        spans
            .iter()
            .map(|s| Span::styled(s.content.to_string(), s.style.bg(bg)))
            .collect()
    }

    fn str_width(s: &str) -> usize {
        s.chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum()
    }

    fn spans_truncate_to_width(
        spans: &[Span<'static>],
        max_width: usize,
    ) -> (Vec<Span<'static>>, usize) {
        let mut out: Vec<Span<'static>> = Vec::new();
        let mut used = 0usize;

        for s in spans {
            if used >= max_width {
                break;
            }
            let mut chunk = String::new();
            for ch in s.content.chars() {
                let w = UnicodeWidthChar::width(ch).unwrap_or(0);
                if used + w > max_width {
                    break;
                }
                chunk.push(ch);
                used += w;
            }
            if !chunk.is_empty() {
                out.push(Span::styled(chunk, s.style));
            }
        }

        (out, used)
    }

    fn pad_spaces(width: usize, style: Style) -> Span<'static> {
        Span::styled(" ".repeat(width), style)
    }

    let (marker, marker_style) = if has_comment && comment_resolved {
        ("✓ ", Style::default().fg(Color::Green))
    } else if has_comment {
        ("💬", Style::default().fg(Color::Yellow))
    } else {
        ("  ", Style::default().fg(Color::Reset))
    };

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

    let left_bg = if !is_deleted_file && matches!(row.left_kind, Some(crate::diff::Kind::Remove)) {
        Some(Color::Rgb(60, 0, 0))
    } else {
        None
    };
    let right_bg = if !is_new_file && matches!(row.right_kind, Some(crate::diff::Kind::Add)) {
        Some(Color::Rgb(0, 50, 0))
    } else {
        None
    };

    let mut left_spans = row.left_spans.clone();
    let mut right_spans = row.right_spans.clone();
    if let Some(bg) = left_bg {
        left_spans = tint(&left_spans, bg);
    }
    if let Some(bg) = right_bg {
        right_spans = tint(&right_spans, bg);
    }

    // Allocate fixed-width columns so the right side never "slides" into the left side.
    // Layout:
    //   marker + sp + oldnum + sp + prefix + sp + leftcode + sp + │ + sp + newnum + sp + prefix + sp + rightcode
    let fixed_left = str_width(marker) + 1 + old_w + 1 + 1 + 1;
    let fixed_sep = 3usize; // " │ "
    let fixed_right = new_w + 1 + 1 + 1;
    let avail = total_width.saturating_sub(fixed_left + fixed_sep + fixed_right);
    let left_code_w = avail / 2;
    let right_code_w = avail.saturating_sub(left_code_w);

    let left_pad_style = match left_bg {
        Some(bg) => Style::default().bg(bg),
        None => Style::default(),
    };
    let right_pad_style = match right_bg {
        Some(bg) => Style::default().bg(bg),
        None => Style::default(),
    };

    let (mut left_code, left_used) = spans_truncate_to_width(&left_spans, left_code_w);
    if left_used < left_code_w {
        left_code.push(pad_spaces(left_code_w - left_used, left_pad_style));
    }
    let (mut right_code, right_used) = spans_truncate_to_width(&right_spans, right_code_w);
    if right_used < right_code_w {
        right_code.push(pad_spaces(right_code_w - right_used, right_pad_style));
    }

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(16 + left_code.len() + right_code.len());
    spans.push(Span::styled(marker.to_string(), marker_style));
    spans.push(Span::raw(" "));

    spans.push(Span::styled(old_s, Style::default().fg(Color::DarkGray)));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(left_prefix.to_string(), left_prefix_style));
    spans.push(Span::raw(" "));
    spans.extend(left_code);

    spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));

    spans.push(Span::styled(new_s, Style::default().fg(Color::DarkGray)));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(right_prefix.to_string(), right_prefix_style));
    spans.push(Span::raw(" "));
    spans.extend(right_code);

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

    let diff_mode = match s.diff_view_mode {
        DiffViewMode::Unified => "unified",
        DiffViewMode::SideBySide => "side-by-side",
    };

    let mut left = match s.mode {
        Mode::Browse => {
            // Intentionally minimal: rely on `?` for keybinding help.
            fit_with_ellipsis(
                &format!("view={view_label} diff={diff_mode} ctx={}", s.diff_context),
                area.width as usize,
            )
        }
        Mode::EditComment => {
            let s = "comment editor  (Shift+Enter/Ctrl+S accept)  (Esc cancel)".to_string();
            fit_with_ellipsis(&s, area.width as usize)
        }
        Mode::EditPrompt => {
            let s = "prompt editor  (Shift+Enter/Ctrl+S copy)  (Esc close)".to_string();
            fit_with_ellipsis(&s, area.width as usize)
        }
    };

    // Notes are written immediately on accept/delete/resolve; we don't display an "unsaved" state.
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
        Line::from("  h / l or Left/Right   Switch between files and diff"),
        Line::from(""),
        Line::from("Views"),
        Line::from("  1 / 2 / 3 / 4     All / Unstaged / Staged / Base"),
        Line::from(""),
        Line::from("Files"),
        Line::from("  Up/Down, j/k      Select file"),
        Line::from("  Ctrl+U / Ctrl+D   Page up / down"),
        Line::from("  Enter             Move focus to diff"),
        Line::from("  c                 Add/edit file comment"),
        Line::from("  v                 Toggle reviewed"),
        Line::from(""),
        Line::from("Diff"),
        Line::from("  Up/Down, j/k      Navigate diff"),
        Line::from("  PgUp/Dn           Page up / down"),
        Line::from("  Ctrl+U / Ctrl+D   Page up / down"),
        Line::from("  Ctrl+N / Ctrl+P   Next/prev unreviewed file"),
        Line::from("  i                 Toggle unified / side-by-side"),
        Line::from("  [ / ]             Less/more diff context"),
        Line::from("  R                 Reload file list"),
        Line::from("  c                 Add/edit comment (file or line)"),
        Line::from("  d                 Delete comment (file or line)"),
        Line::from("  r                 Resolve/unresolve comment"),
        Line::from(""),
        Line::from("Review"),
        Line::from("  p                 Open prompt editor"),
        Line::from("  q / Q             Quit"),
        Line::from("  Esc               Dismiss overlay or quit"),
        Line::from(""),
        Line::from("Comment editor"),
        Line::from("  Shift+Enter/Ctrl+S  Accept and close"),
        Line::from("  Enter             Newline"),
        Line::from("  Esc               Cancel"),
        Line::from(""),
        Line::from("Prompt editor"),
        Line::from("  Shift+Enter/Ctrl+S  Copy prompt and close"),
        Line::from("  Esc               Close prompt"),
        Line::from(""),
        Line::from("Help"),
        Line::from("  ? or Esc          Close this help"),
    ]);

    let popup = centered_rect(78, 80, area);
    f.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).title("Help");
    let para = Paragraph::new(help).block(block).wrap(Wrap { trim: false });
    f.render_widget(para, popup);
}

pub fn prompt_popup_rect(area: Rect) -> Rect {
    centered_rect(82, 82, area)
}

fn draw_prompt(f: &mut ratatui::Frame, area: Rect, s: &DrawState<'_>) {
    let popup = prompt_popup_rect(area);
    f.render_widget(Clear, popup);
    let title = match s.mode {
        Mode::EditPrompt => {
            "LLM Prompt (editable)  (Shift+Enter/Ctrl+S copy, Esc close)".to_string()
        }
        _ => "LLM Prompt Preview (collated)".to_string(),
    };
    let block = Block::default().borders(Borders::ALL).title(title);

    let text = Text::from(
        s.prompt_buffer
            .lines
            .iter()
            .map(|l| Line::from(Span::raw(l.clone())))
            .collect::<Vec<_>>(),
    );
    let para = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((s.prompt_scroll, 0));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    f.render_widget(para, inner);

    if s.mode == Mode::EditPrompt {
        let rel_row = s
            .prompt_buffer
            .cursor_row
            .saturating_sub(s.prompt_scroll as usize) as u16;
        let cur_y = inner.y + rel_row;
        let cur_x = inner.x + s.prompt_buffer.cursor_col as u16;
        if cur_y < inner.y + inner.height && cur_x < inner.x + inner.width {
            f.set_cursor_position((cur_x, cur_y));
        }
    }
}

fn draw_comment_editor(f: &mut ratatui::Frame, diff_area: Rect, s: &DrawState<'_>) {
    let Some(target) = s.editor_target else {
        return;
    };
    let popup_h = 6u16.min(diff_area.height.saturating_sub(2)).max(3);
    let popup_w = (diff_area.width.saturating_sub(4)).clamp(20, 90);

    let inner = diff_area.inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 1,
    });
    let rel_y = s
        .diff_cursor_visual
        .saturating_sub(s.diff_scroll as u32)
        .min(u16::MAX as u32) as u16;
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
        CommentLocator::File => {
            format!(
                "Comment {} (file)  (Shift+Enter/Ctrl+S accept)",
                target.path
            )
        }
        CommentLocator::Line { side, line } => {
            let side = match side {
                crate::review::LineSide::Old => "old",
                crate::review::LineSide::New => "new",
            };
            format!(
                "Comment {}:{} ({side})  (Shift+Enter/Ctrl+S accept)",
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
