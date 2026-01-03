use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionOptions, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, Command, Diagnostic, DiagnosticSeverity, ExecuteCommandOptions,
    ExecuteCommandParams, Hover, HoverContents, InlayHint, InlayHintLabel,
    InlayHintOptions, InlayHintParams, InlayHintServerCapabilities, InitializeParams,
    InitializeResult, InitializedParams, MarkupContent, MarkupKind, MessageType, OneOf, Position,
    Range, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower_lsp::{Client, LanguageServer, LspService, Server};

use crate::config::LspCli;
use crate::git::{self, ViewKind};
use crate::review::{Comment, FileReview, LineKey, LineSide};

const COMMAND_RESOLVE: &str = "remark.resolve";
const COMMAND_UNRESOLVE: &str = "remark.unresolve";

pub fn run(
    repo: gix::Repository,
    notes_ref: String,
    base_ref: Option<String>,
    cli: LspCli,
) -> Result<()> {
    let repo_root = repo
        .workdir()
        .map(ToOwned::to_owned)
        .context("remark lsp requires a working tree")?;
    let include_resolved = cli.include_resolved;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;

    runtime.block_on(async move {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let (service, socket) = LspService::new(|client| {
            Backend::new(client, repo_root, notes_ref, base_ref, include_resolved)
        });
        Server::new(stdin, stdout, socket).serve(service).await;
    });

    Ok(())
}

struct Backend {
    client: Client,
    repo_root: PathBuf,
    notes_ref: String,
    base_ref: Option<String>,
    include_resolved: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct ResolveArgs {
    file: String,
    line: Option<u32>,
    side: Option<String>,
    #[serde(default)]
    file_comment: bool,
}

impl Backend {
    fn new(
        client: Client,
        repo_root: PathBuf,
        notes_ref: String,
        base_ref: Option<String>,
        include_resolved: bool,
    ) -> Self {
        Self {
            client,
            repo_root,
            notes_ref,
            base_ref,
            include_resolved,
        }
    }

    fn open_repo(&self) -> Result<gix::Repository> {
        gix::open(&self.repo_root).context("open repository")
    }

    fn to_repo_relative(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.repo_root).ok()?;
        Some(rel.to_string_lossy().to_string())
    }

    async fn publish_diagnostics(&self, uri: &Url) {
        let Ok(path) = uri.to_file_path() else {
            return;
        };
        let Some(rel_path) = self.to_repo_relative(&path) else {
            return;
        };

        let repo = match self.open_repo() {
            Ok(repo) => repo,
            Err(err) => {
                self.log_error(err).await;
                return;
            }
        };

        let review =
            match load_file_review(&repo, &self.notes_ref, &rel_path, self.base_ref.as_deref()) {
                Ok(review) => review,
                Err(err) => {
                    self.log_error(err).await;
                    return;
                }
            };

        let diagnostics = review
            .map(|mut review| {
                if !self.include_resolved {
                    prune_resolved(&mut review);
                }
                review_to_diagnostics(&review)
            })
            .unwrap_or_default();

        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
    }

    async fn log_error(&self, err: anyhow::Error) {
        self.client
            .log_message(MessageType::ERROR, format!("remark-lsp: {err:#}"))
            .await;
    }

