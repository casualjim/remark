use anyhow::{Context, Result};

use crate::config::PromptFilter;
use crate::git::ViewKind;
use crate::prompt_code::LineCodeResolver;
use crate::review::{FileReview, Review};

pub fn run(
    repo: &gix::Repository,
    notes_ref: &str,
    filter: PromptFilter,
    base_ref: Option<String>,
) -> Result<()> {
    let head = crate::git::head_commit_oid(repo)?;

    let mut paths = match filter {
        PromptFilter::All => {
            let mut staged = crate::git::list_staged_paths(repo)?;
            staged.extend(crate::git::list_unstaged_paths(repo, false)?);
            staged
        }
        PromptFilter::Unstaged => crate::git::list_unstaged_paths(repo, false)?,
        PromptFilter::Staged => crate::git::list_staged_paths(repo)?,
        PromptFilter::Base => {
            let Some(base) = base_ref.clone() else {
                anyhow::bail!("--filter base requires --base <ref>")
            };
            crate::git::list_base_paths(repo, &base)?
        }
    };

    paths.sort();
    paths.dedup();

    let mut review = Review::new();
    let mut views_to_scan = vec![ViewKind::All, ViewKind::Staged, ViewKind::Unstaged];
    if base_ref.is_some() {
        views_to_scan.push(ViewKind::Base);
    }

    for path in paths {
        let mut merged: Option<FileReview> = None;
        for view in &views_to_scan {
            let base_for_key = match view {
                ViewKind::Base => base_ref.as_deref(),
                _ => None,
            };
            let oid = crate::git::note_file_key_oid(repo, head, *view, base_for_key, &path)
                .with_context(|| format!("compute note key for '{path}'"))?;
            let note = crate::notes::read(repo, notes_ref, &oid)
                .with_context(|| format!("read note for '{path}'"))?;
            let Some(note) = note.as_deref() else {
                continue;
            };
            let Some(file) = crate::review::decode_file_note(note) else {
                continue;
            };
            merged = Some(match merged {
                None => file,
                Some(mut existing) => {
                    merge_file_review(&mut existing, file);
                    existing
                }
            });
        }
        if let Some(file) = merged
            && (file.file_comment.is_some() || !file.comments.is_empty())
        {
            review.files.insert(path, file);
        }
    }

    let diff_context = prompt_diff_context(repo);
    let view_order = prompt_view_order(filter, base_ref.is_some());
    let base_tree = base_ref
        .as_deref()
        .and_then(|b| crate::git::merge_base_tree(repo, b).ok());
    let mut resolver = LineCodeResolver::new(repo, base_tree, diff_context, view_order);

    let prompt = crate::review::render_prompt(&review, |path, key| resolver.line_code(path, key));
    print!("{prompt}");
    if !prompt.ends_with('\n') {
        println!();
    }
    Ok(())
}

const DEFAULT_DIFF_CONTEXT: u32 = 3;
const MIN_DIFF_CONTEXT: u32 = 0;
const MAX_DIFF_CONTEXT: u32 = 20;

fn merge_file_review(target: &mut FileReview, incoming: FileReview) {
    match (&target.file_comment, &incoming.file_comment) {
        (None, Some(_)) => target.file_comment = incoming.file_comment,
        (Some(t), Some(i)) if t.resolved && !i.resolved => {
            target.file_comment = incoming.file_comment
        }
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

fn prompt_view_order(filter: PromptFilter, include_base: bool) -> Vec<ViewKind> {
    let mut order = match filter {
        PromptFilter::All => vec![ViewKind::All, ViewKind::Staged, ViewKind::Unstaged],
        PromptFilter::Staged => vec![ViewKind::Staged, ViewKind::All, ViewKind::Unstaged],
        PromptFilter::Unstaged => vec![ViewKind::Unstaged, ViewKind::All, ViewKind::Staged],
        PromptFilter::Base => vec![
            ViewKind::Base,
            ViewKind::All,
            ViewKind::Staged,
            ViewKind::Unstaged,
        ],
    };
    if include_base && !order.contains(&ViewKind::Base) {
        order.push(ViewKind::Base);
    }
    if !include_base {
        order.retain(|v| *v != ViewKind::Base);
    }
    order
}

fn prompt_diff_context(repo: &gix::Repository) -> u32 {
    let raw = crate::git::read_local_config_value(repo, "remark.diffContext")
        .ok()
        .flatten();
    raw.and_then(|v| v.trim().parse::<u32>().ok())
        .map(|v| v.clamp(MIN_DIFF_CONTEXT, MAX_DIFF_CONTEXT))
        .unwrap_or(DEFAULT_DIFF_CONTEXT)
}
