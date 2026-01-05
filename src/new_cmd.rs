use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use gix::Repository;

pub fn run(repo: &Repository, notes_ref: Option<String>) -> Result<()> {
    let notes_ref = match notes_ref {
        Some(r) => r,
        None => generate_notes_ref(repo)?,
    };

    crate::git::write_local_config_value(repo, crate::git::CONFIG_NOTES_REF_KEY, &notes_ref)?;
    println!("Set default notes ref to {notes_ref}");
    Ok(())
}

fn generate_notes_ref(repo: &Repository) -> Result<String> {
    let base = crate::git::DEFAULT_NOTES_REF;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut candidate = format!("{base}-{ts}");
    let mut suffix = 1;
    while repo.try_find_reference(&candidate)?.is_some() {
        candidate = format!("{base}-{ts}-{suffix}");
        suffix += 1;
    }
    Ok(candidate)
}
