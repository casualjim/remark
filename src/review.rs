use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub version: u8,
    pub kind: String,
    pub base_ref: Option<String>,
    pub files: BTreeMap<String, FileReview>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileReview {
    pub comments: BTreeMap<u32, String>,
}

impl Review {
    pub fn new(kind: impl Into<String>, base_ref: Option<String>) -> Self {
        Self {
            version: 1,
            kind: kind.into(),
            base_ref,
            files: BTreeMap::new(),
        }
    }

    pub fn set_comment(&mut self, path: &str, line_1_based: u32, comment: String) {
        let f = self.files.entry(path.to_string()).or_default();
        if comment.trim().is_empty() {
            f.comments.remove(&line_1_based);
        } else {
            f.comments.insert(line_1_based, comment);
        }
        if f.comments.is_empty() {
            self.files.remove(path);
        }
    }

    pub fn comment(&self, path: &str, line_1_based: u32) -> Option<&str> {
        self.files
            .get(path)
            .and_then(|f| f.comments.get(&line_1_based))
            .map(|s| s.as_str())
    }

    pub fn remove_comment(&mut self, path: &str, line_1_based: u32) -> bool {
        let Some(f) = self.files.get_mut(path) else { return false };
        let removed = f.comments.remove(&line_1_based).is_some();
        if f.comments.is_empty() {
            self.files.remove(path);
        }
        removed
    }
}

pub fn encode_note(review: &Review) -> String {
    let json = serde_json::to_string_pretty(review).unwrap_or_else(|_| "{}".to_string());
    let prompt = render_prompt(review);
    format!(
        "<!-- git-review:1 -->\n```json\n{json}\n```\n\n# Review (LLM Prompt)\n\n{prompt}\n"
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
            return serde_json::from_str(&json).ok();
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
        for (line, comment) in &f.comments {
            let comment = comment.trim_end();
            out.push_str(&format!("- Line {line}: {comment}\n"));
        }
        out.push('\n');
    }
    out
}

