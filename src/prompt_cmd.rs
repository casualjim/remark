use anyhow::{Context, Result};

use crate::git::ViewKind;

const DEFAULT_NOTES_REF: &str = "refs/notes/remark";

pub fn run() -> Result<()> {
    let repo = gix::discover(std::env::current_dir().context("get current directory")?)
        .context("discover git repository")?;

    // `remark prompt [--ref <notes-ref>] [--view all|unstaged|staged|base] [--base <ref>] [--copy]`
    let mut notes_ref = DEFAULT_NOTES_REF.to_string();
    let mut view = ViewKind::All;
    let mut base_ref: Option<String> = crate::git::default_base_ref(&repo);
    let mut copy = false;

    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ref" => {
                if let Some(v) = args.next() {
                    notes_ref = v;
                }
            }
            "--view" => {
                if let Some(v) = args.next() {
                    view = match v.as_str() {
                        "all" => ViewKind::All,
                        "unstaged" => ViewKind::Unstaged,
                        "staged" => ViewKind::Staged,
                        "base" => ViewKind::Base,
                        _ => ViewKind::All,
                    };
                }
            }
            "--base" => {
                if let Some(v) = args.next() {
                    base_ref = Some(v);
                }
            }
            "--copy" | "-c" => {
                copy = true;
            }
            "-h" | "--help" => {
                print_help_and_exit();
            }
            _ => {}
        }
    }

    let (oid, kind, base_for_review) = match view {
        ViewKind::All => (
            crate::git::note_key_oid(&repo, ViewKind::All, None)?,
            "all".to_string(),
            None,
        ),
        ViewKind::Unstaged => (
            crate::git::note_key_oid(&repo, ViewKind::Unstaged, None)?,
            "unstaged".to_string(),
            None,
        ),
        ViewKind::Staged => (
            crate::git::note_key_oid(&repo, ViewKind::Staged, None)?,
            "staged".to_string(),
            None,
        ),
        ViewKind::Base => {
            let Some(base) = base_ref.clone() else {
                anyhow::bail!("base view requires --base <ref> (or an upstream/main/master)")
            };
            (
                crate::git::note_key_oid(&repo, ViewKind::Base, Some(&base))?,
                "base".to_string(),
                Some(base),
            )
        }
    };

    let note = crate::notes::read(&repo, &notes_ref, &oid).context("read review note")?;

    let review = note
        .as_deref()
        .and_then(crate::review::decode_note)
        .unwrap_or_else(|| crate::review::Review::new(kind, base_for_review));

    let prompt = crate::review::render_prompt(&review);
    if copy {
        let method = crate::clipboard::copy(&prompt)?;
        eprintln!("Copied prompt to clipboard ({method})");
    } else {
        print!("{prompt}");
        if !prompt.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

fn print_help_and_exit() -> ! {
    eprintln!(
        "remark prompt\n\nUSAGE:\n  remark prompt [--view all|unstaged|staged|base] [--base <ref>] [--ref <notes-ref>] [--copy]\n\nOPTIONS:\n  --view <v>          Which review note to render (default: all)\n  --base <ref>        Base ref when --view base (default: @{{upstream}} / main / master)\n  --ref <notes-ref>   Notes ref to read (default: {DEFAULT_NOTES_REF})\n  --copy, -c          Copy prompt to clipboard\n"
    );
    std::process::exit(2);
}
