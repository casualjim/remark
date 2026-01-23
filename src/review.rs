use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Comment {
  pub body: String,
  #[serde(default)]
  pub resolved: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub snippet_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum LineSide {
  Old,
  New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LineKey {
  pub side: LineSide,
  pub line: u32,
}

impl LineKey {
  // Intentionally minimal: this is primarily a typed key for comment maps.
}

impl Serialize for LineKey {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: serde::Serializer,
  {
    let prefix = match self.side {
      LineSide::Old => "o:",
      LineSide::New => "n:",
    };
    serializer.serialize_str(&format!("{prefix}{}", self.line))
  }
}

impl<'de> Deserialize<'de> for LineKey {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: serde::Deserializer<'de>,
  {
    let s = String::deserialize(deserializer)?;
    let (side, rest) = if let Some(rest) = s.strip_prefix("o:") {
      (LineSide::Old, rest)
    } else if let Some(rest) = s.strip_prefix("n:") {
      (LineSide::New, rest)
    } else {
      // Backward compatibility: a plain number means a "new" line comment.
      (LineSide::New, s.as_str())
    };

    let line: u32 = rest
      .parse()
      .map_err(|_| serde::de::Error::custom("invalid line number"))?;
    Ok(LineKey { side, line })
  }
}

#[derive(Debug, Clone)]
pub struct Review {
  pub files: BTreeMap<String, FileReview>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileReview {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub file_comment: Option<Comment>,
  #[serde(default)]
  pub comments: BTreeMap<LineKey, Comment>,
  #[serde(default, skip_serializing_if = "is_false")]
  pub reviewed: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub reviewed_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentState {
  None,
  ResolvedOnly,
  HasUnresolved,
}

#[derive(Debug, Clone)]
pub struct PromptSnippet {
  pub code: String,
  pub lang: String,
}

impl Review {
  pub fn new() -> Self {
    Self {
      files: BTreeMap::new(),
    }
  }

  pub fn set_file_comment(&mut self, path: &str, comment: String) {
    let f = self.files.entry(path.to_string()).or_default();
    if comment.trim().is_empty() {
      f.file_comment = None;
    } else {
      let resolved = f.file_comment.as_ref().map(|c| c.resolved).unwrap_or(false);
      f.file_comment = Some(Comment {
        body: comment,
        resolved,
        snippet_hash: None,
      });
    }
    if f.file_comment.is_none() && f.comments.is_empty() && !f.reviewed {
      self.files.remove(path);
    }
  }

  pub fn file_comment(&self, path: &str) -> Option<&Comment> {
    self.files.get(path).and_then(|f| f.file_comment.as_ref())
  }

  pub fn remove_file_comment(&mut self, path: &str) -> bool {
    let Some(f) = self.files.get_mut(path) else {
      return false;
    };
    let removed = f.file_comment.take().is_some();
    if f.file_comment.is_none() && f.comments.is_empty() && !f.reviewed {
      self.files.remove(path);
    }
    removed
  }

  pub fn toggle_file_comment_resolved(&mut self, path: &str) -> Option<bool> {
    let f = self.files.get_mut(path)?;
    let c = f.file_comment.as_mut()?;
    c.resolved = !c.resolved;
    Some(c.resolved)
  }

  pub fn set_line_comment(
    &mut self,
    path: &str,
    side: LineSide,
    line_1_based: u32,
    comment: String,
  ) {
    let f = self.files.entry(path.to_string()).or_default();
    if comment.trim().is_empty() {
      f.comments.remove(&LineKey {
        side,
        line: line_1_based,
      });
    } else {
      let key = LineKey {
        side,
        line: line_1_based,
      };
      let resolved = f.comments.get(&key).map(|c| c.resolved).unwrap_or(false);
      let snippet_hash = f.comments.get(&key).and_then(|c| c.snippet_hash.clone());
      f.comments.insert(
        key,
        Comment {
          body: comment,
          resolved,
          snippet_hash,
        },
      );
    }
    if f.file_comment.is_none() && f.comments.is_empty() && !f.reviewed {
      self.files.remove(path);
    }
  }

  pub fn set_line_comment_snippet_hash(
    &mut self,
    path: &str,
    side: LineSide,
    line_1_based: u32,
    hash: Option<String>,
  ) {
    let Some(f) = self.files.get_mut(path) else {
      return;
    };
    let key = LineKey {
      side,
      line: line_1_based,
    };
    if let Some(comment) = f.comments.get_mut(&key) {
      comment.snippet_hash = hash;
    }
  }

  pub fn line_comment(&self, path: &str, side: LineSide, line_1_based: u32) -> Option<&Comment> {
    self.files.get(path).and_then(|f| {
      f.comments.get(&LineKey {
        side,
        line: line_1_based,
      })
    })
  }

  pub fn remove_line_comment(&mut self, path: &str, side: LineSide, line_1_based: u32) -> bool {
    let Some(f) = self.files.get_mut(path) else {
      return false;
    };
    let removed = f
      .comments
      .remove(&LineKey {
        side,
        line: line_1_based,
      })
      .is_some();
    if f.file_comment.is_none() && f.comments.is_empty() && !f.reviewed {
      self.files.remove(path);
    }
    removed
  }

  pub fn toggle_line_comment_resolved(
    &mut self,
    path: &str,
    side: LineSide,
    line_1_based: u32,
  ) -> Option<bool> {
    let f = self.files.get_mut(path)?;
    let c = f.comments.get_mut(&LineKey {
      side,
      line: line_1_based,
    })?;
    c.resolved = !c.resolved;
    Some(c.resolved)
  }

  pub fn prune_line_comments(
    &mut self,
    path: &str,
    valid_old: &HashSet<u32>,
    valid_new: &HashSet<u32>,
  ) -> bool {
    let Some(f) = self.files.get_mut(path) else {
      return false;
    };

    let before = f.comments.len();
    f.comments.retain(|k, _| match k.side {
      LineSide::Old => valid_old.contains(&k.line),
      LineSide::New => valid_new.contains(&k.line),
    });
    let mut changed = f.comments.len() != before;

    if f.file_comment.is_none()
      && f.comments.is_empty()
      && !f.reviewed
      && self.files.remove(path).is_some()
    {
      changed = true;
    }

    changed
  }

  pub fn comment_state(&self, path: &str) -> CommentState {
    let Some(f) = self.files.get(path) else {
      return CommentState::None;
    };
    let mut saw_any = false;
    if let Some(c) = f.file_comment.as_ref() {
      saw_any = true;
      if !c.resolved {
        return CommentState::HasUnresolved;
      }
    }
    for c in f.comments.values() {
      saw_any = true;
      if !c.resolved {
        return CommentState::HasUnresolved;
      }
    }
    if saw_any {
      CommentState::ResolvedOnly
    } else {
      CommentState::None
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileNote {
  version: u8,
  file: FileReview,
}

pub fn encode_file_note(file: &FileReview) -> String {
  let json = serde_json::to_string_pretty(&FileNote {
    version: 2,
    file: file.clone(),
  })
  .unwrap_or_else(|_| "{}".to_string());
  format!("<!-- remark-file:2 -->\n```json\n{json}\n```\n")
}

pub fn decode_file_note(note: &str) -> Option<FileReview> {
  let mut lines = note.lines();
  while let Some(line) = lines.next() {
    if line.trim() == "```json" {
      let mut json_lines = Vec::new();
      for l in &mut lines {
        if l.trim() == "```" {
          break;
        }
        json_lines.push(l);
      }
      let json = json_lines.join("\n");
      if let Ok(v) = serde_json::from_str::<FileNote>(&json)
        && v.version == 2
      {
        return Some(v.file);
      }

      #[derive(Debug, Clone, Deserialize)]
      struct FileNoteV1 {
        version: u8,
        file: FileReviewV1,
      }
      #[derive(Debug, Clone, Default, Deserialize)]
      struct FileReviewV1 {
        #[serde(default)]
        file_comment: Option<String>,
        #[serde(default)]
        comments: BTreeMap<LineKey, String>,
      }

      let v1 = serde_json::from_str::<FileNoteV1>(&json).ok()?;
      if v1.version != 1 {
        return None;
      }
      return Some(FileReview {
        file_comment: v1.file.file_comment.map(|body| Comment {
          body,
          resolved: false,
          snippet_hash: None,
        }),
        comments: v1
          .file
          .comments
          .into_iter()
          .map(|(k, body)| {
            (
              k,
              Comment {
                body,
                resolved: false,
                snippet_hash: None,
              },
            )
          })
          .collect(),
        reviewed: false,
        reviewed_hash: None,
      });
    }
  }
  None
}

pub fn render_prompt<F>(review: &Review, mut line_code: F) -> String
where
  F: FnMut(&str, LineKey) -> Option<PromptSnippet>,
{
  let mut out = String::new();
  if review.files.is_empty() {
    out.push_str("No comments.\n");
    return out;
  }

  let mut wrote_any = false;

  out.push_str(
    "# Review Notes\n\n\
These notes are grouped by file. Every comment belongs to the file section it appears under.\n\n",
  );

  for (path, f) in &review.files {
    let file_comment = f
      .file_comment
      .as_ref()
      .filter(|c| !c.resolved)
      .map(|c| c.body.trim_end())
      .filter(|s| !s.is_empty());

    let line_comments = f
      .comments
      .iter()
      .filter(|(_, c)| !c.resolved && !c.body.trim().is_empty())
      .map(|(k, c)| (k, c.body.trim_end()))
      .collect::<Vec<_>>();

    if file_comment.is_none() && line_comments.is_empty() {
      continue;
    }

    wrote_any = true;
    out.push_str(&format!("## {path}\n"));

    if let Some(fc) = file_comment {
      out.push_str("### File comment\n");
      push_fenced_block(&mut out, fc);
      out.push('\n');
    }

    if !line_comments.is_empty() {
      out.push_str("### Line comments\n");
      for (line, comment) in line_comments {
        out.push_str(&format!("- line {}", line.line));
        if line.side == LineSide::Old {
          out.push_str(" (old)");
        }
        out.push('\n');
        if let Some(snippet) = line_code(path, *line) {
          push_fenced_block_with_lang(&mut out, &snippet.code, &snippet.lang);
        }
        push_fenced_block(&mut out, comment);
      }
    }
    out.push('\n');
  }

  if !wrote_any {
    out.push_str("No comments.\n");
    return out;
  }
  out
}

fn push_fenced_block(out: &mut String, text: &str) {
  push_fenced_block_with_lang(out, text, "text");
}

fn push_fenced_block_with_lang(out: &mut String, text: &str, lang: &str) {
  let mut ticks = 3usize;
  loop {
    let fence = "`".repeat(ticks);
    if !text.contains(&fence) {
      out.push_str(&format!("{fence}{lang}\n"));
      out.push_str(text);
      if !text.ends_with('\n') {
        out.push('\n');
      }
      out.push_str(&format!("{fence}\n"));
      return;
    }
    ticks += 1;
  }
}

fn is_false(v: &bool) -> bool {
  !*v
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn line_key_plain_number_is_new_side() {
    let k: LineKey = serde_json::from_str("\"12\"").unwrap();
    assert_eq!(k.side, LineSide::New);
    assert_eq!(k.line, 12);
  }

  #[test]
  fn line_key_roundtrip_has_prefix() {
    let k = LineKey {
      side: LineSide::Old,
      line: 7,
    };
    let s = serde_json::to_string(&k).unwrap();
    assert_eq!(s, "\"o:7\"");
    let k2: LineKey = serde_json::from_str(&s).unwrap();
    assert_eq!(k2.side, LineSide::Old);
    assert_eq!(k2.line, 7);
  }

  #[test]
  fn file_note_roundtrip_v2() {
    let mut fr = FileReview {
      file_comment: Some(Comment {
        body: "file-level".to_string(),
        resolved: true,
        snippet_hash: None,
      }),
      reviewed: true,
      reviewed_hash: Some("abc123".to_string()),
      ..Default::default()
    };
    fr.comments.insert(
      LineKey {
        side: LineSide::New,
        line: 3,
      },
      Comment {
        body: "hello".to_string(),
        resolved: false,
        snippet_hash: None,
      },
    );
    fr.comments.insert(
      LineKey {
        side: LineSide::Old,
        line: 1,
      },
      Comment {
        body: "restore this".to_string(),
        resolved: true,
        snippet_hash: None,
      },
    );

    let note = encode_file_note(&fr);
    let decoded = decode_file_note(&note).unwrap();
    assert_eq!(
      decoded.file_comment.as_ref().unwrap().body,
      "file-level".to_string()
    );
    assert!(decoded.file_comment.as_ref().unwrap().resolved);
    assert!(decoded.reviewed);
    assert_eq!(decoded.reviewed_hash.as_deref(), Some("abc123"));
    assert_eq!(decoded.comments.len(), 2);
    assert_eq!(
      decoded
        .comments
        .get(&LineKey {
          side: LineSide::New,
          line: 3
        })
        .unwrap()
        .body,
      "hello"
    );
    assert!(
      !decoded
        .comments
        .get(&LineKey {
          side: LineSide::New,
          line: 3
        })
        .unwrap()
        .resolved
    );
  }

  #[test]
  fn file_note_v1_upgrades_to_v2_comments_unresolved() {
    let note = r#"<!-- remark-file:1 -->
```json
{
  "version": 1,
  "file": {
    "file_comment": "do the thing",
    "comments": {
      "12": "new side (implicit)",
      "o:2": "old side",
      "n:3": "new side"
    }
  }
}
```"#;

    let fr = decode_file_note(note).unwrap();
    assert_eq!(fr.file_comment.as_ref().unwrap().body, "do the thing");
    assert!(!fr.file_comment.as_ref().unwrap().resolved);
    assert_eq!(fr.comments.len(), 3);
    assert_eq!(
      fr.comments
        .get(&LineKey {
          side: LineSide::New,
          line: 12
        })
        .unwrap()
        .body,
      "new side (implicit)"
    );
    assert!(
      !fr
        .comments
        .get(&LineKey {
          side: LineSide::Old,
          line: 2
        })
        .unwrap()
        .resolved
    );
  }

  #[test]
  fn prompt_omits_resolved_comments() {
    let mut r = Review::new();

    r.set_file_comment("a.rs", "File todo".to_string());
    r.set_line_comment("a.rs", LineSide::New, 10, "Fix this".to_string());
    r.set_line_comment("a.rs", LineSide::Old, 2, "Restore this".to_string());

    // Resolve one line and the file comment; only the unresolved line should remain.
    r.toggle_file_comment_resolved("a.rs");
    r.toggle_line_comment_resolved("a.rs", LineSide::Old, 2);

    // File with only resolved comments should not appear.
    r.set_file_comment("b.rs", "All done".to_string());
    r.toggle_file_comment_resolved("b.rs");

    let p = render_prompt(&r, |_, _| None);
    assert!(!p.contains("Target:"));
    assert!(!p.contains("Base:"));
    assert!(p.contains("## a.rs"));
    assert!(p.contains("- line 10"));
    assert!(p.contains("Fix this"));
    assert!(!p.contains("File todo"));
    assert!(!p.contains("Restore this"));
    assert!(!p.contains("## b.rs"));
    assert!(!p.contains("## TODOs"));
  }

  #[test]
  fn prune_line_comments_removes_unreachable() {
    let mut r = Review::new();
    r.set_line_comment("a.rs", LineSide::New, 10, "Fix this".to_string());
    r.set_line_comment("a.rs", LineSide::Old, 2, "Restore this".to_string());

    let mut valid_old = HashSet::new();
    valid_old.insert(2);
    let mut valid_new = HashSet::new();
    valid_new.insert(11);

    assert!(r.prune_line_comments("a.rs", &valid_old, &valid_new));
    assert!(r.line_comment("a.rs", LineSide::Old, 2).is_some());
    assert!(r.line_comment("a.rs", LineSide::New, 10).is_none());
  }

  #[test]
  fn prompt_shows_no_comments_when_all_resolved() {
    let mut r = Review::new();
    r.set_file_comment("a.rs", "done".to_string());
    r.toggle_file_comment_resolved("a.rs");
    let p = render_prompt(&r, |_, _| None);
    assert!(p.contains("No comments."));
  }

  #[test]
  fn prompt_does_not_duplicate_file_comments_as_todos() {
    let mut r = Review::new();
    r.set_file_comment("a.rs", "Do thing".to_string());
    let p = render_prompt(&r, |_, _| None);
    assert!(p.contains("## a.rs"));
    assert!(p.contains("### File comment"));
    assert!(p.contains("Do thing"));
    assert!(!p.contains("## TODOs"));
  }

  #[test]
  fn prompt_includes_line_code_when_available() {
    let mut r = Review::new();
    r.set_line_comment("a.rs", LineSide::New, 10, "Fix this".to_string());

    let p = render_prompt(&r, |path, key| {
      if path == "a.rs" && key.side == LineSide::New && key.line == 10 {
        Some(PromptSnippet {
          code: "let x = 1;".to_string(),
          lang: "rust".to_string(),
        })
      } else {
        None
      }
    });

    assert!(p.contains("```rust"));
    assert!(p.contains("let x = 1;"));
    assert!(p.contains("Fix this"));
  }
}
