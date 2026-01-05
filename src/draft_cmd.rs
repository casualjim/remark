use anyhow::Result;

use crate::config::DraftCli;

pub fn run(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<String>,
    _cmd: DraftCli,
) -> Result<()> {
    if crate::git::head_commit_oid(repo).is_ok() {
        crate::add_cmd::sync_draft_notes(repo, notes_ref, base_ref.as_deref())?;
    } else {
        crate::add_cmd::ensure_draft_exists(repo, notes_ref, base_ref.as_deref())?;
    }

    let path = crate::add_cmd::draft_path(repo)?;
    println!("{}", path.display());
    Ok(())
}
