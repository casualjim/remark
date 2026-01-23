use anyhow::Result;
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    FileHeader,
    HunkHeader,
    Context,
    Add,
    Remove,
}

/// Line status for the decorated view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStatus {
    Unchanged,
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone)]
pub struct InlineSpan {
    pub text: String,
    pub emphasized: bool,
}

#[derive(Debug, Clone)]
pub struct Line {
    #[allow(dead_code)]
    pub kind: Kind,
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub inline_spans: Option<Vec<InlineSpan>>,
}

pub fn unified_file_diff(
    before_label: &str,
    after_label: &str,
    before: Option<&str>,
    after: Option<&str>,
    context_lines: u32,
) -> Result<Vec<Line>> {
    let before_s = before.unwrap_or("");
    let after_s = after.unwrap_or("");

    let mut out = Vec::new();
    out.push(Line {
        kind: Kind::FileHeader,
        text: format!("--- {before_label}"),
        old_line: None,
        new_line: None,
        inline_spans: None,
    });
    out.push(Line {
        kind: Kind::FileHeader,
        text: format!("+++ {after_label}"),
        old_line: None,
        new_line: None,
        inline_spans: None,
    });

    // Use similar for diffing
    let diff = TextDiff::from_lines(before_s, after_s);

    // Group ops into hunks
    let grouped = diff.grouped_ops(context_lines as usize);

    for group in grouped {
        // Calculate hunk bounds
        let (before_start, before_len, after_start, after_len) =
            calculate_hunk_bounds(&group);

        // Generate hunk header with context hint
        let hunk_header_text = generate_hunk_header_text(
            &group,
            before_start,
            before_len,
            after_start,
            after_len,
            &diff,
        );

        out.push(Line {
            kind: Kind::HunkHeader,
            text: hunk_header_text,
            old_line: None,
            new_line: None,
            inline_spans: None,
        });

        // Process each op with inline changes
        for op in group {
            for change in diff.iter_inline_changes(&op) {
                let (kind, old_line, new_line) = match change.tag() {
                    ChangeTag::Equal => (
                        Kind::Context,
                        change.old_index().map(|i| i as u32 + 1),
                        change.new_index().map(|i| i as u32 + 1),
                    ),
                    ChangeTag::Delete => (
                        Kind::Remove,
                        change.old_index().map(|i| i as u32 + 1),
                        None,
                    ),
                    ChangeTag::Insert => (
                        Kind::Add,
                        None,
                        change.new_index().map(|i| i as u32 + 1),
                    ),
                };

                // Build inline spans with emphasis
                let inline_spans: Vec<InlineSpan> = change
                    .iter_strings_lossy()
                    .map(|(emphasized, text)| InlineSpan {
                        text: text.into_owned(),
                        emphasized,
                    })
                    .collect();

                // Build full text with prefix
                let prefix = match kind {
                    Kind::Remove => "-",
                    Kind::Add => "+",
                    Kind::Context => " ",
                    _ => "",
                };
                let full_text = format!("{}{}", prefix, change);

                out.push(Line {
                    kind,
                    text: full_text,
                    old_line,
                    new_line,
                    inline_spans: Some(inline_spans),
                });
            }
        }
    }

    Ok(out)
}

fn calculate_hunk_bounds(group: &[similar::DiffOp]) -> (u32, u32, u32, u32) {
    let before_start = group
        .first()
        .and_then(|op| {
            if op.new_range().is_empty() {
                Some(op.old_range().start)
            } else {
                None
            }
        })
        .or_else(|| {
            group.first().map(|op| {
                let start = op.old_range().start;
                if start > 0 { start - 1 } else { 0 }
            })
        })
        .unwrap_or(0);

    let before_end = group
        .last()
        .map(|op| op.old_range().end)
        .unwrap_or(0);

    let after_start = group
        .first()
        .and_then(|op| {
            if op.old_range().is_empty() {
                Some(op.new_range().start)
            } else {
                None
            }
        })
        .or_else(|| {
            group.first().map(|op| {
                let start = op.new_range().start;
                if start > 0 { start - 1 } else { 0 }
            })
        })
        .unwrap_or(0);

    let after_end = group
        .last()
        .map(|op| op.new_range().end)
        .unwrap_or(0);

    let before_len = before_end.saturating_sub(before_start);
    let after_len = after_end.saturating_sub(after_start);

    (
        before_start as u32 + 1,
        before_len as u32,
        after_start as u32 + 1,
        after_len as u32,
    )
}

