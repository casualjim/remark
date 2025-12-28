use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Comment {
    pub body: String,
    #[serde(default)]
    pub resolved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    pub kind: String,
    pub base_ref: Option<String>,
    pub files: BTreeMap<String, FileReview>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileReview {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_comment: Option<Comment>,
    #[serde(default)]
    pub comments: BTreeMap<LineKey, Comment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentState {
    None,
    ResolvedOnly,
    HasUnresolved,
}

impl Review {
    pub fn new(kind: impl Into<String>, base_ref: Option<String>) -> Self {
        Self {
            kind: kind.into(),
            base_ref,
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
            });
        }
        if f.file_comment.is_none() && f.comments.is_empty() {
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
        if f.file_comment.is_none() && f.comments.is_empty() {
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
            f.comments.insert(
                key,
                Comment {
                    body: comment,
                    resolved,
                },
            );
        }
        if f.file_comment.is_none() && f.comments.is_empty() {
            self.files.remove(path);
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
        if f.file_comment.is_none() && f.comments.is_empty() {
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
                            },
                        )
                    })
                    .collect(),
            });
        }
    }
    None
}

pub fn render_prompt(review: &Review) -> String {
    let mut out = String::new();
    out.push_str(&format!("Target: {}\n", review.kind));
    if let Some(b) = &review.base_ref {
        out.push_str(&format!("Base: {b}\n"));
    }
    out.push('\n');
    if review.files.is_empty() {
        out.push_str("No comments.\n");
        return out;
    }

    let mut todos: Vec<(String, String)> = Vec::new();
    let mut wrote_any = false;

    for (path, f) in &review.files {
        let has_unresolved = f
            .file_comment
            .as_ref()
            .map(|c| !c.resolved && !c.body.trim().is_empty())
            .unwrap_or(false)
            || f.comments
                .values()
                .any(|c| !c.resolved && !c.body.trim().is_empty());
        if !has_unresolved {
            continue;
        }
        wrote_any = true;
        out.push_str(&format!("## {path}\n"));
        if let Some(fc) = f
            .file_comment
            .as_ref()
            .filter(|c| !c.resolved)
            .map(|c| c.body.trim_end())
            .filter(|s| !s.is_empty())
        {
            out.push_str(&format!("- File: {fc}\n"));
            todos.push((path.clone(), fc.to_string()));
        }
        for (line, comment) in &f.comments {
            if comment.resolved {
                continue;
            }
            let comment = comment.body.trim_end();
            if comment.is_empty() {
                continue;
            }
            let side = match line.side {
                LineSide::Old => "old",
                LineSide::New => "new",
            };
            out.push_str(&format!("- Line {} ({side}): {comment}\n", line.line));
        }
        out.push('\n');
    }

    if !wrote_any {
        out.push_str("No comments.\n");
        return out;
    }

    if !todos.is_empty() {
        out.push_str("## TODOs\n");
        for (path, c) in todos {
            out.push_str(&format!("- {path}: {c}\n"));
        }
        out.push('\n');
    }
    out
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
            }),
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
            },
        );

        let note = encode_file_note(&fr);
        let decoded = decode_file_note(&note).unwrap();
        assert_eq!(
            decoded.file_comment.as_ref().unwrap().body,
            "file-level".to_string()
        );
        assert!(decoded.file_comment.as_ref().unwrap().resolved);
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
            !fr.comments
                .get(&LineKey {
                    side: LineSide::Old,
                    line: 2
                })
                .unwrap()
                .resolved
        );
    }

    #[test]
    fn prompt_omits_resolved_comments_and_adds_todos() {
        let mut r = Review::new("all", None);

        r.set_file_comment("a.rs", "File todo".to_string());
        r.set_line_comment("a.rs", LineSide::New, 10, "Fix this".to_string());
        r.set_line_comment("a.rs", LineSide::Old, 2, "Restore this".to_string());

        // Resolve one line and the file comment; only the unresolved line should remain.
        r.toggle_file_comment_resolved("a.rs");
        r.toggle_line_comment_resolved("a.rs", LineSide::Old, 2);

        // File with only resolved comments should not appear.
        r.set_file_comment("b.rs", "All done".to_string());
        r.toggle_file_comment_resolved("b.rs");

        let p = render_prompt(&r);
        assert!(p.contains("## a.rs"));
        assert!(p.contains("Line 10 (new): Fix this"));
        assert!(!p.contains("File: File todo"));
        assert!(!p.contains("Restore this"));
        assert!(!p.contains("## b.rs"));
        assert!(!p.contains("## TODOs"));
    }

    #[test]
    fn prompt_shows_no_comments_when_all_resolved() {
        let mut r = Review::new("all", None);
        r.set_file_comment("a.rs", "done".to_string());
        r.toggle_file_comment_resolved("a.rs");
        let p = render_prompt(&r);
        assert!(p.contains("No comments."));
    }
}
