use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub version: u8,
    pub kind: String,
    pub base_ref: Option<String>,
    pub files: BTreeMap<String, FileReview>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileReview {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_comment: Option<String>,
    #[serde(default)]
    pub comments: BTreeMap<LineKey, String>,
}

impl Review {
    pub fn new(kind: impl Into<String>, base_ref: Option<String>) -> Self {
        Self {
            version: 2,
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
            f.file_comment = Some(comment);
        }
        if f.file_comment.is_none() && f.comments.is_empty() {
            self.files.remove(path);
        }
    }

    pub fn file_comment(&self, path: &str) -> Option<&str> {
        self.files
            .get(path)
            .and_then(|f| f.file_comment.as_deref())
    }

    pub fn remove_file_comment(&mut self, path: &str) -> bool {
        let Some(f) = self.files.get_mut(path) else { return false };
        let removed = f.file_comment.take().is_some();
        if f.file_comment.is_none() && f.comments.is_empty() {
            self.files.remove(path);
        }
        removed
    }

    pub fn set_line_comment(&mut self, path: &str, side: LineSide, line_1_based: u32, comment: String) {
        let f = self.files.entry(path.to_string()).or_default();
        if comment.trim().is_empty() {
            f.comments.remove(&LineKey { side, line: line_1_based });
        } else {
            f.comments.insert(LineKey { side, line: line_1_based }, comment);
        }
        if f.file_comment.is_none() && f.comments.is_empty() {
            self.files.remove(path);
        }
    }

    pub fn line_comment(&self, path: &str, side: LineSide, line_1_based: u32) -> Option<&str> {
        self.files
            .get(path)
            .and_then(|f| f.comments.get(&LineKey { side, line: line_1_based }))
            .map(|s| s.as_str())
    }

    pub fn remove_line_comment(&mut self, path: &str, side: LineSide, line_1_based: u32) -> bool {
        let Some(f) = self.files.get_mut(path) else { return false };
        let removed = f
            .comments
            .remove(&LineKey { side, line: line_1_based })
            .is_some();
        if f.file_comment.is_none() && f.comments.is_empty() {
            self.files.remove(path);
        }
        removed
    }

    pub fn has_any_comments(&self, path: &str) -> bool {
        self.files
            .get(path)
            .map(|f| f.file_comment.as_deref().is_some() || !f.comments.is_empty())
            .unwrap_or(false)
    }
}

pub fn encode_note(review: &Review) -> String {
    let json = serde_json::to_string_pretty(review).unwrap_or_else(|_| "{}".to_string());
    let prompt = render_prompt(review);
    format!(
        "<!-- remark:2 -->\n```json\n{json}\n```\n\n# Review (LLM Prompt)\n\n{prompt}\n"
    )
}

pub fn decode_note(note: &str) -> Option<Review> {
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
            if let Ok(r) = serde_json::from_str::<Review>(&json) {
                return Some(r);
            }

            #[derive(Debug, Clone, Deserialize)]
            struct ReviewV1 {
                version: u8,
                kind: String,
                base_ref: Option<String>,
                files: BTreeMap<String, FileReviewV1>,
            }
            #[derive(Debug, Clone, Default, Deserialize)]
            struct FileReviewV1 {
                comments: BTreeMap<u32, String>,
            }

            let v1 = serde_json::from_str::<ReviewV1>(&json).ok()?;
            if v1.version != 1 {
                return None;
            }

            let mut out = Review::new(v1.kind, v1.base_ref);
            for (path, f) in v1.files {
                for (line, c) in f.comments {
                    out.set_line_comment(&path, LineSide::New, line, c);
                }
            }
            return Some(out);
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

    for (path, f) in &review.files {
        out.push_str(&format!("## {path}\n"));
        if let Some(fc) = f.file_comment.as_deref().map(str::trim_end).filter(|s| !s.is_empty()) {
            out.push_str(&format!("- File: {fc}\n"));
        }
        for (line, comment) in &f.comments {
            let comment = comment.trim_end();
            let side = match line.side {
                LineSide::Old => "old",
                LineSide::New => "new",
            };
            out.push_str(&format!(
                "- Line {} ({side}): {comment}\n",
                line.line
            ));
        }
        out.push('\n');
    }
    out
}
