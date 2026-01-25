use std::collections::BTreeMap;

use crate::app::FileEntry;

#[derive(Debug, Clone)]
pub(crate) struct FileTreeRow {
  pub(crate) label: String,
  pub(crate) file_index: Option<usize>,
  pub(crate) is_dir: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FileTreeView {
  pub(crate) rows: Vec<FileTreeRow>,
  pub(crate) file_to_row: Vec<usize>,
}

#[derive(Debug, Default)]
struct Node {
  dirs: BTreeMap<String, Node>,
  files: BTreeMap<String, usize>,
}

impl FileTreeView {
  pub(crate) fn build(files: &[FileEntry]) -> Self {
    if files.is_empty() {
      return Self::default();
    }

    let mut root = Node::default();
    for (file_index, e) in files.iter().enumerate() {
      let mut parts = e.path.split('/').peekable();
      let mut node = &mut root;
      while let Some(part) = parts.next() {
        if parts.peek().is_none() {
          node.files.insert(part.to_string(), file_index);
        } else {
          node = node.dirs.entry(part.to_string()).or_default();
        }
      }
    }

    let mut out = Self {
      rows: Vec::new(),
      file_to_row: vec![0; files.len()],
    };
    walk_node(&root, &mut Vec::new(), true, &mut out);
    out
  }

  pub(crate) fn selected_row(&self, file_index: usize) -> Option<usize> {
    self.file_to_row.get(file_index).copied()
  }

  pub(crate) fn file_at_row(&self, row: usize) -> Option<usize> {
    self.rows.get(row)?.file_index
  }

  pub(crate) fn next_file_row(&self, current_row: usize) -> Option<usize> {
    (current_row + 1..self.rows.len()).find(|&row| self.rows[row].file_index.is_some())
  }

  pub(crate) fn prev_file_row(&self, current_row: usize) -> Option<usize> {
    (0..current_row)
      .rev()
      .find(|&row| self.rows[row].file_index.is_some())
  }
}

fn walk_node(node: &Node, prefix_stack: &mut Vec<bool>, is_root: bool, out: &mut FileTreeView) {
  // Collect directories separately from files so we can sort them independently
  let mut dirs: Vec<(String, EntryRef<'_>)> = node
    .dirs
    .iter()
    .map(|(name, child)| {
      (
        name.clone(),
        EntryRef::Dir {
          name: name.as_str(),
          child,
        },
      )
    })
    .collect();

  let mut files: Vec<(String, EntryRef<'_>)> = node
    .files
    .iter()
    .map(|(name, idx)| {
      (
        name.clone(),
        EntryRef::File {
          name: name.as_str(),
          file_index: *idx,
        },
      )
    })
    .collect();

  // Sort directories and files independently
  dirs.sort_by(|a, b| a.0.cmp(&b.0));
  files.sort_by(|a, b| a.0.cmp(&b.0));

  // Chain directories first, then files
  let entries = dirs.into_iter().chain(files);
  let total = node.dirs.len() + node.files.len();

  for (i, (_sort_key, entry)) in entries.enumerate() {
    let is_last = i + 1 == total;
    match entry {
      EntryRef::File { name, file_index } => {
        let label = if is_root {
          name.to_string()
        } else {
          format!("{}{}", tree_prefix(prefix_stack, is_last), name)
        };
        let row_idx = out.rows.len();
        out.rows.push(FileTreeRow {
          label,
          file_index: Some(file_index),
          is_dir: false,
        });
        if let Some(slot) = out.file_to_row.get_mut(file_index) {
          *slot = row_idx;
        }
      }
      EntryRef::Dir { name, child } => {
        let label = if is_root {
          format!("{name}/")
        } else {
          format!("{}{name}/", tree_prefix(prefix_stack, is_last))
        };
        out.rows.push(FileTreeRow {
          label,
          file_index: None,
          is_dir: true,
        });

        // Don't draw root-level vertical connector columns; root has no "├/└" lines.
        if !is_root {
          prefix_stack.push(is_last);
        }
        walk_node(child, prefix_stack, false, out);
        if !is_root {
          prefix_stack.pop();
        }
      }
    }
  }
}

fn tree_prefix(prefix_stack: &[bool], is_last: bool) -> String {
  let mut out = String::new();
  for &ancestor_is_last in prefix_stack {
    if ancestor_is_last {
      out.push_str("   ");
    } else {
      out.push_str("│  ");
    }
  }
  if is_last {
    out.push_str("└─ ");
  } else {
    out.push_str("├─ ");
  }
  out
}

#[derive(Clone, Copy)]
enum EntryRef<'a> {
  File { name: &'a str, file_index: usize },
  Dir { name: &'a str, child: &'a Node },
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::app::FileChangeKind;

  fn fe(path: &str) -> FileEntry {
    FileEntry {
      path: path.to_string(),
      change: FileChangeKind::Modified,
      git_xy: ['-', '-'],
    }
  }

  #[test]
  fn builds_flat_list() {
    let files = vec![fe("a.rs"), fe("b.rs")];
    let view = FileTreeView::build(&files);
    assert_eq!(view.rows.len(), 2);
    assert_eq!(view.rows[0].label, "a.rs");
    assert_eq!(view.rows[1].label, "b.rs");
    assert_eq!(view.file_to_row, vec![0, 1]);
    assert_eq!(view.file_at_row(0), Some(0));
    assert_eq!(view.file_at_row(1), Some(1));
  }

  #[test]
  fn builds_tree_with_dirs() {
    let files = vec![fe("README.md"), fe("src/app.rs"), fe("src/ui.rs")];
    let view = FileTreeView::build(&files);

    let labels: Vec<&str> = view.rows.iter().map(|r| r.label.as_str()).collect();
    // Directories now come before files: src/ comes before README.md
    assert_eq!(labels, vec!["src/", "├─ app.rs", "└─ ui.rs", "README.md"]);
    // file_to_row maps: files[0]=README.md->row3, files[1]=app.rs->row1, files[2]=ui.rs->row2
    assert_eq!(view.file_to_row, vec![3, 1, 2]);
    assert_eq!(view.file_at_row(0), None); // src/ is a dir
    assert_eq!(view.file_at_row(1), Some(1)); // app.rs
    assert_eq!(view.file_at_row(2), Some(2)); // ui.rs
    assert_eq!(view.file_at_row(3), Some(0)); // README.md
  }

  #[test]
  fn navigation_through_directories() {
    let files = vec![fe("abc/first.rs"), fe("plo/second.rs"), fe("mod.rs")];
    let view = FileTreeView::build(&files);

    eprintln!(
      "Labels: {:?}",
      view
        .rows
        .iter()
        .map(|r| r.label.as_str())
        .collect::<Vec<_>>()
    );
    eprintln!("file_to_row: {:?}", view.file_to_row);

    // Test navigation order
    let first_row = view.file_to_row[0]; // first.rs
    let next1 = view.next_file_row(first_row);
    let next2 = next1.and_then(|r| view.next_file_row(r));

    eprintln!(
      "From first.rs (row {}): next is {:?}, then {:?}",
      first_row, next1, next2
    );
  }
}
