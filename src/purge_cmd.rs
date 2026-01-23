use anyhow::Result;
use gix::Repository;

pub fn run(repo: &Repository, confirm: bool) -> Result<()> {
  if !confirm {
    anyhow::bail!("refusing to purge without --yes");
  }

  let refs = collect_remark_refs(repo)?;
  if refs.is_empty() {
    println!("No remark notes refs found.");
    return Ok(());
  }

  let mut deleted = 0usize;
  for name in refs {
    if let Some(r) = repo.try_find_reference(&name)? {
      r.delete()?;
      deleted += 1;
    }
  }

  let mut reset_config = false;
  let configured = crate::git::read_notes_ref(repo);
  if is_remark_ref_name(configured.as_bytes()) && configured != crate::git::DEFAULT_NOTES_REF {
    crate::git::write_local_config_value(
      repo,
      crate::git::CONFIG_NOTES_REF_KEY,
      crate::git::DEFAULT_NOTES_REF,
    )?;
    reset_config = true;
  }

  println!("Deleted {deleted} remark notes ref(s).");
  if reset_config {
    println!(
      "Reset default notes ref to {}.",
      crate::git::DEFAULT_NOTES_REF
    );
  }
  Ok(())
}

fn collect_remark_refs(repo: &Repository) -> Result<Vec<String>> {
  let mut out = Vec::new();
  let refs = repo.references()?;
  let iter = refs.prefixed("refs/notes/")?;
  for reference in iter {
    let reference = reference.map_err(|e| anyhow::anyhow!(e))?;
    let name = reference.name().as_bstr().as_ref();
    if is_remark_ref_name(name) {
      out.push(String::from_utf8_lossy(name).to_string());
    }
  }
  out.sort();
  out.dedup();
  Ok(out)
}

fn is_remark_ref_name(name: &[u8]) -> bool {
  name == crate::git::DEFAULT_NOTES_REF.as_bytes()
    || name.starts_with(b"refs/notes/remark-")
    || name.starts_with(b"refs/notes/remark.")
    || name.starts_with(b"refs/notes/remark/")
}
