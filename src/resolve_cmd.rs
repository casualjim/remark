use anyhow::{Context, Result};

use crate::config::ResolveCli;
use crate::git::ViewKind;
use crate::review::{LineKey, LineSide};

pub fn run(
  repo: &gix::Repository,
  notes_ref: &str,
  base_ref: Option<String>,
  cmd: ResolveCli,
) -> Result<()> {
  let file = cmd.file.context("missing --file <path>")?;
  let file = crate::git::normalize_repo_path(repo, &file);
  if cmd.file_comment && cmd.line.is_some() {
    anyhow::bail!("use either --file-comment or --line (not both)");
  }

  let resolve_file_comment = if cmd.line.is_none() {
    true
  } else {
    cmd.file_comment
  };
  let mut side = cmd.side;
  if !resolve_file_comment && cmd.line.is_some() && side.is_none() {
    side = Some(LineSide::New);
  }

  let line_key = if resolve_file_comment {
    None
  } else {
    Some(LineKey {
      side: side.context("missing --side <old|new>")?,
      line: cmd.line.context("missing --line <n>")?,
    })
  };

  let head = crate::git::head_commit_oid(repo)?;
  let mut views = vec![ViewKind::All, ViewKind::Staged, ViewKind::Unstaged];
  if base_ref.is_some() {
    views.push(ViewKind::Base);
  }

  let mut changed_any = false;
  for view in views {
    let base_for_key = match view {
      ViewKind::Base => base_ref.as_deref(),
      _ => None,
    };
    let oid = crate::git::note_file_key_oid(repo, head, view, base_for_key, &file)
      .context("compute note key")?;
    let note = crate::notes::read(repo, notes_ref, &oid).context("read file note")?;
    let Some(note) = note.as_deref() else {
      continue;
    };
    let Some(mut fr) = crate::review::decode_file_note(note) else {
      continue;
    };

    let mut changed = false;
    if let Some(key) = &line_key {
      if let Some(c) = fr.comments.get_mut(key)
        && c.resolved == cmd.unresolve
      {
        c.resolved = !cmd.unresolve;
        changed = true;
      }
    } else if let Some(c) = fr.file_comment.as_mut()
      && c.resolved == cmd.unresolve
    {
      c.resolved = !cmd.unresolve;
      changed = true;
    }

    if !changed {
      continue;
    }

    if fr.file_comment.is_none() && fr.comments.is_empty() {
      crate::notes::write(repo, notes_ref, &oid, None)?;
    } else {
      let note = crate::review::encode_file_note(&fr);
      crate::notes::write(repo, notes_ref, &oid, Some(&note))?;
    }
    changed_any = true;
  }

  if !changed_any {
    anyhow::bail!("no matching comment found to resolve");
  }

  crate::add_cmd::sync_draft_notes(repo, notes_ref, base_ref.as_deref())?;
  Ok(())
}
