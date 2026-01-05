use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::process::Command;

use anyhow::{Context, Result};
use gix::bstr::ByteSlice;
use gix_hash::{Kind, hasher};
use serde::{Deserialize, Serialize};

use crate::config::AddCli;
use crate::git::ViewKind;
use crate::prompt_code::{LineSnippetResolver, language_for_path};
use crate::review::{LineKey, LineSide, PromptSnippet, Review};

const DRAFT_DIR: &str = "remark";
const DRAFT_FILENAME: &str = "draft.md";
const DRAFT_PLACEHOLDER: &str = "<!-- remark:write comment -->";
// Drafts are stored as prompt-formatted markdown in `.git/remark/draft.md`.

pub fn run(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<String>,
    cmd: AddCli,
) -> Result<()> {
    if cmd.draft && cmd.apply {
        anyhow::bail!("use either --draft or --apply (not both)");
    }
    if cmd.apply {
        let report = apply_draft(repo, notes_ref, base_ref.as_deref(), true)?;
        if !report.skipped.is_empty() {
            for skip in report.skipped {
                eprintln!(
                    "remark: skipped {}:{} ({}) - {}",
                    skip.file,
                    skip.line,
                    side_label(skip.side),
                    skip.reason
                );
            }
        }
        return Ok(());
    }

    let file = cmd.file.context("missing --file <path>")?;
    let file = crate::git::normalize_repo_path(repo, &file);
    if cmd.file_comment && cmd.line.is_some() {
        anyhow::bail!("use either --file-comment or --line (not both)");
    }

    let file_comment = if cmd.line.is_none() {
        true
    } else {
        cmd.file_comment
    };
    let mut side = cmd.side;
    if !file_comment && cmd.line.is_some() && side.is_none() {
        side = Some(LineSide::New);
    }

    if cmd.draft {
        return write_draft(
            repo,
            notes_ref,
            base_ref.as_deref(),
            DraftWriteArgs {
                file: &file,
                file_comment,
                line: cmd.line,
                side,
                print_path: cmd.print_path,
            },
        );
    }

    let existing = load_file_review(repo, notes_ref, &file)?;
    let initial = existing
        .as_ref()
        .and_then(|f| {
            if file_comment {
                f.file_comment.as_ref().map(|c| c.body.clone())
            } else {
                f.comments
                    .get(&crate::review::LineKey {
                        side: side.unwrap_or(LineSide::New),
                        line: cmd.line.unwrap_or(1),
                    })
                    .map(|c| c.body.clone())
            }
        })
        .unwrap_or_default();

    let body = if cmd.edit {
        edit_comment(&initial, cmd.editor.as_deref())?
    } else if let Some(message) = cmd.message {
        message
    } else {
        anyhow::bail!("missing comment body (use --message or --edit)");
    };

    let mut review = Review::new();
    if let Some(file_review) = existing {
        review.files.insert(file.clone(), file_review);
    }

    if file_comment {
        review.set_file_comment(&file, body);
    } else {
        let line = cmd.line.context("missing --line <n>")?;
        let side = side.context("missing --side <old|new>")?;
        review.set_line_comment(&file, side, line, body);
        if let Some(hash) = current_snippet_hash(
            repo,
            base_ref.as_deref(),
            &file,
            LineKey { side, line },
        ) {
            review.set_line_comment_snippet_hash(&file, side, line, Some(hash));
        }
    }

    persist_file_review(repo, notes_ref, &file, review.files.get(&file))?;
    sync_draft_notes(repo, notes_ref, base_ref.as_deref())?;
    Ok(())
}

fn load_file_review(
    repo: &gix::Repository,
    notes_ref: &str,
    path: &str,
) -> Result<Option<crate::review::FileReview>> {
    let head = crate::git::head_commit_oid(repo)?;
    let oid = crate::git::note_file_key_oid(repo, head, ViewKind::All, None, path)?;
    let note = crate::notes::read(repo, notes_ref, &oid)?;
    Ok(note.as_deref().and_then(crate::review::decode_file_note))
}

fn persist_file_review(
    repo: &gix::Repository,
    notes_ref: &str,
    path: &str,
    file: Option<&crate::review::FileReview>,
) -> Result<()> {
    let head = crate::git::head_commit_oid(repo)?;
    let oid = crate::git::note_file_key_oid(repo, head, ViewKind::All, None, path)?;
    match file {
        Some(file) if !file.comments.is_empty() || file.file_comment.is_some() || file.reviewed => {
            let note = crate::review::encode_file_note(file);
            crate::notes::write(repo, notes_ref, &oid, Some(&note))?;
        }
        _ => {
            crate::notes::write(repo, notes_ref, &oid, None)?;
        }
    }
    Ok(())
}

fn edit_comment(initial: &str, editor_override: Option<&str>) -> Result<String> {
    let file = tempfile::NamedTempFile::new().context("create temp file")?;
    std::fs::write(file.path(), initial).context("write initial comment")?;

    let editor = if let Some(editor) = editor_override {
        editor.to_string()
    } else {
        std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .context("set $VISUAL or $EDITOR to use --edit")?
    };

    let mut parts = editor.split_whitespace();
    let program = parts.next().context("editor command was empty")?;
    let mut cmd = Command::new(program);
    for part in parts {
        cmd.arg(part);
    }
    cmd.arg(file.path());

    let status = cmd.status().context("launch editor")?;
    if !status.success() {
        anyhow::bail!("editor exited with status {status}");
    }

    let content = std::fs::read_to_string(file.path()).context("read edited comment")?;
    Ok(content.trim_end().to_string())
}

#[derive(Clone, Default)]
struct DraftReview {
    files: BTreeMap<String, DraftFileReview>,
}

