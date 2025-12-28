use std::collections::BTreeSet;

use anyhow::{Context, Result};
use gix::bstr::{BStr, ByteSlice};
use gix::{ObjectId, Repository};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    All,
    Unstaged,
    Staged,
    Base,
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

pub fn head_commit_oid(repo: &Repository) -> Result<ObjectId> {
    Ok(repo.head_commit().context("read HEAD commit")?.id)
}

pub fn note_file_key_oid(
    repo: &Repository,
    head_commit: ObjectId,
    kind: ViewKind,
    base_ref: Option<&str>,
    path: &str,
) -> Result<ObjectId> {
    let mut key = String::new();
    key.push_str("remark-file-key:v1\n");
    key.push_str("head:");
    key.push_str(&head_commit.to_string());
    key.push('\n');
    key.push_str("kind:");
    key.push_str(match kind {
        ViewKind::All => "all",
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
    key.push_str("path:");
    key.push_str(path);
    key.push('\n');

    let oid = gix_object::compute_hash(repo.object_hash(), gix_object::Kind::Blob, key.as_bytes())
        .context("compute file note key oid")?;
    Ok(oid)
}

pub fn list_unstaged_paths(repo: &Repository) -> Result<Vec<String>> {
    let mut out = BTreeSet::<String>::new();
    let workdir = repo.workdir().map(ToOwned::to_owned);
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
        let path = item.rela_path().to_str_lossy().into_owned();
        if let Some(wt) = &workdir {
            let abs = wt.join(&path);
            if abs.is_dir() {
                // `gix status` may report untracked directories; we only review files.
                continue;
            }
        }
        out.insert(path);
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
        &index,
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
    let base_tree = merge_base_tree(repo, base_ref)?;
    let head_tree = repo.head_tree().context("head tree")?;

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

pub fn merge_base_tree<'repo>(repo: &'repo Repository, base_ref: &str) -> Result<gix::Tree<'repo>> {
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
    merge_base
        .object()
        .context("merge-base object")?
        .peel_to_commit()
        .context("merge-base peel to commit")?
        .tree()
        .context("merge-base tree")
}

pub fn try_read_worktree(repo: &Repository, path: &str) -> Result<Option<String>> {
    let Some(wt) = repo.workdir() else {
        return Ok(None);
    };
    let abs = wt.join(path);
    match std::fs::read(&abs) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).to_string())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) if err.kind() == std::io::ErrorKind::IsADirectory => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read worktree file '{}'", abs.display())),
    }
}

pub fn try_read_index(repo: &Repository, path: &str) -> Result<Option<String>> {
    let index = repo.index_or_empty().context("open index")?;
    let Some(entry) = index.entry_by_path(path.as_bytes().as_bstr()) else {
        return Ok(None);
    };
    let blob = repo
        .find_object(entry.id)
        .context("find index blob")?
        .try_into_blob()
        .context("index entry is not a blob")?;
    Ok(Some(
        String::from_utf8_lossy(blob.data.as_ref()).to_string(),
    ))
}

pub fn try_read_head(repo: &Repository, path: &str) -> Result<Option<String>> {
    let tree = repo.head_tree().context("read HEAD tree")?;
    let Some(entry) = tree
        .lookup_entry_by_path(path)
        .with_context(|| format!("lookup '{path}' in HEAD tree"))?
    else {
        return Ok(None);
    };
    let blob = entry
        .object()
        .context("load tree entry")?
        .try_into_blob()
        .context("tree entry is not a blob")?;
    Ok(Some(
        String::from_utf8_lossy(blob.data.as_ref()).to_string(),
    ))
}

pub fn try_read_tree(tree: &gix::Tree<'_>, path: &str) -> Result<Option<String>> {
    let Some(entry) = tree
        .lookup_entry_by_path(path)
        .with_context(|| format!("lookup '{path}' in tree"))?
    else {
        return Ok(None);
    };
    let blob = entry
        .object()
        .context("load tree entry")?
        .try_into_blob()
        .context("tree entry is not a blob")?;
    Ok(Some(
        String::from_utf8_lossy(blob.data.as_ref()).to_string(),
    ))
}
