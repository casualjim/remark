use anyhow::{Context, Result};

use crate::git::ViewKind;
use crate::review::Review;

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

    let head = crate::git::head_commit_oid(&repo)?;

    let (kind, base_for_review, base_for_key, mut paths) = match view {
        ViewKind::All => {
            let mut staged = crate::git::list_staged_paths(&repo)?;
            staged.extend(crate::git::list_unstaged_paths(&repo)?);
            ("all".to_string(), None, None, staged)
        }
        ViewKind::Unstaged => (
            "unstaged".to_string(),
            None,
            None,
            crate::git::list_unstaged_paths(&repo)?,
        ),
        ViewKind::Staged => (
            "staged".to_string(),
            None,
            None,
            crate::git::list_staged_paths(&repo)?,
        ),
        ViewKind::Base => {
            let Some(base) = base_ref.clone() else {
                anyhow::bail!("base view requires --base <ref> (or an upstream/main/master)")
            };
            (
                "base".to_string(),
                Some(base.clone()),
                Some(base.clone()),
                crate::git::list_base_paths(&repo, &base)?,
            )
        }
    };

    paths.sort();
    paths.dedup();

    let mut review = Review::new(kind, base_for_review);
    for path in paths {
        let oid = crate::git::note_file_key_oid(&repo, head, view, base_for_key.as_deref(), &path)
            .with_context(|| format!("compute note key for '{path}'"))?;
        let note = crate::notes::read(&repo, &notes_ref, &oid)
            .with_context(|| format!("read note for '{path}'"))?;
        let Some(note) = note.as_deref() else {
            continue;
        };
        let Some(file) = crate::review::decode_file_note(note) else {
            continue;
        };
        if file.file_comment.is_some() || !file.comments.is_empty() {
            review.files.insert(path, file);
        }
    }

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
        "remark prompt\n\nUSAGE:\n  remark prompt [--view all|unstaged|staged|base] [--base <ref>] [--ref <notes-ref>] [--copy]\n\nOPTIONS:\n  --view <v>          Which view to render (default: all)\n  --base <ref>        Base ref when --view base (default: @{{upstream}} / main / master)\n  --ref <notes-ref>   Notes ref to read (default: {DEFAULT_NOTES_REF})\n  --copy, -c          Copy prompt to clipboard\n"
    );
    std::process::exit(2);
}
