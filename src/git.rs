use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use anyhow::{Context, Result};
use gix::bstr::{BStr, ByteSlice};
use gix::{ObjectId, Repository};
use gix_dir::walk::EmissionMode;

pub const DEFAULT_NOTES_REF: &str = "refs/notes/remark";
pub const CONFIG_NOTES_REF_KEY: &str = "remark.notesRef";
pub const CONFIG_NOTES_REMOTE_KEY: &str = "remark.notesRemote";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum ViewKind {
  All,
  Unstaged,
  Staged,
  Base,
}

pub fn default_base_ref(repo: &Repository) -> Option<String> {
  // Prefer configured upstream if available.
  repo
    .rev_parse_single("@{upstream}".as_bytes().as_bstr())
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
      repo
        .try_find_reference("refs/remotes/origin/HEAD")
        .ok()
        .flatten()
        .and_then(|r| r.target().try_name().map(|n| n.to_string()))
    })
}

pub fn read_local_config_value(repo: &Repository, key: &str) -> Result<Option<String>> {
  let path = repo.git_dir().join("config");
  if !path.exists() {
    return Ok(None);
  }
  let config = gix::config::File::from_path_no_includes(path, gix::config::Source::Local)
    .context("read local git config")?;
  let value = match config.raw_value(key) {
    Ok(v) => v.to_str().ok().map(|s| s.to_string()),
    Err(_) => None,
  };
  Ok(value)
}

pub fn read_notes_ref(repo: &Repository) -> String {
  read_local_config_value(repo, CONFIG_NOTES_REF_KEY)
    .ok()
    .flatten()
    .unwrap_or_else(|| DEFAULT_NOTES_REF.to_string())
}

pub fn ensure_notes_ref(repo: &Repository, notes_ref: &str) -> Result<()> {
  if repo
    .try_find_reference(notes_ref)
    .context("find notes ref")?
    .is_some()
  {
    return Ok(());
  }

  let Some(remote) = select_notes_remote(repo)? else {
    return Ok(());
  };

  if !remote_has_notes_ref(repo, &remote, notes_ref)? {
    return Ok(());
  }

  fetch_notes_ref(repo, &remote, notes_ref)
    .with_context(|| format!("fetch {notes_ref} from {remote}"))?;

  Ok(())
}

fn select_notes_remote(repo: &Repository) -> Result<Option<String>> {
  if let Some(remote) = read_local_config_value(repo, CONFIG_NOTES_REMOTE_KEY)
    .ok()
    .flatten()
  {
    return Ok(Some(remote));
  }

  if let Some(branch) = current_branch_name(repo)? {
    let key = format!("branch.{branch}.remote");
    if let Some(remote) = read_local_config_value(repo, &key).ok().flatten() {
      return Ok(Some(remote));
    }
  }

  let remotes = list_remotes(repo)?;
  if remotes.is_empty() {
    return Ok(None);
  }
  if remotes.iter().any(|r| r == "origin") {
    return Ok(Some("origin".to_string()));
  }
  Ok(remotes.into_iter().next())
}

fn current_branch_name(repo: &Repository) -> Result<Option<String>> {
  let Some(name) = repo.head_name().context("read HEAD name")? else {
    return Ok(None);
  };
  let short = name.shorten();
  Ok(
    short
      .to_str()
      .ok()
      .map(|s| s.to_string())
      .or_else(|| Some(short.to_str_lossy().to_string())),
  )
}

fn list_remotes(repo: &Repository) -> Result<Vec<String>> {
  let path = repo.git_dir().join("config");
  if !path.exists() {
    return Ok(Vec::new());
  }
  let config = gix::config::File::from_path_no_includes(path, gix::config::Source::Local)
    .context("read local git config")?;
  let mut remotes = Vec::new();
  if let Some(sections) = config.sections_by_name("remote") {
    for section in sections {
      if let Some(name) = section.header().subsection_name() {
        let remote = name
          .to_str()
          .map(|s| s.to_string())
          .unwrap_or_else(|_| name.to_str_lossy().to_string());
        remotes.push(remote);
      }
    }
  }
  remotes.sort();
  remotes.dedup();
  Ok(remotes)
}

