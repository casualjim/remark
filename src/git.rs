use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use gix::ObjectId;
use gix::bstr::ByteSlice;

#[derive(Debug, Clone)]
pub struct CommitEntry {
    pub id: ObjectId,
    pub short_id: String,
    pub summary: String,
}

pub fn list_commits(repo: &gix::Repository, limit: usize) -> Result<Vec<CommitEntry>> {
    let head = repo.head_id().context("read HEAD")?.detach();
    let mut walk = repo
        .rev_walk([head])
        .all()
        .context("start revision walk")?;

    let mut commits = Vec::new();
    while commits.len() < limit {
        let Some(info) = walk.next() else { break };
        let info = info?;
        let id = info.id;
        let commit = repo.find_commit(id).context("find commit")?;
        let summary = commit
            .message_raw_sloppy()
            .lines()
            .next()
            .map(|l| l.to_str_lossy().into_owned())
            .unwrap_or_default();
        let full = id.to_string();
        let short_id = full.chars().take(8).collect::<String>();
        commits.push(CommitEntry {
            id,
            short_id,
            summary,
        });
    }

    Ok(commits)
}

pub struct GitBackend {
    workdir: PathBuf,
    pub notes_ref: String,
}

impl GitBackend {
    pub fn new(workdir: PathBuf, notes_ref: String) -> Self {
        Self { workdir, notes_ref }
    }

    pub fn read_note(&self, oid: &ObjectId) -> Result<Option<String>> {
        let out = self.git_cmd()
            .args([
                "notes",
                &format!("--ref={}", self.notes_ref),
                "show",
                &oid.to_string(),
            ])
            .output()
            .context("run git notes show")?;

        if out.status.success() {
            return Ok(Some(String::from_utf8_lossy(&out.stdout).to_string()));
        }

        // No note is a normal case: exit 1, message on stderr.
        if out.status.code() == Some(1) {
            return Ok(None);
        }

        bail!(
            "git notes show failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    }

    pub fn write_note(&self, oid: &ObjectId, text: &str) -> Result<()> {
        let text = text.trim_end_matches(['\n', '\r']);
        if text.is_empty() {
            let out = self.git_cmd()
                .args([
                    "notes",
                    &format!("--ref={}", self.notes_ref),
                    "remove",
                    &oid.to_string(),
                ])
                .output()
                .context("run git notes remove")?;
            if out.status.success() || out.status.code() == Some(1) {
                return Ok(());
            }
            bail!(
                "git notes remove failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        }

        let mut tmp = tempfile::NamedTempFile::new().context("create temp file for note")?;
        std::io::Write::write_all(&mut tmp, text.as_bytes()).context("write note temp file")?;
        std::io::Write::write_all(&mut tmp, b"\n").context("write note temp file newline")?;

        let out = self
            .git_cmd()
            .args([
                "notes",
                &format!("--ref={}", self.notes_ref),
                "add",
                "-f",
                "-F",
                tmp.path()
                    .to_str()
                    .context("temp file path is not utf-8")?,
                &oid.to_string(),
            ])
            .output()
            .context("run git notes add")?;
        if out.status.success() {
            return Ok(());
        }
        bail!(
            "git notes add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    }

    pub fn diff_commit(&self, oid: &ObjectId, width: u16) -> Result<String> {
        let out = self
            .git_cmd()
            .args([
                "--no-pager",
                "-c",
                "diff.external=difft",
                "show",
                "--ext-diff",
                "--pretty=format:",
                &oid.to_string(),
            ])
            .env("DFT_COLOR", "never")
            .env("DFT_SYNTAX_HIGHLIGHT", "off")
            .env("DFT_BACKGROUND", "dark")
            .env("DFT_WIDTH", width.to_string())
            .output()
            .context("run git show with difftastic")?;

        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).to_string());
        }

        let fallback = self
            .git_cmd()
            .args([
                "--no-pager",
                "-c",
                "color.ui=false",
                "show",
                "--pretty=format:",
                "--patch",
                &oid.to_string(),
            ])
            .output()
            .context("run git show fallback")?;
        if fallback.status.success() {
            let mut out = String::new();
            out.push_str("difftastic unavailable; showing unified diff\n\n");
            out.push_str(&String::from_utf8_lossy(&fallback.stdout));
            return Ok(out);
        }

        bail!(
            "diff failed; difftastic stderr: {}; git stderr: {}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&fallback.stderr)
        )
    }

    fn git_cmd(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.workdir);
        cmd
    }
}
