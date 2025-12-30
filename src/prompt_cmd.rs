use anyhow::{Context, Result};

use crate::git::ViewKind;
use crate::review::{FileReview, Review};

const DEFAULT_NOTES_REF: &str = "refs/notes/remark";

pub fn run() -> Result<()> {
    let repo = gix::discover(std::env::current_dir().context("get current directory")?)
        .context("discover git repository")?;

    // `remark prompt [--ref <notes-ref>] [--filter all|staged|unstaged|base] [--base <ref>] [--copy]`
    let mut notes_ref = DEFAULT_NOTES_REF.to_string();
    let mut filter = Filter::All;
    let mut base_ref: Option<String> = None;
    let mut copy = false;

    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ref" => {
                if let Some(v) = args.next() {
                    notes_ref = v;
                }
            }
            "--filter" => {
                if let Some(v) = args.next() {
                    filter = match v.as_str() {
                        "all" => Filter::All,
                        "unstaged" => Filter::Unstaged,
                        "staged" => Filter::Staged,
                        "base" => Filter::Base,
                        _ => Filter::All,
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

    let mut paths = match filter {
        Filter::All => {
            let mut staged = crate::git::list_staged_paths(&repo)?;
            staged.extend(crate::git::list_unstaged_paths(&repo, false)?);
            staged
        }
        Filter::Unstaged => crate::git::list_unstaged_paths(&repo, false)?,
        Filter::Staged => crate::git::list_staged_paths(&repo)?,
        Filter::Base => {
            let Some(base) = base_ref.clone() else {
                anyhow::bail!("--filter base requires --base <ref>")
            };
            crate::git::list_base_paths(&repo, &base)?
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
            let oid = crate::git::note_file_key_oid(&repo, head, *view, base_for_key, &path)
                .with_context(|| format!("compute note key for '{path}'"))?;
            let note = crate::notes::read(&repo, &notes_ref, &oid)
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

#[derive(Debug, Clone, Copy)]
enum Filter {
    All,
    Staged,
    Unstaged,
    Base,
}

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

fn print_help_and_exit() -> ! {
    eprintln!(
        "remark prompt\n\nUSAGE:\n  remark prompt [--filter all|staged|unstaged|base] [--base <ref>] [--ref <notes-ref>] [--copy]\n\nOPTIONS:\n  --filter <f>        Filter files (default: all)\n  --base <ref>        Base ref when --filter base\n  --ref <notes-ref>   Notes ref to read (default: {DEFAULT_NOTES_REF})\n  --copy, -c          Copy prompt to clipboard\n"
    );
    std::process::exit(2);
}
