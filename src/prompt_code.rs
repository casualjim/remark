use std::collections::HashMap;
use std::path::Path;

use crate::git::ViewKind;
use crate::review::{LineKey, LineSide};

#[derive(Debug, Default)]
struct LineContentMap {
    before: Option<Vec<String>>,
    after: Option<Vec<String>>,
}

pub struct LineSnippetResolver<'a> {
    repo: &'a gix::Repository,
    base_tree: Option<gix::Tree<'a>>,
    context_lines: u32,
    view_order: Vec<ViewKind>,
    cache: HashMap<(ViewKind, String), LineContentMap>,
}

impl<'a> LineSnippetResolver<'a> {
    pub fn new(
        repo: &'a gix::Repository,
        base_tree: Option<gix::Tree<'a>>,
        context_lines: u32,
        view_order: Vec<ViewKind>,
    ) -> Self {
        Self {
            repo,
            base_tree,
            context_lines,
            view_order,
            cache: HashMap::new(),
        }
    }

    pub fn snippet(&mut self, path: &str, key: LineKey) -> Option<String> {
        let views = self.view_order.clone();
        let context_lines = self.context_lines;
        for view in views {
            if let Some(map) = self.map_for_view_path(view, path) {
                let lines = match key.side {
                    LineSide::Old => map.before.as_ref(),
                    LineSide::New => map.after.as_ref(),
                };
                if let Some(lines) = lines
                    && let Some(snippet) = snippet_from_lines(lines, key.line, context_lines)
                {
                    return Some(snippet);
                }
            }
        }
        None
    }

    fn map_for_view_path(&mut self, view: ViewKind, path: &str) -> Option<&LineContentMap> {
        let cache_key = (view, path.to_string());
        if !self.cache.contains_key(&cache_key) {
            let map = self.build_map_for_view_path(view, path).unwrap_or_default();
            self.cache.insert(cache_key.clone(), map);
        }
        self.cache.get(&cache_key)
    }

    fn build_map_for_view_path(
        &self,
        view: ViewKind,
        path: &str,
    ) -> anyhow::Result<LineContentMap> {
        if view == ViewKind::Base && self.base_tree.is_none() {
            return Ok(LineContentMap::default());
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

        Ok(LineContentMap {
            before: before.map(|s| split_lines(&s)),
            after: after.map(|s| split_lines(&s)),
        })
    }
}

fn split_lines(content: &str) -> Vec<String> {
    content.lines().map(|line| line.to_string()).collect()
}

fn snippet_from_lines(lines: &[String], line: u32, context: u32) -> Option<String> {
    if line == 0 {
        return None;
    }
    let idx = (line - 1) as usize;
    if idx >= lines.len() {
        return None;
    }
    let context = context as usize;
    let start = idx.saturating_sub(context);
    let end = (idx + context + 1).min(lines.len());
    Some(lines[start..end].join("\n"))
}

pub fn language_for_path(path: &str) -> String {
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "go" => "go",
        "py" => "python",
        "js" => "javascript",
        "jsx" => "jsx",
        "ts" => "typescript",
        "tsx" => "tsx",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "sh" => "sh",
        "bash" => "bash",
        "zsh" => "zsh",
        "c" => "c",
        "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "md" => "markdown",
        _ => "text",
    }
    .to_string()
}