#[derive(Clone, Default)]
struct DraftFileReview {
    file_comment: Option<String>,
    comments: BTreeMap<LineKey, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DraftMeta {
    notes_ref_oid: Option<String>,
    notes_ref_time: Option<i64>,
    draft_mtime: Option<i64>,
    draft_hash: Option<String>,
    lines: Vec<DraftLineMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DraftLineMeta {
    file: String,
    side: String,
    line: u32,
    hash: String,
}

impl DraftReview {
    fn file_comment(&self, path: &str) -> Option<&str> {
        self.files.get(path).and_then(|f| f.file_comment.as_deref())
    }

    fn line_comment(&self, path: &str, side: LineSide, line: u32) -> Option<&str> {
        self.files
            .get(path)
            .and_then(|f| f.comments.get(&LineKey { side, line }).map(|c| c.as_str()))
    }

    fn set_file_comment(&mut self, path: &str, body: String) {
        let f = self.files.entry(path.to_string()).or_default();
        f.file_comment = Some(body);
    }

    fn set_line_comment(&mut self, path: &str, side: LineSide, line: u32, body: String) {
        let f = self.files.entry(path.to_string()).or_default();
        f.comments.insert(LineKey { side, line }, body);
    }

    fn remove_file_comment(&mut self, path: &str) -> bool {
        let Some(f) = self.files.get_mut(path) else {
            return false;
        };
        let removed = f.file_comment.take().is_some();
        if f.file_comment.is_none() && f.comments.is_empty() {
            self.files.remove(path);
        }
        removed
    }

    fn remove_line_comment(&mut self, path: &str, side: LineSide, line: u32) -> bool {
        let Some(f) = self.files.get_mut(path) else {
            return false;
        };
        let removed = f.comments.remove(&LineKey { side, line }).is_some();
        if f.file_comment.is_none() && f.comments.is_empty() {
            self.files.remove(path);
        }
        removed
    }
}

pub(crate) fn draft_path(repo: &gix::Repository) -> Result<std::path::PathBuf> {
    let gitdir = repo.path().to_path_buf();
    Ok(gitdir.join(DRAFT_DIR).join(DRAFT_FILENAME))
}

fn draft_meta_path(repo: &gix::Repository) -> Result<std::path::PathBuf> {
    let gitdir = repo.path().to_path_buf();
    Ok(gitdir.join(DRAFT_DIR).join("draft.meta.json"))
}

pub(crate) fn ensure_draft_exists(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<&str>,
) -> Result<()> {
    let path = draft_path(repo)?;
    if path.exists() {
        let meta_path = draft_meta_path(repo)?;
        let needs_meta = match std::fs::read_to_string(&meta_path) {
            Ok(content) => {
                let trimmed = content.trim();
                trimmed.is_empty() || serde_json::from_str::<DraftMeta>(trimmed).is_err()
            }
            Err(_) => true,
        };
        if needs_meta {
            rebuild_draft_meta(repo, notes_ref, base_ref, &load_draft_review(&path)?, None).ok();
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create draft directory")?;
    }
    let draft = render_prompt_draft(repo, None, &DraftReview::default());
    std::fs::write(&path, draft).context("write draft file")?;
    let meta_path = draft_meta_path(repo)?;
    if let Some(parent) = meta_path.parent() {
        std::fs::create_dir_all(parent).context("create draft directory")?;
    }
    let draft_hash = draft_content_hash(&std::fs::read_to_string(&path).unwrap_or_default())?;
    let meta = DraftMeta {
        notes_ref_oid: None,
        notes_ref_time: None,
        draft_mtime: draft_mtime(&path),
        draft_hash: Some(draft_hash),
        lines: Vec::new(),
    };
    write_draft_meta(&meta_path, &meta)?;
    Ok(())
}

pub(crate) fn remove_from_draft(
    repo: &gix::Repository,
    base_ref: Option<&str>,
    file: &str,
    line: Option<u32>,
    side: Option<LineSide>,
    file_comment: bool,
) -> Result<()> {
    let path = draft_path(repo)?;
    if !path.exists() {
        return Ok(());
    }
    let mut draft_review = load_draft_review(&path)?;
    let mut touched = false;
    if file_comment {
        if draft_review.remove_file_comment(file) {
            touched = true;
        }
    } else if let (Some(line), Some(side)) = (line, side)
        && draft_review.remove_line_comment(file, side, line)
    {
        touched = true;
    }

    if touched {
        let draft = render_prompt_draft(repo, base_ref, &draft_review);
        std::fs::write(&path, draft).context("write draft file")?;
        rebuild_draft_meta(
            repo,
            &crate::git::read_notes_ref(repo),
            base_ref,
            &draft_review,
            None,
        )?;
    }
    Ok(())
}

struct DraftWriteArgs<'a> {
    file: &'a str,
    file_comment: bool,
    line: Option<u32>,
    side: Option<LineSide>,
    print_path: bool,
}

fn write_draft(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<&str>,
    args: DraftWriteArgs<'_>,
) -> Result<()> {
    let path = draft_path(repo)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create draft directory")?;
    }

    let mut draft_review = load_draft_review(&path)?;
    let existing = load_file_review(repo, notes_ref, args.file)?;
    let line = if args.file_comment {
        None
    } else {
        Some(args.line.context("missing --line <n>")?)
    };
    let side = if args.file_comment {
        None
    } else {
        Some(args.side.unwrap_or(LineSide::New))
    };

    let current_body = if args.file_comment {
        draft_review
            .file_comment(args.file)
            .map(|c| c.to_string())
            .or_else(|| {
                existing
                    .as_ref()
                    .and_then(|f| f.file_comment.as_ref().map(|c| c.body.clone()))
            })
            .unwrap_or_default()
    } else {
        let line = line.unwrap_or(1);
        let side = side.unwrap_or(LineSide::New);
        draft_review
            .line_comment(args.file, side, line)
            .map(|c| c.to_string())
            .or_else(|| {
                existing.as_ref().and_then(|f| {
                    f.comments
                        .get(&LineKey { side, line })
                        .map(|c| c.body.clone())
                })
            })
            .unwrap_or_default()
    };

    let current_body = if current_body.trim().is_empty() {
        DRAFT_PLACEHOLDER.to_string()
    } else {
        current_body
    };

    if args.file_comment {
        draft_review.set_file_comment(args.file, current_body);
    } else {
        let line = line.context("missing --line <n>")?;
        let side = side.unwrap_or(LineSide::New);
        draft_review.set_line_comment(args.file, side, line, current_body);
    }

    let draft = render_prompt_draft(repo, base_ref, &draft_review);
    std::fs::write(&path, draft).context("write draft file")?;
    rebuild_draft_meta(repo, notes_ref, base_ref, &draft_review, None)?;
    if args.print_path {
        println!("{}", path.display());
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct ApplyReport {
    pub(crate) applied: usize,
    pub(crate) skipped: Vec<ApplySkip>,
}

pub(crate) struct ApplySkip {
    pub(crate) file: String,
    pub(crate) line: u32,
    pub(crate) side: LineSide,
    pub(crate) reason: String,
}

#[derive(Clone, Copy)]
pub(crate) enum SyncAction {
    None,
    DraftFromNotes,
    NotesFromDraft,
}

pub(crate) enum DraftSyncMode {
    Passive,
    OnSave,
}

pub(crate) struct SyncReport {
    pub(crate) draft_updated: bool,
}

pub(crate) fn apply_draft(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<&str>,
    remove_after: bool,
) -> Result<ApplyReport> {
    let path = draft_path(repo)?;
    let content = std::fs::read_to_string(&path).context("read draft file")?;
    let review = parse_prompt_draft(&content)?;
    let meta = load_draft_meta(repo).unwrap_or_default();
    let meta_map = meta_hash_map(&meta);
    let diff_context = prompt_diff_context(repo);
    let base_tree = base_ref.and_then(|b| crate::git::merge_base_tree(repo, b).ok());
    let view_order = prompt_view_order(base_ref.is_some());
    let mut resolver = LineSnippetResolver::new(repo, base_tree, diff_context, view_order);
    let mut report = ApplyReport::default();
    for (path_key, file_review) in &review.files {
        let mut updated = Review::new();
        if let Some(existing) = load_file_review(repo, notes_ref, path_key)? {
            updated.files.insert(path_key.to_string(), existing);
        }

        if file_review.file_comment.is_none() {
            updated.remove_file_comment(path_key);
        }

        let draft_keys: std::collections::BTreeSet<LineKey> =
            file_review.comments.keys().copied().collect();
        let existing_keys = updated
            .files
            .get(path_key)
            .map(|f| f.comments.keys().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        for key in existing_keys {
            if !draft_keys.contains(&key) {
                updated.remove_line_comment(path_key, key.side, key.line);
            }
        }

        if let Some(fc) = file_review.file_comment.as_ref() {
            updated.set_file_comment(path_key, fc.clone());
            report.applied += 1;
        }
        for (line_key, comment) in &file_review.comments {
            let snippet = resolver.snippet(path_key, *line_key);
            let Some(snippet) = snippet else {
                report.skipped.push(ApplySkip {
                    file: path_key.to_string(),
                    line: line_key.line,
                    side: line_key.side,
                    reason: "no snippet available".to_string(),
                });
                continue;
            };
            let hash = snippet_hash(path_key, *line_key, &snippet)?;
            let meta_key = meta_key(path_key, side_label(line_key.side), line_key.line);
            let stored = match meta_map.get(&meta_key) {
                None => {
                    report.skipped.push(ApplySkip {
                        file: path_key.to_string(),
                        line: line_key.line,
                        side: line_key.side,
                        reason: "missing checksum".to_string(),
                    });
                    continue;
                }
                Some(stored) if stored != &hash => {
                    report.skipped.push(ApplySkip {
                        file: path_key.to_string(),
                        line: line_key.line,
                        side: line_key.side,
                        reason: "draft out of date".to_string(),
                    });
                    continue;
                }
                Some(stored) => stored.clone(),
            };
            updated.set_line_comment(path_key, line_key.side, line_key.line, comment.clone());
            updated.set_line_comment_snippet_hash(
                path_key,
                line_key.side,
                line_key.line,
                Some(stored),
            );
            report.applied += 1;
        }

        persist_file_review(repo, notes_ref, path_key, updated.files.get(path_key))?;
    }

    if remove_after {
        std::fs::remove_file(&path).ok();
        let meta_path = draft_meta_path(repo)?;
        std::fs::remove_file(meta_path).ok();
    }
    Ok(report)
}

pub(crate) fn sync_draft_notes(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<&str>,
) -> Result<SyncReport> {
    sync_draft_notes_with_mode(repo, notes_ref, base_ref, DraftSyncMode::Passive)
}

pub(crate) fn sync_draft_notes_on_save(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<&str>,
) -> Result<SyncReport> {
    sync_draft_notes_with_mode(repo, notes_ref, base_ref, DraftSyncMode::OnSave)
}

fn sync_draft_notes_with_mode(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<&str>,
    _mode: DraftSyncMode,
) -> Result<SyncReport> {
    ensure_draft_exists(repo, notes_ref, base_ref)?;

    let draft_path = draft_path(repo)?;
    let mut draft_review = load_draft_review(&draft_path)?;
    let meta = load_draft_meta(repo).unwrap_or_default();
    let meta_map = meta_hash_map(&meta);

    let draft_mtime = draft_mtime(&draft_path).unwrap_or(0);
    let draft_hash = match std::fs::read_to_string(&draft_path) {
        Ok(content) => draft_content_hash(&content)?,
        Err(_) => String::new(),
    };
    let notes_info = notes_ref_info(repo, notes_ref).ok();
    let notes_ref_time = notes_info.as_ref().map(|info| info.time).unwrap_or(0);
    let draft_newer = draft_mtime >= notes_ref_time;
    let draft_changed = meta.draft_hash.as_deref().is_none_or(|h| h != draft_hash);
    let notes_changed = meta
        .notes_ref_oid
        .as_deref()
        .is_none_or(|oid| Some(oid) != notes_info.as_ref().map(|i| i.oid.as_str()));
    let change_bias = if draft_changed && !notes_changed {
        Some(SyncAction::NotesFromDraft)
    } else if notes_changed && !draft_changed {
        Some(SyncAction::DraftFromNotes)
    } else {
        None
    };

    let diff_context = prompt_diff_context(repo);
    let base_tree = base_ref.and_then(|b| crate::git::merge_base_tree(repo, b).ok());
    let view_order = prompt_view_order(base_ref.is_some());
    let mut resolver = LineSnippetResolver::new(repo, base_tree, diff_context, view_order);
    let mut preferred_hashes = HashMap::new();

    let mut paths = BTreeSet::new();
    if let Ok(tracked) = list_tracked_paths(repo) {
        paths.extend(tracked);
    }
    paths.extend(draft_review.files.keys().cloned());

    let mut draft_updated = false;
    for path in paths {
        let mut notes_file = load_file_review(repo, notes_ref, &path)?.unwrap_or_default();
        let mut notes_unresolved = notes_file.clone();
        prune_resolved(&mut notes_unresolved);
        let mut notes_dirty = false;

        let mut draft_file = draft_review.files.get(&path).cloned().unwrap_or_default();

        // File comment sync (change-biased).
        match (
            draft_file.file_comment.clone(),
            notes_unresolved
                .file_comment
                .as_ref()
                .map(|c| c.body.clone()),
        ) {
            (Some(draft_body), Some(notes_body)) => {
                if draft_body != notes_body {
                    let prefer_draft = change_bias
                        .as_ref()
                        .map_or(draft_newer, |b| matches!(b, SyncAction::NotesFromDraft));
                    if prefer_draft {
                        notes_file.file_comment = Some(crate::review::Comment {
                            body: draft_body,
                            resolved: false,
                            snippet_hash: None,
                        });
                        notes_dirty = true;
                    } else {
                        draft_file.file_comment = Some(notes_body);
                        draft_updated = true;
                    }
                }
            }
            (Some(draft_body), None) => {
                let prefer_draft = change_bias
                    .as_ref()
                    .map_or(draft_newer, |b| matches!(b, SyncAction::NotesFromDraft));
                if prefer_draft {
                    notes_file.file_comment = Some(crate::review::Comment {
                        body: draft_body,
                        resolved: false,
                        snippet_hash: None,
                    });
                    notes_dirty = true;
                } else {
                    draft_file.file_comment = None;
                    draft_updated = true;
                }
            }
            (None, Some(notes_body)) => {
                let prefer_draft = change_bias
                    .as_ref()
                    .map_or(draft_newer, |b| matches!(b, SyncAction::NotesFromDraft));
                if prefer_draft {
                    notes_file.file_comment = None;
                    notes_dirty = true;
                } else {
                    draft_file.file_comment = Some(notes_body);
                    draft_updated = true;
                }
            }
            (None, None) => {}
        }

        let mut keys = BTreeSet::new();
        keys.extend(draft_file.comments.keys().copied());
        keys.extend(notes_unresolved.comments.keys().copied());

        for key in keys {
            let draft_body = draft_file.comments.get(&key).cloned();
            let notes_comment = notes_unresolved.comments.get(&key);
            let notes_body = notes_comment.map(|c| c.body.clone());
            let notes_hash = notes_comment.and_then(|c| c.snippet_hash.clone());

            if draft_body.is_none()
                && change_bias
                    .as_ref()
                    .is_some_and(|b| matches!(b, SyncAction::NotesFromDraft))
            {
                if notes_body.is_some() {
                    notes_file.comments.remove(&key);
                    notes_dirty = true;
                }
                continue;
            }

            let meta_key = meta_key(&path, side_label(key.side), key.line);
            let draft_hash = meta_map.get(&meta_key).cloned();
            let current_hash = resolver
                .snippet(&path, key)
                .and_then(|snippet| snippet_hash(&path, key, &snippet).ok());
            let snippet_present = current_hash.is_some();
            let stored_hash = notes_hash.clone().or(draft_hash.clone());
            let invalid = if snippet_present {
                stored_hash
                    .as_ref()
                    .is_some_and(|stored| Some(stored.as_str()) != current_hash.as_deref())
            } else {
                draft_body.is_some() || notes_body.is_some()
            };
            if invalid {
                if notes_body.is_some() {
                    if let Some(comment) = notes_file.comments.get_mut(&key) {
                        comment.resolved = true;
                        notes_dirty = true;
                    }
                }
                if draft_body.is_some() {
                    draft_file.comments.remove(&key);
                    draft_updated = true;
                }
                continue;
            }

            if notes_body.is_some() && notes_hash.is_none() && snippet_present {
                if let Some(comment) = notes_file.comments.get_mut(&key) {
                    comment.snippet_hash = draft_hash.clone().or(current_hash.clone());
                    notes_dirty = true;
                }
            }

            let draft_hash_missing = draft_body.is_some() && draft_hash.is_none() && notes_body.is_some();
            let draft_valid = draft_body.is_some() && snippet_present && !draft_hash_missing;
            let notes_valid = notes_body.is_some() && snippet_present;

            let winner = if draft_valid && !notes_valid {
                if matches!(change_bias, Some(SyncAction::DraftFromNotes)) {
                    SyncAction::DraftFromNotes
                } else {
                    SyncAction::NotesFromDraft
                }
            } else if notes_valid && !draft_valid {
                SyncAction::DraftFromNotes
            } else if draft_valid && notes_valid {
                if draft_body == notes_body {
                    SyncAction::None
                } else if let Some(bias) = change_bias {
                    bias
                } else if draft_newer {
                    SyncAction::NotesFromDraft
                } else {
                    SyncAction::DraftFromNotes
                }
            } else if let Some(bias) = change_bias {
                bias
            } else if draft_newer {
                SyncAction::NotesFromDraft
            } else {
                SyncAction::DraftFromNotes
            };

            match winner {
                SyncAction::NotesFromDraft => {
                    if let Some(body) = draft_body {
                        let hash = draft_hash.or(current_hash.clone());
                        notes_file.comments.insert(
                            key,
                            crate::review::Comment {
                                body,
                                resolved: false,
                                snippet_hash: hash,
                            },
                        );
                        notes_dirty = true;
                    } else if notes_body.is_some() {
                        notes_file.comments.remove(&key);
                        notes_dirty = true;
                    }
                }
                SyncAction::DraftFromNotes => {
                    if let Some(body) = notes_body {
                        draft_file.comments.insert(key, body);
                        draft_updated = true;
                    } else if draft_body.is_some() {
                        draft_file.comments.remove(&key);
                        draft_updated = true;
                    }
                }
                SyncAction::None => {}
            }
        }

        if draft_file.file_comment.is_none() && draft_file.comments.is_empty() {
            if draft_review.files.remove(&path).is_some() {
                draft_updated = true;
            }
        } else {
            draft_review.files.insert(path.clone(), draft_file);
        }

        for (key, comment) in &notes_file.comments {
            if comment.resolved {
                continue;
            }
            if let Some(hash) = comment.snippet_hash.as_ref() {
                preferred_hashes.insert(
                    meta_key(&path, side_label(key.side), key.line),
                    hash.clone(),
                );
            }
        }

        if notes_dirty {
            persist_file_review(repo, notes_ref, &path, Some(&notes_file))?;
        }
    }

    if draft_updated {
        let draft = render_prompt_draft(repo, base_ref, &draft_review);
        std::fs::write(&draft_path, draft).context("write draft file")?;
    }

    rebuild_draft_meta(
        repo,
        notes_ref,
        base_ref,
        &draft_review,
        Some(&preferred_hashes),
    )?;

    Ok(SyncReport { draft_updated })
}

fn load_draft_meta(repo: &gix::Repository) -> Result<DraftMeta> {
    let path = draft_meta_path(repo)?;
    if !path.exists() {
        return Ok(DraftMeta::default());
    }
    let content = std::fs::read_to_string(path).context("read draft metadata")?;
    if content.trim().is_empty() {
        return Ok(DraftMeta::default());
    }
    let meta: DraftMeta = serde_json::from_str(&content).context("parse draft metadata")?;
    Ok(meta)
}

fn write_draft_meta(path: &std::path::Path, meta: &DraftMeta) -> Result<()> {
    let content = serde_json::to_string_pretty(meta).context("serialize draft metadata")?;
    std::fs::write(path, content).context("write draft metadata")?;
    Ok(())
}

fn rebuild_draft_meta(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<&str>,
    draft_review: &DraftReview,
    preferred_hashes: Option<&HashMap<String, String>>,
) -> Result<()> {
    let meta_path = draft_meta_path(repo)?;
    if let Some(parent) = meta_path.parent() {
        std::fs::create_dir_all(parent).context("create draft directory")?;
    }

    let info = notes_ref_info(repo, notes_ref).ok();
    let existing = load_draft_meta(repo).unwrap_or_default();
    let existing_map = meta_hash_map(&existing);
    let mut meta = DraftMeta {
        notes_ref_oid: info.as_ref().map(|i| i.oid.clone()),
        notes_ref_time: info.as_ref().map(|i| i.time),
        draft_mtime: draft_mtime(&draft_path(repo)?),
        draft_hash: None,
        lines: Vec::new(),
    };
    let draft_hash = std::fs::read_to_string(draft_path(repo)?)
        .ok()
        .and_then(|content| draft_content_hash(&content).ok());
    meta.draft_hash = draft_hash;

    let diff_context = prompt_diff_context(repo);
    let base_tree = base_ref.and_then(|b| crate::git::merge_base_tree(repo, b).ok());
    let view_order = prompt_view_order(base_ref.is_some());
    let mut resolver = LineSnippetResolver::new(repo, base_tree, diff_context, view_order);

    for (path, file) in &draft_review.files {
        for key in file.comments.keys() {
            let meta_key = meta_key(path, side_label(key.side), key.line);
            let hash = existing_map
                .get(&meta_key)
                .cloned()
                .or_else(|| preferred_hashes.and_then(|h| h.get(&meta_key).cloned()))
                .or_else(|| {
                    resolver
                        .snippet(path, *key)
                        .and_then(|snippet| snippet_hash(path, *key, &snippet).ok())
                });
            if let Some(hash) = hash {
                meta.lines.push(DraftLineMeta {
                    file: path.to_string(),
                    side: side_label(key.side).to_string(),
                    line: key.line,
                    hash,
                });
            }
        }
    }

    write_draft_meta(&meta_path, &meta)
}

#[derive(Debug)]
struct NotesRefInfo {
    oid: String,
    time: i64,
}

fn notes_ref_info(repo: &gix::Repository, notes_ref: &str) -> Result<NotesRefInfo> {
    let Some(r) = repo
        .try_find_reference(notes_ref)
        .context("find notes ref")?
    else {
        anyhow::bail!("notes ref not found");
    };
    let commit = r
        .id()
        .object()
        .context("read notes ref object")?
        .peel_to_commit()
        .context("notes ref is not a commit")?;
    let time = commit.time().context("read notes ref time")?.seconds;
    Ok(NotesRefInfo {
        oid: commit.id.to_string(),
        time,
    })
}

fn draft_mtime(path: &std::path::Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)?;
    Some(secs)
}

fn draft_content_hash(content: &str) -> Result<String> {
    let mut h = hasher(Kind::Sha1);
    h.update(b"remark-draft-content-v1\0");
    h.update(content.as_bytes());
    let oid = h.try_finalize().context("finalize draft hash")?;
    Ok(oid.to_string())
}

fn list_tracked_paths(repo: &gix::Repository) -> Result<Vec<String>> {
    let index = repo.index_or_empty().context("open index")?;
    let mut options = repo.dirwalk_options().context("init dirwalk options")?;
    options.set_emit_tracked(true);
    options.set_emit_untracked(gix_dir::walk::EmissionMode::Matching);
    options.set_emit_ignored(None);

    let mut delegate = gix_dir::walk::delegate::Collect::default();
    let should_interrupt = std::sync::atomic::AtomicBool::new(false);
    repo.dirwalk(
        &index,
        std::iter::empty::<&gix::bstr::BStr>(),
        &should_interrupt,
        options,
        &mut delegate,
    )
    .context("dirwalk worktree")?;

    let mut paths = BTreeSet::new();
    for (entry, _) in delegate.into_entries_by_path() {
        let kind = entry.disk_kind.or(entry.index_kind);
        if matches!(
            kind,
            Some(gix_dir::entry::Kind::File | gix_dir::entry::Kind::Symlink)
        ) {
            paths.insert(entry.rela_path.to_str_lossy().into_owned());
        }
    }

    Ok(paths.into_iter().collect())
}

fn meta_hash_map(meta: &DraftMeta) -> HashMap<String, String> {
    meta.lines
        .iter()
        .map(|line| {
            (
                meta_key(&line.file, &line.side, line.line),
                line.hash.clone(),
            )
        })
        .collect()
}

fn meta_key(file: &str, side: &str, line: u32) -> String {
    format!("{file}\0{side}\0{line}")
}

fn snippet_hash(path: &str, key: LineKey, snippet: &str) -> Result<String> {
    let mut h = hasher(Kind::Sha1);
    h.update(b"remark-draft-snippet-v1\0");
    h.update(path.as_bytes());
    h.update(b"\0");
    h.update(side_label(key.side).as_bytes());
    h.update(b"\0");
    h.update(key.line.to_string().as_bytes());
    h.update(b"\0");
    h.update(snippet.as_bytes());
    let oid = h.try_finalize().context("finalize snippet hash")?;
    Ok(oid.to_string())
}

pub(crate) fn current_snippet_hash(
    repo: &gix::Repository,
    base_ref: Option<&str>,
    path: &str,
    key: LineKey,
) -> Option<String> {
    let diff_context = prompt_diff_context(repo);
    let base_tree = base_ref.and_then(|b| crate::git::merge_base_tree(repo, b).ok());
    let view_order = prompt_view_order(base_ref.is_some());
    let mut resolver = LineSnippetResolver::new(repo, base_tree, diff_context, view_order);
    let snippet = resolver.snippet(path, key)?;
    snippet_hash(path, key, &snippet).ok()
}

fn side_label(side: LineSide) -> &'static str {
    match side {
        LineSide::Old => "old",
        LineSide::New => "new",
    }
}

fn prune_resolved(review: &mut crate::review::FileReview) {
    if matches!(
        review.file_comment,
        Some(crate::review::Comment { resolved: true, .. })
    ) {
        review.file_comment = None;
    }
    review.comments.retain(|_, comment| !comment.resolved);
}

pub(crate) fn load_review_from_draft(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<&str>,
) -> Result<Review> {
    ensure_draft_exists(repo, notes_ref, base_ref)?;
    let path = draft_path(repo)?;
    let content = std::fs::read_to_string(&path).context("read draft file")?;
    let draft = parse_prompt_draft(&content)?;
    Ok(draft_to_review(&draft))
}

#[cfg(test)]
pub(crate) fn write_draft_from_review(
    repo: &gix::Repository,
    notes_ref: &str,
    base_ref: Option<&str>,
    review: &Review,
) -> Result<()> {
    let draft_review = write_draft_from_review_impl(repo, base_ref, review)?;
    let mut preferred_hashes = HashMap::new();
    for (path, file) in &review.files {
        for (key, comment) in &file.comments {
            if comment.resolved {
                continue;
            }
            if let Some(hash) = comment.snippet_hash.as_ref() {
                preferred_hashes.insert(
                    meta_key(path, side_label(key.side), key.line),
                    hash.clone(),
                );
            }
        }
    }
    rebuild_draft_meta(
        repo,
        notes_ref,
        base_ref,
        &draft_review,
        Some(&preferred_hashes),
    )?;
    Ok(())
}

pub(crate) fn write_draft_from_review_no_meta(
    repo: &gix::Repository,
    base_ref: Option<&str>,
    review: &Review,
) -> Result<()> {
    let _ = write_draft_from_review_impl(repo, base_ref, review)?;
    Ok(())
}

fn write_draft_from_review_impl(
    repo: &gix::Repository,
    base_ref: Option<&str>,
    review: &Review,
) -> Result<DraftReview> {
    let draft_review = review_to_draft(review);
    let path = draft_path(repo)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create draft directory")?;
    }
    let draft = render_prompt_draft(repo, base_ref, &draft_review);
    std::fs::write(&path, draft).context("write draft file")?;
    Ok(draft_review)
}

fn render_prompt_draft(
    repo: &gix::Repository,
    base_ref: Option<&str>,
    review: &DraftReview,
) -> String {
    let diff_context = prompt_diff_context(repo);
    let base_tree = base_ref.and_then(|b| crate::git::merge_base_tree(repo, b).ok());
    let view_order = prompt_view_order(base_ref.is_some());
    let mut resolver = LineSnippetResolver::new(repo, base_tree, diff_context, view_order);
    let review = draft_to_review(review);
    crate::review::render_prompt(&review, |path, key| {
        resolver.snippet(path, key).map(|code| PromptSnippet {
            code,
            lang: language_for_path(path),
        })
    })
}

fn draft_to_review(draft: &DraftReview) -> Review {
    let mut review = Review::new();
    for (path, file) in &draft.files {
        if let Some(comment) = file.file_comment.as_ref() {
            review.set_file_comment(path, comment.clone());
        }
        for (line_key, comment) in &file.comments {
            review.set_line_comment(path, line_key.side, line_key.line, comment.clone());
        }
    }
    review
}

fn review_to_draft(review: &Review) -> DraftReview {
    let mut draft = DraftReview::default();
    for (path, file) in &review.files {
        if let Some(comment) = file
            .file_comment
            .as_ref()
            .filter(|c| !c.resolved)
            .map(|c| c.body.trim_end())
            .filter(|body| !body.is_empty())
        {
            draft.set_file_comment(path, comment.to_string());
        }
        for (key, comment) in &file.comments {
            if comment.resolved {
                continue;
            }
            let body = comment.body.trim_end();
            if body.is_empty() {
                continue;
            }
            draft.set_line_comment(path, key.side, key.line, body.to_string());
        }
    }
    draft
}

fn parse_prompt_draft(content: &str) -> Result<DraftReview> {
    let mut review = DraftReview::default();
    let mut current_file: Option<String> = None;
    let mut mode = DraftSection::None;
    let mut pending_line: Option<(u32, LineSide)> = None;
    let mut text_fence: Option<String> = None;
    let mut skip_fence: Option<String> = None;
    let mut pending_target: Option<DraftTarget> = None;
    let mut body = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim_start();
        if let Some(fence) = text_fence.as_ref() {
            if trimmed == fence.as_str() {
                if let Some(target) = pending_target.take() {
                    let text = body.join("\n").trim_end().to_string();
                    let trimmed = text.trim();
                    if trimmed.is_empty() || trimmed == DRAFT_PLACEHOLDER {
                        body.clear();
                        text_fence = None;
                        continue;
                    }
                    match target {
                        DraftTarget::File(path) => review.set_file_comment(&path, text),
                        DraftTarget::Line(path, line, side) => {
                            review.set_line_comment(&path, side, line, text);
                        }
                    }
                }
                body.clear();
                text_fence = None;
                continue;
            }
            body.push(line);
            continue;
        }

        if let Some(fence) = skip_fence.as_ref() {
            if trimmed == fence.as_str() {
                skip_fence = None;
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("```") {
            let ticks = trimmed.chars().take_while(|c| *c == '`').count();
            let fence = "`".repeat(ticks.max(3));
            let lang = rest.trim();
            if lang.starts_with("text") {
                pending_target = match mode {
                    DraftSection::FileComment => current_file.clone().map(DraftTarget::File),
                    DraftSection::LineComments => current_file.clone().and_then(|path| {
                        pending_line.map(|(line, side)| DraftTarget::Line(path, line, side))
                    }),
                    DraftSection::None => None,
                };
                if mode == DraftSection::LineComments {
                    pending_line = None;
                }
                body.clear();
                text_fence = Some(fence);
            } else {
                skip_fence = Some(fence);
            }
            continue;
        }

        if let Some(path) = trimmed.strip_prefix("## ") {
            current_file = Some(path.trim().to_string());
            mode = DraftSection::None;
            pending_line = None;
            continue;
        }
        if trimmed.starts_with("### File comment") {
            mode = DraftSection::FileComment;
            pending_line = None;
            continue;
        }
        if trimmed.starts_with("### Line comments") {
            mode = DraftSection::LineComments;
            continue;
        }
        if mode == DraftSection::LineComments
            && let Some(rest) = trimmed.strip_prefix("- line ")
        {
            pending_line = parse_line_marker(rest);
        }
    }

    Ok(review)
}

fn load_draft_review(path: &std::path::Path) -> Result<DraftReview> {
    if !path.exists() {
        return Ok(DraftReview::default());
    }
    let content = std::fs::read_to_string(path).context("read draft file")?;
    if content.trim().is_empty() {
        return Ok(DraftReview::default());
    }
    parse_prompt_draft(&content)
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum DraftSection {
    None,
    FileComment,
    LineComments,
}

enum DraftTarget {
    File(String),
    Line(String, u32, LineSide),
}

fn parse_line_marker(input: &str) -> Option<(u32, LineSide)> {
    let digits: String = input.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        let line = digits.parse().ok()?;
        let rest = &input[digits.len()..];
        let side = if rest.contains("old") {
            LineSide::Old
        } else {
            LineSide::New
        };
        Some((line, side))
    }
}

fn prompt_view_order(include_base: bool) -> Vec<ViewKind> {
    let mut order = vec![ViewKind::All, ViewKind::Staged, ViewKind::Unstaged];
    if include_base {
        order.push(ViewKind::Base);
    }
    order
}

const DEFAULT_DIFF_CONTEXT: u32 = 3;
const MIN_DIFF_CONTEXT: u32 = 0;
const MAX_DIFF_CONTEXT: u32 = 20;

fn prompt_diff_context(repo: &gix::Repository) -> u32 {
    let raw = crate::git::read_local_config_value(repo, "remark.diffContext")
        .ok()
        .flatten();
    raw.and_then(|v| v.trim().parse::<u32>().ok())
        .map(|v| v.clamp(MIN_DIFF_CONTEXT, MAX_DIFF_CONTEXT))
        .unwrap_or(DEFAULT_DIFF_CONTEXT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{FileTime, set_file_mtime};
    use gix::bstr::ByteSlice;
    use gix::object::tree::EntryKind;
    use gix_ref::transaction::PreviousValue;
    use std::sync::Once;

    #[test]
    fn review_to_draft_omits_resolved_and_empty() {
        let mut review = Review::new();
        review.set_file_comment("a.rs", "file".to_string());
        review.toggle_file_comment_resolved("a.rs");
        review.set_file_comment("b.rs", "   ".to_string());
        review.set_line_comment("a.rs", LineSide::New, 1, "keep".to_string());
        review.set_line_comment("a.rs", LineSide::Old, 2, "drop".to_string());
        review.toggle_line_comment_resolved("a.rs", LineSide::Old, 2);

        let draft = review_to_draft(&review);
        assert!(draft.file_comment("a.rs").is_none());
        assert!(draft.file_comment("b.rs").is_none());
        assert_eq!(draft.line_comment("a.rs", LineSide::New, 1), Some("keep"));
        assert!(draft.line_comment("a.rs", LineSide::Old, 2).is_none());
    }

    #[test]
    fn parse_prompt_draft_ignores_non_text_fences() {
        let content = r#"# Review Notes

## src/lib.rs
### File comment
```text
File note
```
### Line comments
- line 5
```text
Line note
```
```rust
fn not_a_comment() {}
```
- line 7 (old)
```text
Old note
```
"#;

        let draft = parse_prompt_draft(content).unwrap();
        assert_eq!(draft.file_comment("src/lib.rs"), Some("File note"));
        assert_eq!(
            draft.line_comment("src/lib.rs", LineSide::New, 5),
            Some("Line note")
        );
        assert_eq!(
            draft.line_comment("src/lib.rs", LineSide::Old, 7),
            Some("Old note")
        );
    }

    #[test]
    fn parse_prompt_draft_ignores_placeholder() {
        let content = r#"# Review Notes

## src/lib.rs
### File comment
```text
<!-- remark:write comment -->
```

### Line comments
- line 3
```text
<!-- remark:write comment -->
```
"#;

        let draft = parse_prompt_draft(content).unwrap();
        assert!(draft.file_comment("src/lib.rs").is_none());
        assert!(draft.line_comment("src/lib.rs", LineSide::New, 3).is_none());
    }

    #[test]
    fn write_draft_inserts_placeholder_for_empty_comment() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        write_draft(
            &repo,
            crate::git::DEFAULT_NOTES_REF,
            None,
            DraftWriteArgs {
                file: "src/lib.rs",
                file_comment: true,
                line: None,
                side: None,
                print_path: false,
            },
        )
        .expect("write draft");

        let draft_path = draft_path(&repo).expect("draft path");
        let content = std::fs::read_to_string(draft_path).expect("read draft");
        assert!(content.contains(DRAFT_PLACEHOLDER));
        assert!(content.contains("### File comment"));
    }

    fn init_repo_with_commit(path: &str, contents: &str) -> (tempfile::TempDir, gix::Repository) {
        ensure_git_identity();
        let td = tempfile::tempdir().expect("tempdir");
        let repo = gix::init(td.path()).expect("init repo");

        let workdir = repo.workdir().expect("workdir");
        let full_path = workdir.join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parent");
        }
        std::fs::write(&full_path, contents).expect("write file");

        let blob_id = repo
            .write_blob(contents.as_bytes())
            .expect("write blob")
            .detach();
        let mut editor = gix::object::tree::Editor::new(&repo.empty_tree()).expect("tree editor");
        editor
            .upsert(path.as_bytes().as_bstr(), EntryKind::Blob, blob_id)
            .expect("upsert tree entry");
        let tree_id = editor.write().expect("write tree").detach();

        let sig = gix_actor::Signature {
            name: "remark-test".into(),
            email: "remark-test@localhost".into(),
            time: gix_date::Time {
                seconds: 0,
                offset: 0,
            },
        };
        let commit = gix::objs::Commit {
            tree: tree_id,
            parents: Default::default(),
            author: sig.clone(),
            committer: sig,
            encoding: None,
            message: "test commit\n".into(),
            extra_headers: Default::default(),
        };
        let commit_id = repo.write_object(commit).expect("write commit").detach();

        repo.reference("HEAD", commit_id, PreviousValue::Any, "test commit")
            .expect("update HEAD");

        (td, repo)
    }

    fn ensure_git_identity() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            // Safe in tests: single-threaded init, process-scoped env vars.
            unsafe {
                std::env::set_var("GIT_AUTHOR_NAME", "remark-test");
                std::env::set_var("GIT_AUTHOR_EMAIL", "remark-test@localhost");
                std::env::set_var("GIT_COMMITTER_NAME", "remark-test");
                std::env::set_var("GIT_COMMITTER_EMAIL", "remark-test@localhost");
            }
        });
    }

    fn file_comment_review(body: &str) -> crate::review::FileReview {
        crate::review::FileReview {
            file_comment: Some(crate::review::Comment {
                body: body.to_string(),
                resolved: false,
                snippet_hash: None,
            }),
            ..Default::default()
        }
    }

    fn write_notes_file_comment(repo: &gix::Repository, notes_ref: &str, path: &str, body: &str) {
        let file = file_comment_review(body);
        persist_file_review(repo, notes_ref, path, Some(&file)).expect("write note");
    }

    fn write_draft_file_comment(
        repo: &gix::Repository,
        base_ref: Option<&str>,
        path: &str,
        body: &str,
    ) {
        let mut review = Review::new();
        review.set_file_comment(path, body.to_string());
        write_draft_review(repo, base_ref, &review);
    }

    fn write_draft_review(repo: &gix::Repository, base_ref: Option<&str>, review: &Review) {
        let draft_review = review_to_draft(review);
        let draft = render_prompt_draft(repo, base_ref, &draft_review);
        std::fs::write(draft_path(repo).expect("draft path"), draft).expect("write draft");
    }

    fn set_draft_mtime_relative(repo: &gix::Repository, notes_ref: &str, delta_secs: i64) {
        let info = notes_ref_info(repo, notes_ref).expect("notes ref info");
        let target = info.time.saturating_add(delta_secs);
        let draft_path = draft_path(repo).expect("draft path");
        let mtime = FileTime::from_unix_time(target, 0);
        set_file_mtime(&draft_path, mtime).expect("set draft mtime");
    }

    #[test]
    fn sync_noop_when_neither_changed() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let notes_ref = crate::git::DEFAULT_NOTES_REF;

        write_notes_file_comment(&repo, notes_ref, "src/lib.rs", "note");
        let mut review = Review::new();
        review.set_file_comment("src/lib.rs", "note".to_string());
        write_draft_from_review(&repo, notes_ref, None, &review).expect("write draft");

        let report = sync_draft_notes_with_mode(&repo, notes_ref, None, DraftSyncMode::Passive)
            .expect("sync");
        assert!(!report.draft_updated);
    }

    #[test]
    fn sync_prefers_draft_when_only_draft_changed() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let notes_ref = crate::git::DEFAULT_NOTES_REF;

        write_notes_file_comment(&repo, notes_ref, "src/lib.rs", "notes");
        let mut review = Review::new();
        review.set_file_comment("src/lib.rs", "notes".to_string());
        write_draft_from_review(&repo, notes_ref, None, &review).expect("write draft");

        write_draft_file_comment(&repo, None, "src/lib.rs", "draft");

        let report = sync_draft_notes_with_mode(&repo, notes_ref, None, DraftSyncMode::Passive)
            .expect("sync");
        assert!(!report.draft_updated);
        let notes = load_file_review(&repo, notes_ref, "src/lib.rs")
            .expect("load notes")
            .expect("notes present");
        let comment = notes.file_comment.expect("file comment");
        assert_eq!(comment.body, "draft");
    }

    #[test]
    fn sync_prefers_notes_when_only_notes_changed() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let notes_ref = crate::git::DEFAULT_NOTES_REF;

        write_notes_file_comment(&repo, notes_ref, "src/lib.rs", "before");
        let mut review = Review::new();
        review.set_file_comment("src/lib.rs", "before".to_string());
        write_draft_from_review(&repo, notes_ref, None, &review).expect("write draft");

        write_notes_file_comment(&repo, notes_ref, "src/lib.rs", "after");

        sync_draft_notes_with_mode(&repo, notes_ref, None, DraftSyncMode::Passive)
            .expect("sync");
        let draft_review =
            load_draft_review(&draft_path(&repo).expect("draft path")).expect("load draft");
        assert_eq!(draft_review.file_comment("src/lib.rs"), Some("after"));
    }

    #[test]
    fn sync_both_changed_same_content_no_updates() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let notes_ref = crate::git::DEFAULT_NOTES_REF;

        write_notes_file_comment(&repo, notes_ref, "src/lib.rs", "same");
        let mut review = Review::new();
        review.set_file_comment("src/lib.rs", "same".to_string());
        write_draft_from_review(&repo, notes_ref, None, &review).expect("write draft");

        let mut meta = load_draft_meta(&repo).expect("load meta");
        meta.notes_ref_oid = Some("deadbeef".to_string());
        meta.draft_hash = Some("deadbeef".to_string());
        write_draft_meta(&draft_meta_path(&repo).expect("meta path"), &meta).expect("write meta");

        let report = sync_draft_notes_with_mode(&repo, notes_ref, None, DraftSyncMode::Passive)
            .expect("sync");
        assert!(!report.draft_updated);
    }

    #[test]
    fn sync_both_changed_prefers_draft_when_newer() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let notes_ref = crate::git::DEFAULT_NOTES_REF;

        write_notes_file_comment(&repo, notes_ref, "src/lib.rs", "notes");
        let mut review = Review::new();
        review.set_file_comment("src/lib.rs", "notes".to_string());
        write_draft_from_review(&repo, notes_ref, None, &review).expect("write draft");

        write_draft_file_comment(&repo, None, "src/lib.rs", "draft-new");
        write_notes_file_comment(&repo, notes_ref, "src/lib.rs", "notes-new");
        set_draft_mtime_relative(&repo, notes_ref, 10);

        let report = sync_draft_notes_with_mode(&repo, notes_ref, None, DraftSyncMode::Passive)
            .expect("sync");
        assert!(!report.draft_updated);

        let notes = load_file_review(&repo, notes_ref, "src/lib.rs")
            .expect("load notes")
            .expect("notes present");
        let comment = notes.file_comment.expect("file comment");
        assert_eq!(comment.body, "draft-new");
    }

    #[test]
    fn sync_both_changed_prefers_notes_when_newer() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let notes_ref = crate::git::DEFAULT_NOTES_REF;

        write_notes_file_comment(&repo, notes_ref, "src/lib.rs", "notes");
        let mut review = Review::new();
        review.set_file_comment("src/lib.rs", "notes".to_string());
        write_draft_from_review(&repo, notes_ref, None, &review).expect("write draft");

        write_draft_file_comment(&repo, None, "src/lib.rs", "draft-new");
        write_notes_file_comment(&repo, notes_ref, "src/lib.rs", "notes-new");
        set_draft_mtime_relative(&repo, notes_ref, -10);

        sync_draft_notes_with_mode(&repo, notes_ref, None, DraftSyncMode::Passive)
            .expect("sync");

        let draft_review =
            load_draft_review(&draft_path(&repo).expect("draft path")).expect("load draft");
        assert_eq!(draft_review.file_comment("src/lib.rs"), Some("notes-new"));
    }

    #[test]
    fn sync_deletes_note_when_draft_removed() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let notes_ref = crate::git::DEFAULT_NOTES_REF;

        write_notes_file_comment(&repo, notes_ref, "src/lib.rs", "note");
        let mut review = Review::new();
        review.set_file_comment("src/lib.rs", "note".to_string());
        write_draft_from_review(&repo, notes_ref, None, &review).expect("write draft");

        let empty_review = Review::new();
        write_draft_review(&repo, None, &empty_review);

        let report = sync_draft_notes_with_mode(&repo, notes_ref, None, DraftSyncMode::Passive)
            .expect("sync");
        assert!(!report.draft_updated);
        assert!(
            load_file_review(&repo, notes_ref, "src/lib.rs")
                .expect("load notes")
                .is_none()
        );
    }

    #[test]
    fn sync_removes_draft_when_notes_removed() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let notes_ref = crate::git::DEFAULT_NOTES_REF;

        write_notes_file_comment(&repo, notes_ref, "src/lib.rs", "note");
        let mut review = Review::new();
        review.set_file_comment("src/lib.rs", "note".to_string());
        write_draft_from_review(&repo, notes_ref, None, &review).expect("write draft");

        persist_file_review(&repo, notes_ref, "src/lib.rs", None).expect("remove note");

        sync_draft_notes_with_mode(&repo, notes_ref, None, DraftSyncMode::Passive)
            .expect("sync");
        let draft_review =
            load_draft_review(&draft_path(&repo).expect("draft path")).expect("load draft");
        assert!(draft_review.file_comment("src/lib.rs").is_none());
    }

    #[test]
    fn sync_line_comment_prefers_notes_when_draft_invalid() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let notes_ref = crate::git::DEFAULT_NOTES_REF;

        let mut notes_review = Review::new();
        notes_review.set_line_comment("src/lib.rs", LineSide::New, 1, "notes".to_string());
        let notes_file = notes_review.files.get("src/lib.rs").expect("notes file");
        persist_file_review(&repo, notes_ref, "src/lib.rs", Some(notes_file)).expect("note");

        let mut draft_review = Review::new();
        draft_review.set_line_comment("src/lib.rs", LineSide::New, 1, "draft".to_string());
        write_draft_from_review(&repo, notes_ref, None, &draft_review).expect("write draft");

        let mut meta = load_draft_meta(&repo).expect("load meta");
        meta.draft_hash = Some("deadbeef".to_string());
        meta.lines.clear();
        write_draft_meta(&draft_meta_path(&repo).expect("meta path"), &meta).expect("write meta");

        sync_draft_notes_with_mode(&repo, notes_ref, None, DraftSyncMode::Passive)
            .expect("sync");
        let draft_review =
            load_draft_review(&draft_path(&repo).expect("draft path")).expect("load draft");
        assert_eq!(
            draft_review.line_comment("src/lib.rs", LineSide::New, 1),
            Some("notes")
        );
    }

    #[test]
    fn sync_line_comment_removes_when_invalidated() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}