    async fn hover_for(&self, uri: &Url, position: Position) -> Option<Hover> {
        let path = uri.to_file_path().ok()?;
        let rel_path = self.to_repo_relative(&path)?;
        let repo = self.open_repo().ok()?;
        let review = load_file_review(&repo, &self.notes_ref, &rel_path, self.base_ref.as_deref())
            .ok()??;

        let line = position.line.saturating_add(1);
        let mut snippets = Vec::new();

        for (key, comment) in &review.comments {
            if !self.include_resolved && comment.resolved {
                continue;
            }
            if key.side == LineSide::New && key.line == line {
                snippets.push(comment.body.clone());
            } else if key.side == LineSide::Old && key.line == line {
                snippets.push(format!("[old] {}", comment.body));
            }
        }

        if snippets.is_empty()
            && let Some(comment) = &review.file_comment
            && (self.include_resolved || !comment.resolved)
        {
            snippets.push(comment.body.clone());
        }

        if snippets.is_empty() {
            return None;
        }

        let content = snippets.join("\n\n---\n\n");
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: content,
            }),
            range: None,
        })
    }

    async fn inlay_hints_for(&self, uri: &Url, range: Range) -> Vec<InlayHint> {
        let Ok(path) = uri.to_file_path() else {
            return Vec::new();
        };
        let Some(rel_path) = self.to_repo_relative(&path) else {
            return Vec::new();
        };
        let repo = match self.open_repo() {
            Ok(repo) => repo,
            Err(err) => {
                self.log_error(err).await;
                return Vec::new();
            }
        };
        let review =
            match load_file_review(&repo, &self.notes_ref, &rel_path, self.base_ref.as_deref()) {
                Ok(review) => review,
                Err(err) => {
                    self.log_error(err).await;
                    return Vec::new();
                }
            };
        let Some(mut review) = review else {
            return Vec::new();
        };
        if !self.include_resolved {
            prune_resolved(&mut review);
        }

        let mut hints = Vec::new();
        let start = range.start.line;
        let end = range.end.line;

        if let Some(comment) = &review.file_comment
            && start == 0
        {
            let character = line_end_utf16(&path, 0).unwrap_or(0);
            hints.push(make_inlay_hint(0, character, comment, None));
        }

        for (key, comment) in &review.comments {
            let line_idx = key.line.saturating_sub(1);
            if line_idx < start || line_idx > end {
                continue;
            }
            let character = line_end_utf16(&path, line_idx).unwrap_or(0);
            hints.push(make_inlay_hint(line_idx, character, comment, Some(key.side)));
        }

        hints
    }

    async fn code_actions_for(&self, uri: &Url, line_idx: u32) -> Vec<CodeActionOrCommand> {
        let Ok(path) = uri.to_file_path() else {
            return Vec::new();
        };
        let Some(rel_path) = self.to_repo_relative(&path) else {
            return Vec::new();
        };
        let repo = match self.open_repo() {
            Ok(repo) => repo,
            Err(err) => {
                self.log_error(err).await;
                return Vec::new();
            }
        };
        let review =
            match load_file_review(&repo, &self.notes_ref, &rel_path, self.base_ref.as_deref()) {
                Ok(review) => review,
                Err(err) => {
                    self.log_error(err).await;
                    return Vec::new();
                }
            };
        let Some(mut review) = review else {
            return Vec::new();
        };
        if !self.include_resolved {
            prune_resolved(&mut review);
        }

        let line = line_idx.saturating_add(1);
        let mut actions = Vec::new();

        if let Some(comment) = review.comments.get(&LineKey {
            side: LineSide::New,
            line,
        }) {
            actions.push(make_resolve_action(
                &rel_path,
                line,
                LineSide::New,
                comment.resolved,
            ));
        }
        if let Some(comment) = review.comments.get(&LineKey {
            side: LineSide::Old,
            line,
        }) {
            actions.push(make_resolve_action(
                &rel_path,
                line,
                LineSide::Old,
                comment.resolved,
            ));
        }
        if line_idx == 0
            && let Some(comment) = review.file_comment.as_ref()
        {
            actions.push(make_file_comment_action(
                &rel_path,
                comment.resolved,
            ));
        }

        actions
    }

    async fn execute_remark_command(&self, command: &str, args: ResolveArgs) {
        let side = match args.side.as_deref() {
            Some("old") => Some(LineSide::Old),
            Some("new") => Some(LineSide::New),
            Some(other) => {
                self.log_error(anyhow::anyhow!("invalid side '{other}'"))
                    .await;
                return;
            }
            None => None,
        };

        let repo = match self.open_repo() {
            Ok(repo) => repo,
            Err(err) => {
                self.log_error(err).await;
                return;
            }
        };
        let unresolve = matches!(command, COMMAND_UNRESOLVE);
        let cmd = crate::config::ResolveCli {
            file: Some(args.file.clone()),
            line: args.line,
            side,
            file_comment: args.file_comment,
            unresolve,
        };
        if let Err(err) = crate::resolve_cmd::run(&repo, &self.notes_ref, self.base_ref.clone(), cmd)
        {
            self.log_error(err).await;
            return;
        }

        let path = self.repo_root.join(&args.file);
        if let Ok(uri) = Url::from_file_path(path) {
            self.publish_diagnostics(&uri).await;
        }
    }
}

