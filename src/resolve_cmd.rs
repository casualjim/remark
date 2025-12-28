use anyhow::{Context, Result};

use crate::git::ViewKind;
use crate::review::{LineKey, LineSide};

const DEFAULT_NOTES_REF: &str = "refs/notes/remark";

pub fn run() -> Result<()> {
    let repo = gix::discover(std::env::current_dir().context("get current directory")?)
        .context("discover git repository")?;

    // `remark resolve --file <path> [--line <n> --side old|new] [--file-comment] [--unresolve] [--base <ref>] [--ref <notes-ref>]`
    let mut notes_ref = DEFAULT_NOTES_REF.to_string();
    let mut base_ref: Option<String> = None;
    let mut file: Option<String> = None;
    let mut line: Option<u32> = None;
    let mut side: Option<LineSide> = None;
    let mut file_comment = false;
    let mut unresolve = false;

    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ref" => {
                if let Some(v) = args.next() {
                    notes_ref = v;
                }
            }
            "--base" => {
                if let Some(v) = args.next() {
                    base_ref = Some(v);
                }
            }
            "--file" => {
                if let Some(v) = args.next() {
                    file = Some(v);
                }
            }
            "--line" => {
                if let Some(v) = args.next() {
                    line = Some(v.parse::<u32>().context("parse --line")?);
                }
            }
            "--side" => {
                if let Some(v) = args.next() {
                    side = match v.as_str() {
                        "old" => Some(LineSide::Old),
                        "new" => Some(LineSide::New),
                        _ => None,
                    };
                }
            }
            "--file-comment" => {
                file_comment = true;
            }
            "--unresolve" => {
                unresolve = true;
            }
            "-h" | "--help" => {
                print_help_and_exit();
            }
            _ => {}
        }
    }

    let file = file.context("missing --file <path>")?;
    if file_comment && line.is_some() {
        anyhow::bail!("use either --file-comment or --line (not both)");
    }

    let resolve_file_comment = if line.is_none() { true } else { file_comment };
    if !resolve_file_comment && line.is_some() && side.is_none() {
        side = Some(LineSide::New);
    }

    let line_key = if resolve_file_comment {
        None
    } else {
        Some(LineKey {
            side: side.context("missing --side <old|new>")?,
            line: line.context("missing --line <n>")?,
        })
    };

    let head = crate::git::head_commit_oid(&repo)?;
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
        let oid = crate::git::note_file_key_oid(&repo, head, view, base_for_key, &file)
            .context("compute note key")?;
        let note = crate::notes::read(&repo, &notes_ref, &oid).context("read file note")?;
        let Some(note) = note.as_deref() else {
            continue;
        };
        let Some(mut fr) = crate::review::decode_file_note(note) else {
            continue;
        };

        let mut changed = false;
        if let Some(key) = &line_key {
            if let Some(c) = fr.comments.get_mut(key)
                && c.resolved == unresolve
            {
                c.resolved = !unresolve;
                changed = true;
            }
        } else if let Some(c) = fr.file_comment.as_mut()
            && c.resolved == unresolve
        {
            c.resolved = !unresolve;
            changed = true;
        }

        if !changed {
            continue;
        }

        if fr.file_comment.is_none() && fr.comments.is_empty() {
            crate::notes::write(&repo, &notes_ref, &oid, None)?;
        } else {
            let note = crate::review::encode_file_note(&fr);
            crate::notes::write(&repo, &notes_ref, &oid, Some(&note))?;
        }
        changed_any = true;
    }

    if !changed_any {
        anyhow::bail!("no matching comment found to resolve");
    }

    Ok(())
}

fn print_help_and_exit() -> ! {
    eprintln!(
        "remark resolve\n\nUSAGE:\n  remark resolve --file <path> [--line <n> --side old|new] [--file-comment] [--unresolve] [--base <ref>] [--ref <notes-ref>]\n\nOPTIONS:\n  --file <path>       File to resolve a comment on\n  --line <n>          Line number (1-based)\n  --side <old|new>    Which side for line comments (default: new)\n  --file-comment      Resolve the file-level comment\n  --unresolve         Mark comment as unresolved\n  --base <ref>        Include base-view notes when present\n  --ref <notes-ref>   Notes ref to read (default: {DEFAULT_NOTES_REF})\n"
    );
    std::process::exit(2);
}