fn fetch_notes_ref(repo: &Repository, remote: &str, notes_ref: &str) -> Result<()> {
  let workdir = repo
    .workdir()
    .map(ToOwned::to_owned)
    .unwrap_or_else(|| repo.git_dir().to_path_buf());

  let refspec = format!("+{notes_ref}:{notes_ref}");
  let status = Command::new("git")
    .arg("-C")
    .arg(workdir)
    .arg("fetch")
    .arg("--no-tags")
    .arg(remote)
    .arg(refspec)
    .status()
    .context("spawn git fetch")?;

  if !status.success() {
    anyhow::bail!("git fetch failed");
  }

  Ok(())
}

fn remote_has_notes_ref(repo: &Repository, remote: &str, notes_ref: &str) -> Result<bool> {
  let workdir = repo
    .workdir()
    .map(ToOwned::to_owned)
    .unwrap_or_else(|| repo.git_dir().to_path_buf());

  let output = Command::new("git")
    .arg("-C")
    .arg(workdir)
    .arg("ls-remote")
    .arg("--refs")
    .arg(remote)
    .arg(notes_ref)
    .output()
    .context("spawn git ls-remote")?;

  if !output.status.success() {
    anyhow::bail!("git ls-remote failed");
  }

  Ok(!output.stdout.is_empty())
}

pub fn write_local_config_value(repo: &Repository, key: &str, value: &str) -> Result<()> {
  let path = repo.git_dir().join("config");
  let mut config = if path.exists() {
    gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
      .context("read local git config")?
  } else {
    let mut buf = Vec::new();
    let meta = gix::config::file::Metadata::from(gix::config::Source::Local).at(path.clone());
    gix::config::File::from_bytes_owned(&mut buf, meta, Default::default())
      .context("init local git config")?
  };

  let (section, value_name) = split_config_key(key)?;
  config
    .set_raw_value_by(section, None, value_name.to_string(), value)
    .context("set local git config value")?;

  let mut out = Vec::new();
  config.write_to(&mut out).context("serialize git config")?;
  std::fs::write(&path, out).with_context(|| format!("write {}", path.display()))?;
  Ok(())
}

fn split_config_key(key: &str) -> Result<(&str, &str)> {
  let Some((section, value)) = key.split_once('.') else {
    return Err(anyhow::anyhow!("invalid config key: {key}"));
  };
  if section.is_empty() || value.is_empty() {
    return Err(anyhow::anyhow!("invalid config key: {key}"));
  }
  Ok((section, value))
}

pub fn head_commit_oid(repo: &Repository) -> Result<ObjectId> {
  Ok(repo.head_commit().context("read HEAD commit")?.id)
}