fn line_end_utf16(path: &Path, line_idx: u32) -> Option<u32> {
    let content = std::fs::read_to_string(path).ok()?;
    let line = content.lines().nth(line_idx as usize)?;
    Some(line.encode_utf16().count() as u32)
}

fn inlay_label(comment: &Comment, side: Option<LineSide>) -> String {
    let mut snippet = comment
        .body
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    const MAX_LEN: usize = 80;
    if snippet.len() > MAX_LEN {
        snippet.truncate(MAX_LEN);
        snippet.push('…');
    }
    let prefix = match side {
        Some(LineSide::Old) => "[old] ",
        Some(LineSide::New) => "",
        None => "",
    };
    format!("remark: {prefix}{snippet}")
}

fn make_inlay_hint(
    line_idx: u32,
    character: u32,
    comment: &Comment,
    side: Option<LineSide>,
) -> InlayHint {
    InlayHint {
        position: Position::new(line_idx, character),
        label: InlayHintLabel::String(inlay_label(comment, side)),
        kind: None,
        text_edits: None,
        tooltip: None,
        padding_left: Some(true),
        padding_right: Some(false),
        data: None,
    }
}

fn make_resolve_action(
    file: &str,
    line: u32,
    side: LineSide,
    resolved: bool,
) -> CodeActionOrCommand {
    let title = if resolved {
        format!("Unresolve remark comment{}", format_side_label(side))
    } else {
        format!("Resolve remark comment{}", format_side_label(side))
    };
    let command = if resolved {
        COMMAND_UNRESOLVE
    } else {
        COMMAND_RESOLVE
    };
    let args = ResolveArgs {
        file: file.to_string(),
        line: Some(line),
        side: Some(match side {
            LineSide::Old => "old".to_string(),
            LineSide::New => "new".to_string(),
        }),
        file_comment: false,
    };
    let cmd = Command {
        title: title.clone(),
        command: command.to_string(),
        arguments: Some(vec![serde_json::to_value(args).unwrap_or(Value::Null)]),
    };
    CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        command: Some(cmd),
        ..Default::default()
    })
}

fn make_file_comment_action(file: &str, resolved: bool) -> CodeActionOrCommand {
    let title = if resolved {
        "Unresolve remark file comment".to_string()
    } else {
        "Resolve remark file comment".to_string()
    };
    let command = if resolved {
        COMMAND_UNRESOLVE
    } else {
        COMMAND_RESOLVE
    };
    let args = ResolveArgs {
        file: file.to_string(),
        line: None,
        side: None,
        file_comment: true,
    };
    let cmd = Command {
        title: title.clone(),
        command: command.to_string(),
        arguments: Some(vec![serde_json::to_value(args).unwrap_or(Value::Null)]),
    };
    CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        command: Some(cmd),
        ..Default::default()
    })
}

