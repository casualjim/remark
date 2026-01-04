use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::AddCli;
use crate::git::ViewKind;
use crate::prompt_code::LineCodeResolver;
use crate::review::{LineKey, LineSide, Review};

const DRAFT_DIR: &str = "remark";
const DRAFT_FILENAME: &str = "draft.md";

pub fn run(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<String>,
    cmd: AddCli,
) -> Result<()> {
    if cmd.draft && cmd.apply {
        anyhow::bail!("use either --draft or --apply (not both)");
    }
    if cmd.apply {
        return apply_draft(repo, notes_ref);
    }

    let file = cmd.file.context("missing --file <path>")?;
    if cmd.file_comment && cmd.line.is_some() {
        anyhow::bail!("use either --file-comment or --line (not both)");
    }

    let file_comment = if cmd.line.is_none() { true } else { cmd.file_comment };
    let mut side = cmd.side;
    if !file_comment && cmd.line.is_some() && side.is_none() {
        side = Some(LineSide::New);
    }

    if cmd.draft {
        return write_draft(
            repo,
            notes_ref,
            base_ref,
            &file,
            file_comment,
            cmd.line,
            side,
        );
    }

    let existing = load_file_review(repo, notes_ref, &file)?;
    let initial = existing
        .as_ref()
        .and_then(|f| {
            if file_comment {
                f.file_comment.as_ref().map(|c| c.body.clone())
            } else {
                f.comments
                    .get(&crate::review::LineKey {
                        side: side.unwrap_or(LineSide::New),
                        line: cmd.line.unwrap_or(1),
                    })
                    .map(|c| c.body.clone())
            }
        })
        .unwrap_or_default();

    let body = if cmd.edit {
        edit_comment(&initial, cmd.editor.as_deref())?
    } else if let Some(message) = cmd.message {
        message
    } else {
        anyhow::bail!("missing comment body (use --message or --edit)");
    };

    let mut review = Review::new();
    if let Some(file_review) = existing {
        review.files.insert(file.clone(), file_review);
    }

    if file_comment {
        review.set_file_comment(&file, body);
    } else {
        let line = cmd.line.context("missing --line <n>")?;
        let side = side.context("missing --side <old|new>")?;
        review.set_line_comment(&file, side, line, body);
    }

    persist_file_review(repo, notes_ref, &file, review.files.get(&file))?;
    Ok(())
}

fn load_file_review(
    repo: &gix::Repository,
    notes_ref: &str,
    path: &str,
) -> Result<Option<crate::review::FileReview>> {
    let head = crate::git::head_commit_oid(repo)?;
    let oid = crate::git::note_file_key_oid(repo, head, ViewKind::All, None, path)?;
    let note = crate::notes::read(repo, notes_ref, &oid)?;
    Ok(note
        .as_deref()
        .and_then(crate::review::decode_file_note))
}

fn persist_file_review(
    repo: &gix::Repository,
    notes_ref: &str,
    path: &str,
    file: Option<&crate::review::FileReview>,
) -> Result<()> {
    let head = crate::git::head_commit_oid(repo)?;
    let oid = crate::git::note_file_key_oid(repo, head, ViewKind::All, None, path)?;
    match file {
        Some(file)
            if !file.comments.is_empty() || file.file_comment.is_some() || file.reviewed =>
        {
            let note = crate::review::encode_file_note(file);
            crate::notes::write(repo, notes_ref, &oid, Some(&note))?;
        }
        _ => {
            crate::notes::write(repo, notes_ref, &oid, None)?;
        }
    }
    Ok(())
}

fn edit_comment(initial: &str, editor_override: Option<&str>) -> Result<String> {
    let file = tempfile::NamedTempFile::new().context("create temp file")?;
    std::fs::write(file.path(), initial).context("write initial comment")?;

    let editor = if let Some(editor) = editor_override {
        editor.to_string()
    } else {
        std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .context("set $VISUAL or $EDITOR to use --edit")?
    };

    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .context("editor command was empty")?;
    let mut cmd = Command::new(program);
    for part in parts {
        cmd.arg(part);
    }
    cmd.arg(file.path());

    let status = cmd.status().context("launch editor")?;
    if !status.success() {
        anyhow::bail!("editor exited with status {status}");
    }

    let content = std::fs::read_to_string(file.path()).context("read edited comment")?;
    Ok(content.trim_end().to_string())
}

#[derive(Debug, Serialize, Deserialize)]
struct DraftMeta {
    file: String,
    line: Option<u32>,
    side: Option<String>,
    file_comment: bool,
}

fn draft_path(repo: &gix::Repository) -> Result<std::path::PathBuf> {
    let gitdir = repo.path().to_path_buf();
    Ok(gitdir.join(DRAFT_DIR).join(DRAFT_FILENAME))
}

