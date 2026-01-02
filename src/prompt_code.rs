use std::collections::HashMap;

use crate::diff;
use crate::git::ViewKind;
use crate::review::{LineKey, LineSide};

#[derive(Debug, Default)]
pub struct LineCodeMap {
    old: HashMap<u32, String>,
    new: HashMap<u32, String>,
}

impl LineCodeMap {
    fn get(&self, key: LineKey) -> Option<&String> {
        match key.side {
            LineSide::Old => self.old.get(&key.line),
            LineSide::New => self.new.get(&key.line),
        }
    }
}

pub struct LineCodeResolver<'a> {
    repo: &'a gix::Repository,
    base_tree: Option<gix::Tree<'a>>,
    diff_context: u32,
    view_order: Vec<ViewKind>,
    cache: HashMap<(ViewKind, String), LineCodeMap>,
}

impl<'a> LineCodeResolver<'a> {
    pub fn new(
        repo: &'a gix::Repository,
        base_tree: Option<gix::Tree<'a>>,
        diff_context: u32,
        view_order: Vec<ViewKind>,
    ) -> Self {
        Self {
            repo,
            base_tree,
            diff_context,
            view_order,
            cache: HashMap::new(),
        }
    }

    pub fn line_code(&mut self, path: &str, key: LineKey) -> Option<String> {
        let views = self.view_order.clone();
        for view in views {
            if let Some(map) = self.map_for_view_path(view, path) {
                if let Some(code) = map.get(key) {
                    return Some(code.clone());
                }
            }
        }
        None
    }

    fn map_for_view_path(&mut self, view: ViewKind, path: &str) -> Option<&LineCodeMap> {
        let cache_key = (view, path.to_string());
        if !self.cache.contains_key(&cache_key) {
            let map = self
                .build_map_for_view_path(view, path)
                .unwrap_or_default();
            self.cache.insert(cache_key.clone(), map);
        }
        self.cache.get(&cache_key)
    }

    fn build_map_for_view_path(&self, view: ViewKind, path: &str) -> anyhow::Result<LineCodeMap> {
        if view == ViewKind::Base && self.base_tree.is_none() {
            return Ok(LineCodeMap::default());
        }

        let (before, after) = match view {
            ViewKind::All => (
                crate::git::try_read_head(self.repo, path)?,
                crate::git::try_read_worktree(self.repo, path)?,
            ),
            ViewKind::Unstaged => (
                crate::git::try_read_index(self.repo, path)?,
                crate::git::try_read_worktree(self.repo, path)?,
            ),
            ViewKind::Staged => (
                crate::git::try_read_head(self.repo, path)?,
                crate::git::try_read_index(self.repo, path)?,
            ),
            ViewKind::Base => {
                let before = self
                    .base_tree
                    .as_ref()
                    .map(|t| crate::git::try_read_tree(t, path))
                    .transpose()?
                    .flatten();
                (before, crate::git::try_read_head(self.repo, path)?)
            }
        };

        if before.is_none() && after.is_none() {
            return Ok(LineCodeMap::default());
        }

        let before_label = if before.is_some() {
            format!("a/{path}")
        } else {
            "/dev/null".to_string()
        };
        let after_label = if after.is_some() {
            format!("b/{path}")
        } else {
            "/dev/null".to_string()
        };

        let diff_lines = diff::unified_file_diff(
            &before_label,
            &after_label,
            before.as_deref(),
            after.as_deref(),
            self.diff_context,
        )?;

        Ok(build_line_code_map(diff_lines))
    }
}

fn build_line_code_map(lines: Vec<diff::Line>) -> LineCodeMap {
    let mut map = LineCodeMap::default();
    for line in lines {
        match line.kind {
            diff::Kind::Add => {
                if let Some(n) = line.new_line {
                    map.new.insert(n, line.text);
                }
            }
            diff::Kind::Remove => {
                if let Some(n) = line.old_line {
                    map.old.insert(n, line.text);
                }
            }
            diff::Kind::Context => {
                if let Some(n) = line.old_line {
                    map.old.insert(n, line.text.clone());
                }
                if let Some(n) = line.new_line {
                    map.new.insert(n, line.text);
                }
            }
            diff::Kind::FileHeader | diff::Kind::HunkHeader => {}
        }
    }
    map
}