");
        let notes_ref = crate::git::DEFAULT_NOTES_REF;

        let mut review = Review::new();
        review.set_line_comment("src/lib.rs", LineSide::New, 1, "note".to_string());
        let notes_file = review.files.get("src/lib.rs").expect("notes file");
        persist_file_review(&repo, notes_ref, "src/lib.rs", Some(notes_file)).expect("note");
        write_draft_from_review(&repo, notes_ref, None, &review).expect("write draft");

        let workdir = repo.workdir().expect("workdir");
        std::fs::write(
            workdir.join("src/lib.rs"),
            "fn main() { println!(\"hi\"); }\n",
        )
        .expect("write updated file");
        sync_draft_notes_with_mode(&repo, notes_ref, None, DraftSyncMode::Passive)
            .expect("sync");
        let draft_review =
            load_draft_review(&draft_path(&repo).expect("draft path")).expect("load draft");
        assert!(draft_review
            .line_comment("src/lib.rs", LineSide::New, 1)
            .is_none());
        let notes_review =
            load_file_review(&repo, notes_ref, "src/lib.rs").expect("load notes");
        let notes_review = notes_review.expect("notes review");
        assert!(notes_review
            .comments
            .get(&LineKey {
                side: LineSide::New,
                line: 1
            })
            .is_some_and(|c| c.resolved));
    }

    #[test]
    fn sync_line_comment_deletes_note_when_draft_missing() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let notes_ref = crate::git::DEFAULT_NOTES_REF;

        let mut notes_review = Review::new();
        notes_review.set_line_comment("src/lib.rs", LineSide::New, 1, "note".to_string());
        let notes_file = notes_review.files.get("src/lib.rs").expect("notes file");
        persist_file_review(&repo, notes_ref, "src/lib.rs", Some(notes_file)).expect("note");

        let mut draft_review = Review::new();
        draft_review.set_line_comment("src/lib.rs", LineSide::New, 1, "note".to_string());
        write_draft_from_review(&repo, notes_ref, None, &draft_review).expect("write draft");

        let empty_review = Review::new();
        write_draft_review(&repo, None, &empty_review);

        let report = sync_draft_notes_with_mode(&repo, notes_ref, None, DraftSyncMode::Passive)
            .expect("sync");
        assert!(!report.draft_updated);
        assert!(
            load_file_review(&repo, notes_ref, "src/lib.rs")
                .expect("load notes")
                .is_none()
        );
    }
}
