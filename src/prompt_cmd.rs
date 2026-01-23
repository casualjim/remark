use anyhow::Result;

use crate::config::PromptFilter;
use crate::git::ViewKind;
use crate::prompt_code::{LineSnippetResolver, language_for_path};
use crate::review::PromptSnippet;

pub fn run(
  repo: &gix::Repository,
  notes_ref: &str,
  filter: PromptFilter,
  base_ref: Option<String>,
) -> Result<()> {
  let head = crate::git::head_commit_oid(repo).ok();

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

  if head.is_some() {
    crate::add_cmd::sync_draft_notes(repo, notes_ref, base_ref.as_deref())?;
  }

  let mut review = crate::add_cmd::load_review_from_draft(repo, notes_ref, base_ref.as_deref())?;
  let path_set: std::collections::HashSet<_> = paths.iter().cloned().collect();
  review.files.retain(|path, _| path_set.contains(path));

  let diff_context = prompt_diff_context(repo);
  let view_order = prompt_view_order(filter, base_ref.is_some());
  let base_tree = base_ref
    .as_deref()
    .and_then(|b| crate::git::merge_base_tree(repo, b).ok());
  let mut resolver = LineSnippetResolver::new(repo, base_tree, diff_context, view_order);

  let prompt = crate::review::render_prompt(&review, |path, key| {
    resolver.snippet(path, key).map(|code| PromptSnippet {
      code,
      lang: language_for_path(path),
    })
  });
  print!("{prompt}");
  if !prompt.ends_with('\n') {
    println!();
  }
  Ok(())
}

const DEFAULT_DIFF_CONTEXT: u32 = 3;
const MIN_DIFF_CONTEXT: u32 = 0;
const MAX_DIFF_CONTEXT: u32 = 20;

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
  raw
    .and_then(|v| v.trim().parse::<u32>().ok())
    .map(|v| v.clamp(MIN_DIFF_CONTEXT, MAX_DIFF_CONTEXT))
    .unwrap_or(DEFAULT_DIFF_CONTEXT)
}
