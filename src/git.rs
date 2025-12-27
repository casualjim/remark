use std::collections::BTreeSet;

use anyhow::{Context, Result};
use gix::bstr::{BStr, ByteSlice};
use gix::{ObjectId, Repository};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Unstaged,
    Staged,
    Base,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSourceKind {
    Worktree,
    Index,
    HeadTree,
}

pub fn default_base_ref(repo: &Repository) -> Option<String> {
    // Prefer configured upstream if available.
    repo.rev_parse_single("@{upstream}".as_bytes().as_bstr())
        .ok()
        .map(|_| "@{upstream}".to_string())
        .or_else(|| {
            // Try common branch names.
            for candidate in ["refs/heads/main", "refs/heads/master"] {
                if repo.try_find_reference(candidate).ok().flatten().is_some() {
                    return Some(candidate.to_string());
                }
            }
            None
        })
        .or_else(|| {
            // Best-effort: origin/HEAD symbolic ref.
            repo.try_find_reference("refs/remotes/origin/HEAD")
                .ok()
                .flatten()
                .and_then(|r| r.target().try_name().map(|n| n.to_string()))
        })
}

pub fn note_key_oid(repo: &Repository, kind: ViewKind, base_ref: Option<&str>) -> Result<ObjectId> {
    let mut key = String::new();
    key.push_str("git-review-key:v1\n");
    key.push_str("kind:");
    key.push_str(match kind {
        ViewKind::Unstaged => "unstaged",
        ViewKind::Staged => "staged",
        ViewKind::Base => "base",
    });
    key.push('\n');
    if let Some(b) = base_ref {
        key.push_str("base:");
        key.push_str(b);
        key.push('\n');
    }
    let blob_id = repo.write_blob(key.as_bytes()).context("write note key blob")?;
    Ok(blob_id.detach())
}

pub fn list_unstaged_paths(repo: &Repository) -> Result<Vec<String>> {
    let mut out = BTreeSet::<String>::new();
    let iter = repo
        .status(gix::progress::Discard)
        .context("init status")?
        .into_index_worktree_iter(Vec::new())
        .context("status index-worktree")?;

    for item in iter {
        let item = item.context("status item")?;
        if item.summary().is_none() {
            continue;
        }
        out.insert(item.rela_path().to_str_lossy().into_owned());
    }

    Ok(out.into_iter().collect())
}

pub fn list_staged_paths(repo: &Repository) -> Result<Vec<String>> {
    let head_tree_id = repo.head_tree_id().context("get HEAD tree id")?;
    let index = repo
        .index_or_load_from_head_or_empty()
        .context("open worktree index")?;

    let mut out = BTreeSet::<String>::new();
    repo.tree_index_status(
        &head_tree_id,
        &*index,
        None,
        gix::status::tree_index::TrackRenames::Disabled,
        |change, _, _| {
            out.insert(change.location().to_str_lossy().into_owned());
            Ok::<_, std::convert::Infallible>(gix::diff::index::Action::Continue)
        },
    )
    .context("tree-index status")?;

    Ok(out.into_iter().collect())
}

pub fn list_base_paths(repo: &Repository, base_ref: &str) -> Result<Vec<String>> {
    let head_commit = repo.head_commit().context("read HEAD commit")?;
    let head_id = head_commit.id;

    let base_commit = repo
        .rev_parse_single(base_ref.as_bytes().as_bstr())
        .with_context(|| format!("resolve base ref '{base_ref}'"))?
        .object()
        .context("peel base object")?
        .peel_to_commit()
        .context("base is not a commit")?;
    let base_id = base_commit.id;

    let merge_base = repo
        .merge_base(head_id, base_id)
        .context("find merge base")?;
    let base_tree = merge_base
        .object()
        .context("merge-base object")?
        .peel_to_commit()
        .context("merge-base peel to commit")?
        .tree()
        .context("merge-base tree")?;
    let head_tree = head_commit.tree().context("head tree")?;

    let changes = repo
        .diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)
        .context("diff trees")?;

    let mut out = BTreeSet::<String>::new();
    for ch in changes {
        use gix::object::tree::diff::ChangeDetached as C;
        let loc: &BStr = match &ch {
            C::Addition { location, .. } => location.as_ref(),
            C::Deletion { location, .. } => location.as_ref(),
            C::Modification { location, .. } => location.as_ref(),
            C::Rewrite { location, .. } => location.as_ref(),
        };
        out.insert(loc.to_str_lossy().into_owned());
    }

    Ok(out.into_iter().collect())
}

pub fn read_file(repo: &Repository, source: FileSourceKind, path: &str) -> Result<String> {
    match source {
        FileSourceKind::Worktree => {
            let wt = repo
                .workdir()
                .context("repo has no workdir (needed for worktree view)")?;
            let abs = wt.join(path);
            std::fs::read_to_string(&abs)
                .with_context(|| format!("read worktree file '{}'", abs.display()))
        }
        FileSourceKind::Index => read_index_file(repo, path),
        FileSourceKind::HeadTree => read_head_file(repo, path),
    }
}

fn read_index_file(repo: &Repository, path: &str) -> Result<String> {
    let index = repo.index_or_empty().context("open index")?;
    let entry = index
        .entry_by_path(path.as_bytes().as_bstr())
        .with_context(|| format!("index entry for '{path}' not found"))?;
    let blob = repo
        .find_object(entry.id)
        .context("find index blob")?
        .try_into_blob()
        .context("index entry is not a blob")?;
    Ok(String::from_utf8_lossy(blob.data.as_ref()).to_string())
}

fn read_head_file(repo: &Repository, path: &str) -> Result<String> {
    let tree = repo.head_tree().context("read HEAD tree")?;
    let entry = tree
        .lookup_entry_by_path(path)
        .with_context(|| format!("lookup '{path}' in HEAD tree"))?
        .with_context(|| format!("'{path}' not found in HEAD tree"))?;
    let blob = entry
        .object()
        .context("load tree entry")?
        .try_into_blob()
        .context("tree entry is not a blob")?;
    Ok(String::from_utf8_lossy(blob.data.as_ref()).to_string())
}

pub fn view_file_source(view: ViewKind) -> FileSourceKind {
    match view {
        ViewKind::Unstaged => FileSourceKind::Worktree,
        ViewKind::Staged => FileSourceKind::Index,
        ViewKind::Base => FileSourceKind::HeadTree,
    }
}