fn format_side_label(side: LineSide) -> String {
    match side {
        LineSide::Old => " [old]".to_string(),
        LineSide::New => "".to_string(),
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> LspResult<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                hover_provider: Some(true.into()),
                inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
                    InlayHintOptions {
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
                    },
                ))),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                        work_done_progress_options: Default::default(),
                        resolve_provider: Some(false),
                    },
                )),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![COMMAND_RESOLVE.to_string(), COMMAND_UNRESOLVE.to_string()],
                    work_done_progress_options: Default::default(),
                }),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "remark-lsp initialized")
            .await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: tower_lsp::lsp_types::DidOpenTextDocumentParams) {
        self.publish_diagnostics(&params.text_document.uri).await;
    }

    async fn did_change(&self, params: tower_lsp::lsp_types::DidChangeTextDocumentParams) {
        self.publish_diagnostics(&params.text_document.uri).await;
    }

    async fn did_save(&self, params: tower_lsp::lsp_types::DidSaveTextDocumentParams) {
        self.publish_diagnostics(&params.text_document.uri).await;
    }

    async fn hover(&self, params: tower_lsp::lsp_types::HoverParams) -> LspResult<Option<Hover>> {
        Ok(self
            .hover_for(
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )
            .await)
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> LspResult<Option<Vec<InlayHint>>> {
        Ok(Some(
            self.inlay_hints_for(&params.text_document.uri, params.range)
                .await,
        ))
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> LspResult<Option<Vec<CodeActionOrCommand>>> {
        Ok(Some(
            self.code_actions_for(&params.text_document.uri, params.range.start.line)
                .await,
        ))
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> LspResult<Option<Value>> {
        let Some(args) = params
            .arguments
            .into_iter()
            .next()
            .and_then(|value| serde_json::from_value::<ResolveArgs>(value).ok())
        else {
            return Ok(None);
        };

        match params.command.as_str() {
            COMMAND_RESOLVE | COMMAND_UNRESOLVE => {
                self.execute_remark_command(&params.command, args).await;
            }
            _ => {}
        }

        Ok(None)
    }
}

fn load_file_review(
    repo: &gix::Repository,
    notes_ref: &str,
    path: &str,
    base_ref: Option<&str>,
) -> Result<Option<FileReview>> {
    let head = git::head_commit_oid(repo)?;
    let mut merged: Option<FileReview> = None;
    let mut views = vec![ViewKind::All, ViewKind::Staged, ViewKind::Unstaged];
    if base_ref.is_some() {
        views.push(ViewKind::Base);
    }

    for view in views {
        let base_for_key = match view {
            ViewKind::Base => base_ref,
            _ => None,
        };
        let oid = git::note_file_key_oid(repo, head, view, base_for_key, path)
            .with_context(|| format!("compute note key for '{path}'"))?;
        let note = crate::notes::read(repo, notes_ref, &oid)
            .with_context(|| format!("read note for '{path}'"))?;
        let Some(note) = note.as_deref() else {
            continue;
        };
        let Some(file) = crate::review::decode_file_note(note) else {
            continue;
        };
        merged = Some(match merged {
            None => file,
            Some(mut existing) => {
                merge_file_review(&mut existing, file);
                existing
            }
        });
    }

    Ok(merged)
}

fn merge_file_review(target: &mut FileReview, incoming: FileReview) {
    match (&target.file_comment, &incoming.file_comment) {
        (None, Some(_)) => target.file_comment = incoming.file_comment,
        (Some(t), Some(i)) if t.resolved && !i.resolved => {
            target.file_comment = incoming.file_comment
        }
        _ => {}
    }

    for (k, v) in incoming.comments {
        match target.comments.get(&k) {
            None => {
                target.comments.insert(k, v);
            }
            Some(existing) if existing.resolved && !v.resolved => {
                target.comments.insert(k, v);
            }
            _ => {}
        }
    }
}

fn prune_resolved(review: &mut FileReview) {
    if matches!(review.file_comment, Some(Comment { resolved: true, .. })) {
        review.file_comment = None;
    }
    review.comments.retain(|_, comment| !comment.resolved);
}

fn review_to_diagnostics(review: &FileReview) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    if let Some(comment) = &review.file_comment {
        diags.push(build_diag(1, None, comment, None));
    }
    for (key, comment) in &review.comments {
        diags.push(build_diag(key.line, Some(key.side), comment, Some(*key)));
    }
    diags
}

fn build_diag(
    line: u32,
    side: Option<LineSide>,
    comment: &Comment,
    key: Option<LineKey>,
) -> Diagnostic {
    let line_idx = line.saturating_sub(1);
    let range = Range::new(Position::new(line_idx, 0), Position::new(line_idx, 0));
    let mut message = comment.body.trim().to_string();
    if let Some(side) = side
        && side == LineSide::Old
    {
        message = format!("[old] {message}");
    }
    if let Some(key) = key {
        message = format!("{message} (line {})", key.line);
    }
    let severity = if comment.resolved {
        DiagnosticSeverity::HINT
    } else {
        DiagnosticSeverity::WARNING
    };
    Diagnostic {
        range,
        severity: Some(severity),
        source: Some("remark".to_string()),
        message,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn comment(body: &str, resolved: bool) -> Comment {
        Comment {
            body: body.to_string(),
            resolved,
        }
    }

    #[test]
    fn merge_file_review_prefers_unresolved_file_comment() {
        let mut target = FileReview {
            file_comment: Some(comment("old", true)),
            ..Default::default()
        };
        let incoming = FileReview {
            file_comment: Some(comment("new", false)),
            ..Default::default()
        };

        merge_file_review(&mut target, incoming);

        let file_comment = target.file_comment.expect("file comment");
        assert_eq!(file_comment.body, "new");
        assert!(!file_comment.resolved);
    }

    #[test]
    fn merge_file_review_keeps_unresolved_file_comment() {
        let mut target = FileReview {
            file_comment: Some(comment("keep", false)),
            ..Default::default()
        };
        let incoming = FileReview {
            file_comment: Some(comment("resolved", true)),
            ..Default::default()
        };

        merge_file_review(&mut target, incoming);

        let file_comment = target.file_comment.expect("file comment");
        assert_eq!(file_comment.body, "keep");
        assert!(!file_comment.resolved);
    }

    #[test]
    fn merge_file_review_prefers_unresolved_line_comment() {
        let key = LineKey {
            side: LineSide::New,
            line: 10,
        };
        let mut target = FileReview {
            comments: BTreeMap::from([(key, comment("old", true))]),
            ..Default::default()
        };
        let incoming = FileReview {
            comments: BTreeMap::from([(key, comment("new", false))]),
            ..Default::default()
        };

        merge_file_review(&mut target, incoming);

        let comment = target.comments.get(&key).expect("line comment");
        assert_eq!(comment.body, "new");
        assert!(!comment.resolved);
    }

    #[test]
    fn prune_resolved_drops_only_resolved_entries() {
        let mut review = FileReview {
            file_comment: Some(comment("file", true)),
            comments: BTreeMap::from([
                (
                    LineKey {
                        side: LineSide::New,
                        line: 1,
                    },
                    comment("keep", false),
                ),
                (
                    LineKey {
                        side: LineSide::Old,
                        line: 2,
                    },
                    comment("drop", true),
                ),
            ]),
            ..Default::default()
        };

        prune_resolved(&mut review);

        assert!(review.file_comment.is_none());
        assert_eq!(review.comments.len(), 1);
        let remaining = review.comments.values().next().expect("remaining comment");
        assert_eq!(remaining.body, "keep");
    }

    #[test]
    fn review_to_diagnostics_formats_messages() {
        let review = FileReview {
            file_comment: Some(comment("file note", false)),
            comments: BTreeMap::from([
                (
                    LineKey {
                        side: LineSide::Old,
                        line: 5,
                    },
                    comment("old note", false),
                ),
                (
                    LineKey {
                        side: LineSide::New,
                        line: 3,
                    },
                    comment("new note", false),
                ),
            ]),
            ..Default::default()
        };

        let mut messages: Vec<String> = review_to_diagnostics(&review)
            .into_iter()
            .map(|diag| diag.message)
            .collect();
        messages.sort();

        let mut expected = vec![
            "file note".to_string(),
            "new note (line 3)".to_string(),
            "[old] old note (line 5)".to_string(),
        ];
        expected.sort();
        assert_eq!(messages, expected);
    }

    #[test]
    fn build_diag_sets_range_and_source() {
        let diag = build_diag(7, None, &comment("note", false), None);

        assert_eq!(diag.range.start.line, 6);
        assert_eq!(diag.range.start.character, 0);
        assert_eq!(diag.range.end.line, 6);
        assert_eq!(diag.range.end.character, 0);
        assert_eq!(diag.source.as_deref(), Some("remark"));
        assert_eq!(diag.severity, Some(DiagnosticSeverity::WARNING));
    }

    fn init_repo_with_commit(path: &str, contents: &str) -> (tempfile::TempDir, gix::Repository) {
        use gix::bstr::ByteSlice;
        use gix::object::tree::EntryKind;
        use gix_ref::transaction::PreviousValue;

        let td = tempfile::tempdir().expect("tempdir");
        let repo = gix::init(td.path()).expect("init repo");

        let workdir = repo.workdir().expect("workdir");
        let full_path = workdir.join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parent");
        }
        std::fs::write(&full_path, contents).expect("write file");

        let blob_id = repo
            .write_blob(contents.as_bytes())
            .expect("write blob")
            .detach();
        let mut editor = gix::object::tree::Editor::new(&repo.empty_tree()).expect("tree editor");
        editor
            .upsert(path.as_bytes().as_bstr(), EntryKind::Blob, blob_id)
            .expect("upsert tree entry");
        let tree_id = editor.write().expect("write tree").detach();

        let sig = gix_actor::Signature {
            name: "remark-test".into(),
            email: "remark-test@localhost".into(),
            time: gix_date::Time {
                seconds: 0,
                offset: 0,
            },
        };
        let commit = gix::objs::Commit {
            tree: tree_id,
            parents: Default::default(),
            author: sig.clone(),
            committer: sig,
            encoding: None,
            message: "test commit\n".into(),
            extra_headers: Default::default(),
        };
        let commit_id = repo
            .write_object(commit)
            .expect("write commit")
            .detach();

        repo.reference("HEAD", commit_id, PreviousValue::Any, "test commit")
            .expect("update HEAD");

        (td, repo)
    }

    fn write_file_note(
        repo: &gix::Repository,
        notes_ref: &str,
        path: &str,
        view: ViewKind,
        file: &FileReview,
        base_ref: Option<&str>,
    ) {
        let head = crate::git::head_commit_oid(repo).expect("head commit");
        let key = crate::git::note_file_key_oid(repo, head, view, base_ref, path)
            .expect("note key");
        let note = crate::review::encode_file_note(file);
        crate::notes::write(repo, notes_ref, &key, Some(&note)).expect("write note");
    }

    #[test]
    fn load_file_review_reads_notes_from_repo() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let file = FileReview {
            file_comment: Some(comment("file note", false)),
            comments: BTreeMap::from([(
                LineKey {
                    side: LineSide::New,
                    line: 1,
                },
                comment("line note", false),
            )]),
            ..Default::default()
        };

        write_file_note(
            &repo,
            crate::git::DEFAULT_NOTES_REF,
            "src/lib.rs",
            ViewKind::All,
            &file,
            None,
        );

        let loaded = load_file_review(
            &repo,
            crate::git::DEFAULT_NOTES_REF,
            "src/lib.rs",
            None,
        )
        .expect("load review")
        .expect("review present");

        let file_comment = loaded.file_comment.expect("file comment");
        assert_eq!(file_comment.body, "file note");
        let line_comment = loaded
            .comments
            .get(&LineKey {
                side: LineSide::New,
                line: 1,
            })
            .expect("line comment");
        assert_eq!(line_comment.body, "line note");
    }

    #[test]
    fn load_file_review_prefers_unresolved_across_views() {
        let (_td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let key = LineKey {
            side: LineSide::New,
            line: 2,
        };

        let resolved = FileReview {
            comments: BTreeMap::from([(key, comment("resolved", true))]),
            ..Default::default()
        };
        let unresolved = FileReview {
            comments: BTreeMap::from([(key, comment("unresolved", false))]),
            ..Default::default()
        };

        write_file_note(
            &repo,
            crate::git::DEFAULT_NOTES_REF,
            "src/lib.rs",
            ViewKind::Staged,
            &resolved,
            None,
        );
        write_file_note(
            &repo,
            crate::git::DEFAULT_NOTES_REF,
            "src/lib.rs",
            ViewKind::Unstaged,
            &unresolved,
            None,
        );

        let loaded = load_file_review(
            &repo,
            crate::git::DEFAULT_NOTES_REF,
            "src/lib.rs",
            None,
        )
        .expect("load review")
        .expect("review present");
        let comment = loaded.comments.get(&key).expect("line comment");
        assert_eq!(comment.body, "unresolved");
        assert!(!comment.resolved);
    }

    async fn write_lsp_message<W: tokio::io::AsyncWrite + Unpin>(
        write: &mut W,
        value: serde_json::Value,
    ) {
        use tokio::io::AsyncWriteExt;
        let body = serde_json::to_string(&value).expect("serialize");
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        write.write_all(header.as_bytes()).await.expect("write header");
        write.write_all(body.as_bytes()).await.expect("write body");
        write.flush().await.expect("flush");
    }

    async fn read_lsp_message<R: tokio::io::AsyncRead + Unpin>(
        read: &mut R,
    ) -> serde_json::Value {
        use tokio::io::AsyncReadExt;
        let mut header = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            read.read_exact(&mut byte).await.expect("read header byte");
            header.push(byte[0]);
            if header.ends_with(b"\r\n\r\n") {
                break;
            }
        }

        let header_str = String::from_utf8(header).expect("header utf8");
        let mut content_length = None;
        for line in header_str.lines() {
            if let Some(value) = line.strip_prefix("Content-Length:") {
                content_length = Some(value.trim().parse::<usize>().expect("length"));
            }
        }
        let len = content_length.expect("content-length");
        let mut body = vec![0u8; len];
        read.read_exact(&mut body).await.expect("read body");
        serde_json::from_slice(&body).expect("json")
    }

    async fn wait_for_message<F, R>(read: &mut R, mut f: F) -> serde_json::Value
    where
        F: FnMut(&serde_json::Value) -> bool,
        R: tokio::io::AsyncRead + Unpin,
    {
        let fut = async {
            loop {
                let msg = read_lsp_message(read).await;
                if f(&msg) {
                    return msg;
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), fut)
            .await
            .expect("timeout")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lsp_end_to_end_diagnostics_and_hover() {
        let (td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let file_review = FileReview {
            file_comment: Some(comment("file note", false)),
            comments: BTreeMap::from([(
                LineKey {
                    side: LineSide::New,
                    line: 1,
                },
                comment("line note", false),
            )]),
            ..Default::default()
        };
        write_file_note(
            &repo,
            crate::git::DEFAULT_NOTES_REF,
            "src/lib.rs",
            ViewKind::All,
            &file_review,
            None,
        );

        let repo_root = td.path().to_path_buf();
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let (service, socket) = LspService::new(|client| {
            Backend::new(
                client,
                repo_root.clone(),
                crate::git::DEFAULT_NOTES_REF.to_string(),
                None,
                false,
            )
        });
        let (server_read, server_write) = tokio::io::split(server_stream);
        let server_task =
            tokio::spawn(async move { Server::new(server_read, server_write, socket).serve(service).await });

        let (mut client_read, mut client_write) = tokio::io::split(client_stream);
        let root_uri = Url::from_directory_path(&repo_root).expect("root uri");
        write_lsp_message(
            &mut client_write,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "processId": null,
                    "rootUri": root_uri,
                    "capabilities": {}
                }
            }),
        )
        .await;
        let _init = wait_for_message(&mut client_read, |msg| msg.get("id") == Some(&1.into()))
            .await;

        write_lsp_message(
            &mut client_write,
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "params": {}
            }),
        )
        .await;

        let file_path = repo_root.join("src/lib.rs");
        let file_uri = Url::from_file_path(&file_path).expect("file uri");
        write_lsp_message(
            &mut client_write,
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": file_uri,
                        "languageId": "rust",
                        "version": 1,
                        "text": "fn main() {}\n"
                    }
                }
            }),
        )
        .await;

        let diagnostics_msg = wait_for_message(&mut client_read, |msg| {
            msg.get("method")
                .and_then(|m| m.as_str())
                == Some("textDocument/publishDiagnostics")
        })
        .await;
        let diagnostics = diagnostics_msg["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics array");
        let mut messages: Vec<String> = diagnostics
            .iter()
            .filter_map(|diag| diag.get("message").and_then(|m| m.as_str()).map(|s| s.to_string()))
            .collect();
        messages.sort();
        assert!(messages.contains(&"file note".to_string()));
        assert!(messages.contains(&"line note (line 1)".to_string()));

        write_lsp_message(
            &mut client_write,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "textDocument/inlayHint",
                "params": {
                    "textDocument": { "uri": file_uri },
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 0 }
                    }
                }
            }),
        )
        .await;
        let inlay_msg = wait_for_message(&mut client_read, |msg| msg.get("id") == Some(&2.into()))
            .await;
        let inlays = inlay_msg["result"].as_array().expect("inlays array");
        assert!(inlays.iter().any(|hint| {
            hint.get("label")
                .and_then(|label| label.as_str())
                .is_some_and(|label| label.contains("line note"))
        }));

        write_lsp_message(
            &mut client_write,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "textDocument/hover",
                "params": {
                    "textDocument": { "uri": file_uri },
                    "position": { "line": 0, "character": 0 }
                }
            }),
        )
        .await;
        let hover_msg = wait_for_message(&mut client_read, |msg| msg.get("id") == Some(&3.into()))
            .await;
        let hover_contents = hover_msg["result"]["contents"]["value"]
            .as_str()
            .expect("hover contents");
        assert_eq!(hover_contents, "line note");

        write_lsp_message(
            &mut client_write,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "textDocument/codeAction",
                "params": {
                    "textDocument": { "uri": file_uri },
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 0 }
                    },
                    "context": { "diagnostics": [] }
                }
            }),
        )
        .await;
        let actions_msg = wait_for_message(&mut client_read, |msg| msg.get("id") == Some(&4.into()))
            .await;
        let actions = actions_msg["result"]
            .as_array()
            .expect("actions array");
        let resolve_action = actions
            .iter()
            .find(|action| {
                action
                    .get("title")
                    .and_then(|title| title.as_str())
                    .is_some_and(|title| title.starts_with("Resolve remark comment"))
            })
            .and_then(|action| action.get("command"))
            .expect("resolve command");

        write_lsp_message(
            &mut client_write,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "workspace/executeCommand",
                "params": resolve_action
            }),
        )
        .await;
        let _exec = wait_for_message(&mut client_read, |msg| msg.get("id") == Some(&5.into()))
            .await;

        let diagnostics_msg = wait_for_message(&mut client_read, |msg| {
            msg.get("method")
                .and_then(|m| m.as_str())
                == Some("textDocument/publishDiagnostics")
        })
        .await;
        let diagnostics = diagnostics_msg["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics array");
        let messages: Vec<String> = diagnostics
            .iter()
            .filter_map(|diag| diag.get("message").and_then(|m| m.as_str()).map(|s| s.to_string()))
            .collect();
        assert_eq!(messages, vec!["file note".to_string()]);

        server_task.abort();
    }
}