fn generate_hunk_header_text(
    group: &[similar::DiffOp],
    before_start: u32,
    before_len: u32,
    after_start: u32,
    after_len: u32,
    diff: &TextDiff<'_, '_, '_, str>,
) -> String {
    let mut header_text = format!(
        "@@ -{},{} +{},{} @@",
        before_start, before_len, after_start, after_len
    );

    // Try to find a context hint
    if let Some(hint) = find_context_hint(group, diff) {
        header_text.push(' ');
        header_text.push_str(&hint);
    }

    header_text
}

fn find_context_hint(
    group: &[similar::DiffOp],
    diff: &TextDiff<'_, '_, '_, str>,
) -> Option<String> {
    // Prefer context lines for a stable "where am I" hint
    for op in group {
        for change in diff.iter_changes(op) {
            if change.tag() == ChangeTag::Equal {
                let txt = change.to_string();
                let trimmed = txt.trim().trim_matches(['\n', '\r']);
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    // If there is no context, fall back to the first non-empty changed line
    for op in group {
        for change in diff.iter_changes(op) {
            let txt = change.to_string();
            let trimmed = txt.trim().trim_matches(['\n', '\r']);
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    None
}

/// A single line in the decorated file view.
#[derive(Debug, Clone)]
pub struct DecoratedLine {
    pub status: LineStatus,
    pub line_number: u32,     // Line number in the new file (0 for deleted files or removed lines)
    pub old_line_number: Option<u32>,  // Line number in the old file (for reference)
    pub text: String,
}

/// Returns the full file (new version or old for deleted files) with line-by-line git status
/// and inline diff information for modified lines.
///
/// For deleted files, shows the old content with Removed status.
/// For added files, shows the new content with Added status.
/// For modified files, shows the new file content with per-line status.
pub fn decorated_file_diff(
    before: Option<&str>,
    after: Option<&str>,
) -> Result<Vec<DecoratedLine>> {
    // Determine which content to show (primary view)
    // For deleted files, show old content. Otherwise show new content.
    let (content, is_deleted) = match (before, after) {
        (Some(_), None) => (before.unwrap(), true),   // Deleted file
        (_, Some(after_content)) => (after_content, false),  // Added or modified
        (None, None) => ("", false),  // Edge case: both empty
    };

    let before_content = before.unwrap_or("");
    let lines: Vec<&str> = content.lines().collect();

    // If there's no "before" content (new file), all lines are added
    if before_content.is_empty() && !is_deleted {
        return Ok(lines
            .iter()
            .enumerate()
            .map(|(idx, line)| DecoratedLine {
                status: LineStatus::Added,
                line_number: (idx + 1) as u32,
                old_line_number: None,
                text: line.to_string(),
            })
            .collect());
    }

    // Use similar to compute line-by-line diff
    let diff = TextDiff::from_lines(before_content, content);

    // Build a map of line status for lines that have changes
    // Use a HashMap to store status by line number (0-based index)
    use std::collections::HashMap;
    let mut line_changes: HashMap<usize, LineStatus> = HashMap::new();

    for op in diff.ops() {
        // Get the changes with inline diff information
        for change in diff.iter_inline_changes(op) {
            let (status, _old_idx, new_idx) = match change.tag() {
                ChangeTag::Equal => {
                    let old_i = change.old_index();
                    let new_i = change.new_index();
                    (
                        LineStatus::Unchanged,
                        old_i.map(|i| i as usize),
                        new_i.map(|i| i as usize),
                    )
                }
                ChangeTag::Delete => {
                    let old_i = change.old_index();
                    // Removed line - doesn't exist in new file
                    (LineStatus::Removed, old_i.map(|i| i as usize), None)
                }
                ChangeTag::Insert => {
                    let new_i = change.new_index();
                    (
                        LineStatus::Added,
                        None,
                        new_i.map(|i| i as usize),
                    )
                }
            };

            // Only store lines that exist in the new file
            if let Some(new_i) = new_idx {
                line_changes.insert(new_i, status);
            }
        }
    }

    // Build the result by iterating through ALL lines of the file
    let mut result = Vec::with_capacity(lines.len());

    for (idx, line) in lines.iter().enumerate() {
        let line_num = idx + 1; // 1-based line number

        let status = line_changes
            .get(&idx)
            .copied()
            .unwrap_or(LineStatus::Unchanged);

        result.push(DecoratedLine {
            status,
            line_number: if is_deleted { 0 } else { line_num as u32 },
            old_line_number: if is_deleted { Some(line_num as u32) } else { None },
            text: line.to_string(),
        });
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_header_includes_context_hint_when_available() {
        let before = "fn keep() {}\nfn change() { 1 }\nfn tail() {}\n";
        let after = "fn keep() {}\nfn change() { 2 }\nfn tail() {}\n";

        let lines = unified_file_diff("a/x.rs", "b/x.rs", Some(before), Some(after), 3).unwrap();
        let header = lines
            .iter()
            .find(|l| l.kind == Kind::HunkHeader)
            .map(|l| l.text.as_str())
            .unwrap();
        assert!(header.contains("fn keep()"), "{header}");
    }

    #[test]
    fn hunk_header_falls_back_to_changed_line_when_no_context() {
        let before = "one\n";
        let after = "two\n";

        let lines = unified_file_diff("a/t.txt", "b/t.txt", Some(before), Some(after), 3).unwrap();
        let header = lines
            .iter()
            .find(|l| l.kind == Kind::HunkHeader)
            .map(|l| l.text.as_str())
            .unwrap();
        assert!(header.contains("one") || header.contains("two"), "{header}");
    }

    #[test]
    fn inline_highlights_changed_words() {
        let before = "fn foo() -> u32 { 1 }";
        let after = "fn foo() -> u32 { 42 }";

        let lines = unified_file_diff("a.rs", "b.rs", Some(before), Some(after), 3).unwrap();

        // Find the Add line
        let add_line = lines.iter().find(|l| l.kind == Kind::Add).unwrap();
        assert!(add_line.inline_spans.is_some());

        let spans = add_line.inline_spans.as_ref().unwrap();
        // "42" should be emphasized
        assert!(
            spans.iter().any(|s| s.emphasized && s.text.contains("42")),
            "expected emphasized '42' in spans: {spans:?}"
        );
        // "{", "}" should NOT be emphasized
        assert!(
            !spans.iter().any(|s| s.emphasized && (s.text == "{" || s.text == "}")),
            "did not expect emphasized braces in spans: {spans:?}"
        );
    }

    #[test]
    fn inline_handles_unicode_words() {
        let before = "let x = 1;";
        let after = "let value = 1;";

        let lines = unified_file_diff("a.rs", "b.rs", Some(before), Some(after), 3).unwrap();

        let add_line = lines.iter().find(|l| l.kind == Kind::Add).unwrap();
        let spans = add_line.inline_spans.as_ref().unwrap();

        // "value" should be emphasized as a whole word (unicode tokenization)
        assert!(
            spans.iter().any(|s| s.emphasized && s.text == "value"),
            "expected emphasized 'value' in spans: {spans:?}"
        );
    }

    #[test]
    fn decorated_returns_full_file() {
        let before = "line1\nline2\nline3\n";
        let after = "line1\nline2-modified\nline3\n";

        let lines = decorated_file_diff(Some(before), Some(after)).unwrap();

        // Should return 3 lines (full file), not just hunk
        assert_eq!(lines.len(), 3, "expected 3 lines, got {}", lines.len());
        assert_eq!(lines[0].text, "line1");
        assert_eq!(lines[1].text, "line2-modified");
        assert_eq!(lines[2].text, "line3");
    }
}
