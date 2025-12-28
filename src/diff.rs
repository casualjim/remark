use anyhow::{Context, Result};
use gix_diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix_diff::blob::{Algorithm, UnifiedDiff};
use gix_diff::blob::intern::InternedInput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    FileHeader,
    HunkHeader,
    Context,
    Add,
    Remove,
}

#[derive(Debug, Clone)]
pub struct Line {
    #[allow(dead_code)]
    pub kind: Kind,
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

pub fn unified_file_diff(
    before_label: &str,
    after_label: &str,
    before: Option<&str>,
    after: Option<&str>,
) -> Result<Vec<Line>> {
    let before_s = before.unwrap_or("");
    let after_s = after.unwrap_or("");

    let mut out = Vec::new();
    out.push(Line {
        kind: Kind::FileHeader,
        text: format!("--- {before_label}"),
        old_line: None,
        new_line: None,
    });
    out.push(Line {
        kind: Kind::FileHeader,
        text: format!("+++ {after_label}"),
        old_line: None,
        new_line: None,
    });

    let input = InternedInput::new(before_s.as_bytes(), after_s.as_bytes());
    let sink = UnifiedDiff::new(&input, CollectUnified::new(), ContextSize::symmetrical(3));
    let collected = gix_diff::blob::diff(Algorithm::Histogram, &input, sink)
        .context("render unified diff")?;
    out.extend(collected.lines);
    Ok(out)
}

#[derive(Default)]
struct CollectUnified {
    lines: Vec<Line>,
}

impl CollectUnified {
    fn new() -> Self {
        Self { lines: Vec::new() }
    }
}

impl ConsumeHunk for CollectUnified {
    type Out = CollectUnified;

    fn consume_hunk(&mut self, header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        let hint = lines
            .iter()
            .filter_map(|(k, bytes)| {
                // Prefer context lines for a stable "where am I" hint.
                if !matches!(k, DiffLineKind::Context) {
                    return None;
                }
                let txt = String::from_utf8_lossy(bytes);
                let trimmed = txt.trim_matches(['\n', '\r']).trim();
                if trimmed.is_empty() {
                    return None;
                }
                Some(trimmed.to_string())
            })
            .next()
            .or_else(|| {
                // If there is no context, fall back to the first non-empty changed line.
                lines.iter().find_map(|(_k, bytes)| {
                    let txt = String::from_utf8_lossy(bytes);
                    let trimmed = txt.trim_matches(['\n', '\r']).trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                })
            });

        let mut header_text = format!(
            "@@ -{},{} +{},{} @@",
            header.before_hunk_start,
            header.before_hunk_len,
            header.after_hunk_start,
            header.after_hunk_len
        );
        if let Some(h) = hint {
            header_text.push(' ');
            header_text.push_str(&h);
        }

        self.lines.push(Line {
            kind: Kind::HunkHeader,
            text: header_text,
            old_line: None,
            new_line: None,
        });

        let mut old_line = header.before_hunk_start;
        let mut new_line = header.after_hunk_start;

        for (kind, bytes) in lines {
            let txt = String::from_utf8_lossy(bytes).trim_end_matches(['\n', '\r']).to_string();
            match kind {
                DiffLineKind::Context => {
                    self.lines.push(Line {
                        kind: Kind::Context,
                        text: format!(" {txt}"),
                        old_line: Some(old_line),
                        new_line: Some(new_line),
                    });
                    old_line += 1;
                    new_line += 1;
                }
                DiffLineKind::Add => {
                    self.lines.push(Line {
                        kind: Kind::Add,
                        text: format!("+{txt}"),
                        old_line: None,
                        new_line: Some(new_line),
                    });
                    new_line += 1;
                }
                DiffLineKind::Remove => {
                    self.lines.push(Line {
                        kind: Kind::Remove,
                        text: format!("-{txt}"),
                        old_line: Some(old_line),
                        new_line: None,
                    });
                    old_line += 1;
                }
            }
        }

        Ok(())
    }

    fn finish(self) -> Self::Out {
        self
    }
}