fn write_draft(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<String>,
    file: &str,
    file_comment: bool,
    line: Option<u32>,
    side: Option<LineSide>,
) -> Result<()> {
    let path = draft_path(repo)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create draft directory")?;
    }

    let existing = load_file_review(repo, notes_ref, file)?;
    let current_body = existing
        .as_ref()
        .and_then(|f| {
            if file_comment {
                f.file_comment.as_ref().map(|c| c.body.clone())
            } else {
                f.comments
                    .get(&LineKey {
                        side: side.unwrap_or(LineSide::New),
                        line: line.unwrap_or(1),
                    })
                    .map(|c| c.body.clone())
            }
        })
        .unwrap_or_default();

    let mut line_code = None;
    if !file_comment {
        let diff_context = prompt_diff_context(repo);
        let base_tree = base_ref
            .as_deref()
            .and_then(|b| crate::git::merge_base_tree(repo, b).ok());
        let view_order = prompt_view_order(base_ref.is_some());
        let mut resolver = LineCodeResolver::new(repo, base_tree, diff_context, view_order);
        if let (Some(line), Some(side)) = (line, side) {
            line_code = resolver.line_code(
                file,
                LineKey {
                    side,
                    line,
                },
            );
        }
    }

    let meta = DraftMeta {
        file: file.to_string(),
        line,
        side: side.map(|s| match s {
            LineSide::Old => "old".to_string(),
            LineSide::New => "new".to_string(),
        }),
        file_comment,
    };
    let draft = render_draft(&meta, &current_body, line_code.as_deref());
    std::fs::write(&path, draft).context("write draft file")?;
    println!("{}", path.display());
    Ok(())
}

fn apply_draft(repo: &gix::Repository, notes_ref: &str) -> Result<()> {
    let path = draft_path(repo)?;
    let content = std::fs::read_to_string(&path).context("read draft file")?;
    let (meta, body) = parse_draft(&content)?;

    let file_comment = meta.file_comment || meta.line.is_none();
    let side = match meta.side.as_deref() {
        Some("old") => Some(LineSide::Old),
        Some("new") => Some(LineSide::New),
        Some(other) => anyhow::bail!("invalid side '{other}' in draft"),
        None => None,
    };

    let mut review = Review::new();
    if let Some(file_review) = load_file_review(repo, notes_ref, &meta.file)? {
        review.files.insert(meta.file.clone(), file_review);
    }

    if file_comment {
        review.set_file_comment(&meta.file, body);
    } else {
        let line = meta.line.context("missing line in draft")?;
        let side = side.unwrap_or(LineSide::New);
        review.set_line_comment(&meta.file, side, line, body);
    }

    persist_file_review(repo, notes_ref, &meta.file, review.files.get(&meta.file))?;
    std::fs::remove_file(&path).ok();
    Ok(())
}

fn render_draft(meta: &DraftMeta, body: &str, line_code: Option<&str>) -> String {
    let json = serde_json::to_string(meta).unwrap_or_else(|_| "{}".to_string());
    let mut out = String::new();
    out.push_str(&format!("<!-- remark-draft:v1 {json} -->\n"));
    out.push_str("# Review Draft\n\n");
    out.push_str(&format!("File: {}\n", meta.file));
    if meta.file_comment || meta.line.is_none() {
        out.push_str("Target: file comment\n\n");
    } else {
        let side = meta.side.as_deref().unwrap_or("new");
        out.push_str(&format!(
            "Target: line {} ({side})\n\n",
            meta.line.unwrap_or(1)
        ));
    }
    if let Some(code) = line_code {
        push_fenced_block_with_lang(&mut out, code, "diff");
        out.push('\n');
    }
    push_fenced_block_with_lang(&mut out, body, "text");
    out
}

fn parse_draft(content: &str) -> Result<(DraftMeta, String)> {
    let meta_line = content
        .lines()
        .find(|line| line.trim_start().starts_with("<!-- remark-draft:v1"))
        .context("missing draft metadata")?;
    let json = meta_line
        .trim()
        .trim_start_matches("<!-- remark-draft:v1")
        .trim_end_matches("-->")
        .trim();
    let meta: DraftMeta =
        serde_json::from_str(json).context("parse draft metadata")?;

    let mut lines = content.lines();
    let mut fence = None;
    let mut body = Vec::new();
    while let Some(line) = lines.next() {
        if fence.is_none() && line.trim_start().starts_with("```text") {
            let ticks = line.chars().take_while(|c| *c == '`').count();
            fence = Some("`".repeat(ticks.max(3)));
            continue;
        }
        if let Some(f) = fence.as_ref() {
            if line.trim() == f.as_str() {
                break;
            }
            body.push(line);
        }
    }
    let body = body.join("\n").trim_end().to_string();
    Ok((meta, body))
}

fn push_fenced_block_with_lang(out: &mut String, text: &str, lang: &str) {
    let mut ticks = 3usize;
    loop {
        let fence = "`".repeat(ticks);
        if !text.contains(&fence) {
            out.push_str(&format!("{fence}{lang}\n"));
            out.push_str(text);
            if !text.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("{fence}\n"));
            return;
        }
        ticks += 1;
    }
}

fn prompt_view_order(include_base: bool) -> Vec<ViewKind> {
    let mut order = vec![ViewKind::All, ViewKind::Staged, ViewKind::Unstaged];
    if include_base {
        order.push(ViewKind::Base);
    }
    order
}

const DEFAULT_DIFF_CONTEXT: u32 = 3;
const MIN_DIFF_CONTEXT: u32 = 0;
const MAX_DIFF_CONTEXT: u32 = 20;

fn prompt_diff_context(repo: &gix::Repository) -> u32 {
    let raw = crate::git::read_local_config_value(repo, "remark.diffContext")
        .ok()
        .flatten();
    raw.and_then(|v| v.trim().parse::<u32>().ok())
        .map(|v| v.clamp(MIN_DIFF_CONTEXT, MAX_DIFF_CONTEXT))
        .unwrap_or(DEFAULT_DIFF_CONTEXT)
}
