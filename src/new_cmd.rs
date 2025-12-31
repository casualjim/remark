use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use gix::Repository;

pub fn run() -> Result<()> {
    let repo = gix::discover(std::env::current_dir().context("get current directory")?)
        .context("discover git repository")?;

    // `remark new [--ref <notes-ref>]`
    let mut notes_ref: Option<String> = None;
    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ref" => {
                if let Some(v) = args.next() {
                    notes_ref = Some(v);
                }
            }
            "-h" | "--help" => {
                print_help_and_exit();
            }
            _ => {}
        }
    }

    let notes_ref = match notes_ref {
        Some(r) => r,
        None => generate_notes_ref(&repo)?,
    };

    crate::git::write_local_config_value(&repo, crate::git::CONFIG_NOTES_REF_KEY, &notes_ref)?;
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

fn print_help_and_exit() -> ! {
    eprintln!(
        "remark new\n\nUSAGE:\n  remark new [--ref <notes-ref>]\n\nOPTIONS:\n  --ref <notes-ref>   Set an explicit notes ref (default: {default_notes_ref}-<epoch>)\n",
        default_notes_ref = crate::git::DEFAULT_NOTES_REF
    );
    std::process::exit(2);
}