pub fn normalize_repo_path(repo: &Repository, path: &str) -> String {
  let mut out = path.to_string();
  if let Some(stripped) = out.strip_prefix("./") {
    out = stripped.to_string();
  }
  if let Some(wd) = repo.workdir() {
    let p = std::path::Path::new(&out);
    if p.is_absolute()
      && let Ok(rel) = p.strip_prefix(wd)
    {
      out = rel.to_string_lossy().to_string();
    }
  }
  out
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnstagedStatus {
  Changed(char),
  Untracked,
  Ignored,
}

pub fn list_unstaged(
  repo: &Repository,
  include_ignored: bool,
) -> Result<Vec<(String, UnstagedStatus)>> {
  let mut out = BTreeMap::<String, UnstagedStatus>::new();
  let workdir = repo.workdir().map(ToOwned::to_owned);
  let mut platform = repo.status(gix::progress::Discard).context("init status")?;
  platform = platform.untracked_files(gix::status::UntrackedFiles::Files);
  if include_ignored {
    platform = platform.dirwalk_options(|opts| opts.emit_ignored(Some(EmissionMode::Matching)));
  }

  let iter = platform
    .into_index_worktree_iter(Vec::new())
    .context("status index-worktree")?;

  for item in iter {
    let item = item.context("status item")?;

    let status = match item {
      gix::status::index_worktree::Item::DirectoryContents { ref entry, .. } => {
        match entry.status {
          gix_dir::entry::Status::Untracked => UnstagedStatus::Untracked,
          gix_dir::entry::Status::Ignored(_) if include_ignored => UnstagedStatus::Ignored,
          _ => continue,
        }
      }
      _ => {
        let Some(summary) = item.summary() else {
          continue;
        };
        use gix::status::index_worktree::iter::Summary as S;
        let code = match summary {
          S::Conflict => 'U',
          S::Modified => 'M',
          S::Removed => 'D',
          S::TypeChange => 'T',
          S::IntentToAdd => 'A',
          S::Added => 'A',
          S::Renamed => 'R',
          S::Copied => 'C',
        };
        UnstagedStatus::Changed(code)
      }
    };

    let path = item.rela_path().to_str_lossy().into_owned();
    if let Some(wt) = &workdir {
      let abs = wt.join(&path);
      if abs.is_dir() {
        // `gix status` may report untracked directories; we only review files.
        continue;
      }
    }
    out.insert(path, status);
  }

  Ok(out.into_iter().collect())
}

pub fn list_unstaged_paths(repo: &Repository, include_ignored: bool) -> Result<Vec<String>> {
  let mut out: Vec<String> = list_unstaged(repo, include_ignored)?
    .into_iter()
    .map(|(p, _)| p)
    .collect();
  out.sort();
  Ok(out)
}

pub fn list_staged_status(repo: &Repository) -> Result<BTreeMap<String, char>> {
  let head_tree_id = repo.head_tree_id().context("get HEAD tree id")?;
  let index = repo
    .index_or_load_from_head_or_empty()
    .context("open worktree index")?;

  let mut out = BTreeMap::<String, char>::new();
  repo
    .tree_index_status(
      &head_tree_id,
      &index,
      None,
      gix::status::tree_index::TrackRenames::Disabled,
      |change, _, _| {
        use gix::diff::index::ChangeRef as C;
        let (path, code) = match change {
          C::Addition { location, .. } => (location.to_str_lossy().into_owned(), 'A'),
          C::Deletion { location, .. } => (location.to_str_lossy().into_owned(), 'D'),
          C::Modification { location, .. } => (location.to_str_lossy().into_owned(), 'M'),
          C::Rewrite { location, copy, .. } => (
            location.to_str_lossy().into_owned(),
            if copy { 'C' } else { 'R' },
          ),
        };
        out.insert(path, code);
        Ok::<_, std::convert::Infallible>(gix::diff::index::Action::Continue)
      },
    )
    .context("tree-index status")?;

  Ok(out)
}

pub fn list_staged_paths(repo: &Repository) -> Result<Vec<String>> {
  let mut out: Vec<String> = list_staged_status(repo)?.into_keys().collect();
  out.sort();
  Ok(out)
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn list_unstaged_paths_includes_untracked_files() {
    let td = tempfile::tempdir().expect("tempdir");
    let repo = gix::init(td.path()).expect("init repo");

    std::fs::write(td.path().join("untracked.txt"), "hello").expect("write untracked");
    std::fs::create_dir_all(td.path().join("dir")).expect("mkdir");
    std::fs::write(td.path().join("dir/nested.rs"), "fn main() {}").expect("write nested");
    std::fs::create_dir_all(td.path().join("empty-dir")).expect("mkdir empty");

    let mut paths = list_unstaged_paths(&repo, false).expect("list paths");
    paths.sort();

    assert!(paths.contains(&"untracked.txt".to_string()));
    assert!(paths.contains(&"dir/nested.rs".to_string()));
    assert!(!paths.contains(&"dir".to_string()));
    assert!(!paths.contains(&"empty-dir".to_string()));
  }

  #[test]
  fn list_unstaged_paths_can_include_ignored_files() {
    let td = tempfile::tempdir().expect("tempdir");
    let repo = gix::init(td.path()).expect("init repo");

    std::fs::write(td.path().join(".gitignore"), "ignored.txt\n").expect("write gitignore");
    std::fs::write(td.path().join("ignored.txt"), "nope").expect("write ignored");

    let paths_hidden = list_unstaged_paths(&repo, false).expect("list paths");
    assert!(!paths_hidden.contains(&"ignored.txt".to_string()));

    let paths_shown = list_unstaged_paths(&repo, true).expect("list paths");
    assert!(paths_shown.contains(&"ignored.txt".to_string()));
  }
}
