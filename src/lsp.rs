use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionOptions, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, Command, Diagnostic, DiagnosticSeverity, ExecuteCommandOptions,
    ExecuteCommandParams, Hover, HoverContents, InitializeParams, InitializeResult,
    InitializedParams, InlayHint, InlayHintLabel, InlayHintOptions, InlayHintParams,
    InlayHintServerCapabilities, MarkupContent, MarkupKind, MessageType, OneOf, Position, Range,
    ServerCapabilities, ShowDocumentParams, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower_lsp::{Client, LanguageServer, LspService, Server};

use crate::config::LspCli;
use crate::git::{self, ViewKind};
use crate::review::{Comment, FileReview, LineKey, LineSide};

const COMMAND_RESOLVE: &str = "remark.resolve";
const COMMAND_UNRESOLVE: &str = "remark.unresolve";
const COMMAND_ADD_DRAFT_COMMENT: &str = "remark.addDraftComment";
const COMMAND_OPEN_PROMPT: &str = "remark.openPrompt";
const DEFAULT_IGNORE_PATTERNS: &str = include_str!("../extra-ignores");

pub fn run(
    repo_root: Option<PathBuf>,
    notes_ref: String,
    base_ref: Option<String>,
    cli: LspCli,
) -> Result<()> {
    let include_resolved = cli.include_resolved;
    let enable_inlay_hints = !cli.no_inlay_hints;
    let enable_diagnostics = !cli.no_diagnostics;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;

    runtime.block_on(async move {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let (service, socket) = LspService::new(|client| {
            Backend::new(
                client,
                repo_root.clone(),
                notes_ref,
                base_ref,
                include_resolved,
                enable_inlay_hints,
                enable_diagnostics,
            )
        });
        Server::new(stdin, stdout, socket).serve(service).await;
    });

    Ok(())
}

struct Backend {
    client: Client,
    repo_root: std::sync::RwLock<Option<PathBuf>>,
    notes_ref: String,
    base_ref: Option<String>,
    include_resolved: bool,
    enable_inlay_hints: bool,
    enable_diagnostics: bool,
    show_document: std::sync::RwLock<Option<bool>>,
    open_documents: std::sync::RwLock<HashSet<Url>>,
    fs_watcher: std::sync::Mutex<Option<notify::RecommendedWatcher>>,
}

enum FsWatchEvent {
    Change,
    Error(String),
}

#[derive(Debug, Deserialize, Serialize)]
struct ResolveArgs {
    file: String,
    line: Option<u32>,
    side: Option<String>,
    #[serde(default)]
    file_comment: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct DraftArgs {
    file: String,
    line: Option<u32>,
    side: Option<String>,
    #[serde(default)]
    file_comment: bool,
}

impl Backend {
    fn new(
        client: Client,
        repo_root: Option<PathBuf>,
        notes_ref: String,
        base_ref: Option<String>,
        include_resolved: bool,
        enable_inlay_hints: bool,
        enable_diagnostics: bool,
    ) -> Self {
        Self {
            client,
            repo_root: std::sync::RwLock::new(repo_root),
            notes_ref,
            base_ref,
            include_resolved,
            enable_inlay_hints,
            enable_diagnostics,
            show_document: std::sync::RwLock::new(None),
            open_documents: std::sync::RwLock::new(HashSet::new()),
            fs_watcher: std::sync::Mutex::new(None),
        }
    }

    fn open_repo(&self) -> Result<gix::Repository> {
        let root = self
            .repo_root
            .read()
            .ok()
            .and_then(|root| root.clone())
            .context("remark lsp has no workspace root")?;
        gix::open(&root).context("open repository")
    }

    fn start_fs_watcher(&self) {
        let Some(root) = self.repo_root.read().ok().and_then(|root| root.clone()) else {
            return;
        };
        let notes_ref = self.notes_ref.clone();
        let base_ref = self.base_ref.clone();
        let client = self.client.clone();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<FsWatchEvent>();
        let watch_root = root.clone();
        let watch_root_cb = watch_root.clone();
        let gitdir = gix::open(&watch_root)
            .ok()
            .map(|repo| repo.path().to_path_buf());
        let gitdir_cb = gitdir.clone();
        let sender = tx.clone();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    if !should_sync_event(&event.kind) {
                        return;
                    }
                    if event
                        .paths
                        .iter()
                        .any(|path| should_sync_path(&watch_root_cb, gitdir_cb.as_deref(), path))
                    {
                        let _ = sender.send(FsWatchEvent::Change);
                    }
                }
                Err(err) => {
                    let _ = sender.send(FsWatchEvent::Error(format!(
                        "remark-lsp: file watch error: {err}"
                    )));
                }
            }) {
                Ok(watcher) => watcher,
                Err(err) => {
                    let client = self.client.clone();
                    tokio::spawn(async move {
                        client
                            .log_message(
                                MessageType::ERROR,
                                format!("remark-lsp: failed to start file watcher: {err}"),
                            )
                            .await;
                    });
                    return;
                }
            };

        if let Err(err) = watcher.watch(&watch_root, RecursiveMode::Recursive) {
            let client = self.client.clone();
            tokio::spawn(async move {
                client
                    .log_message(
                        MessageType::ERROR,
                        format!("remark-lsp: failed to watch workspace: {err}"),
                    )
                    .await;
            });
            return;
        }

        if let Ok(mut guard) = self.fs_watcher.lock() {
            *guard = Some(watcher);
        }

        tokio::spawn(async move {
            loop {
                let Some(event) = rx.recv().await else {
                    break;
                };
                match event {
                    FsWatchEvent::Change => {}
                    FsWatchEvent::Error(message) => {
                        client.log_message(MessageType::WARNING, message).await;
                        continue;
                    }
                }

                loop {
                    if let Err(err) = sync_once(&root, &notes_ref, base_ref.as_deref()) {
                        client
                            .log_message(
                                MessageType::ERROR,
                                format!("remark-lsp: watch sync failed: {err:#}"),
                            )
                            .await;
                    }

                    let mut pending = false;
                    loop {
                        match rx.try_recv() {
                            Ok(FsWatchEvent::Change) => {
                                pending = true;
                            }
                            Ok(FsWatchEvent::Error(message)) => {
                                client.log_message(MessageType::WARNING, message).await;
                            }
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
                        }
                    }

                    if !pending {
                        break;
                    }
                }
            }
        });
    }

    fn stop_fs_watcher(&self) {
        if let Ok(mut guard) = self.fs_watcher.lock() {
            let _ = guard.take();
        }
    }

    fn to_repo_relative(&self, path: &Path) -> Option<String> {
        let root = self.repo_root.read().ok().and_then(|root| root.clone())?;
        let rel = path.strip_prefix(&root).ok()?;
        Some(rel.to_string_lossy().to_string())
    }

    async fn publish_diagnostics(&self, uri: &Url) {
        if !self.enable_diagnostics {
            return;
        }
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

    async fn refresh_open_diagnostics(&self, exclude: Option<&Url>) {
        if !self.enable_diagnostics {
            return;
        }
        let uris = self
            .open_documents
            .read()
            .ok()
            .map(|set| set.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for uri in uris {
            if exclude.is_some_and(|excluded| excluded == &uri) {
                continue;
            }
            self.publish_diagnostics(&uri).await;
        }
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
            hints.push(make_inlay_hint(
                line_idx,
                character,
                comment,
                Some(key.side),
            ));
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
                Ok(review) => review.unwrap_or_default(),
                Err(err) => {
                    self.log_error(err).await;
                    return Vec::new();
                }
            };
        let mut review = review;
        if !self.include_resolved {
            prune_resolved(&mut review);
        }

        let line = line_idx.saturating_add(1);
        let mut actions = Vec::new();

        actions.push(make_add_line_comment_action(&rel_path, line, LineSide::New));
        actions.push(make_add_file_comment_action(&rel_path));
        actions.push(make_open_prompt_action());

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
        if let Some(comment) = review.file_comment.as_ref() {
            actions.push(make_file_comment_action(&rel_path, comment.resolved));
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
        if let Err(err) =
            crate::resolve_cmd::run(&repo, &self.notes_ref, self.base_ref.clone(), cmd)
        {
            self.log_error(err).await;
            return;
        }

        let root = self.repo_root.read().ok().and_then(|root| root.clone());
        if let Some(root) = root {
            let path = root.join(&args.file);
            if let Ok(uri) = Url::from_file_path(path) {
                self.publish_diagnostics(&uri).await;
            }
        }

        if matches!(command, COMMAND_RESOLVE)
            && let Err(err) = self
                .remove_from_draft(&args.file, args.line, side, args.file_comment)
                .await
        {
            self.log_error(err).await;
        }
    }

    async fn add_draft_comment(&self, args: DraftArgs) {
        let DraftArgs {
            file,
            line,
            side: side_raw,
            file_comment,
        } = args;

        let side = match side_raw.as_deref() {
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

        if let Err(err) = crate::add_cmd::write_draft_placeholder(
            &repo,
            &self.notes_ref,
            self.base_ref.as_deref(),
            &file,
            line,
            side,
            file_comment,
        ) {
            self.log_error(err).await;
        }
    }

    async fn open_prompt(&self) {
        let repo = match self.open_repo() {
            Ok(repo) => repo,
            Err(err) => {
                self.log_error(err).await;
                return;
            }
        };

        if let Err(err) =
            crate::add_cmd::ensure_draft_exists(&repo, &self.notes_ref, self.base_ref.as_deref())
        {
            self.log_error(err).await;
            return;
        }
        if let Err(err) =
            crate::add_cmd::sync_draft_notes(&repo, &self.notes_ref, self.base_ref.as_deref())
        {
            self.log_error(err).await;
        }

        let path = match crate::add_cmd::draft_path(&repo) {
            Ok(path) => path,
            Err(err) => {
                self.log_error(err).await;
                return;
            }
        };
        let Ok(uri) = Url::from_file_path(path) else {
            return;
        };
        self.show_document(uri, None).await;
    }

    async fn show_document(&self, uri: Url, selection: Option<Range>) {
        let params = ShowDocumentParams {
            uri,
            external: None,
            take_focus: Some(true),
            selection,
        };
        let allow_show = self.show_document.read().ok().and_then(|guard| *guard);
        if allow_show == Some(false) {
            self.client
                .log_message(
                    MessageType::INFO,
                    "remark-lsp: client does not support window/showDocument",
                )
                .await;
            return;
        }
        if let Err(err) = self.client.show_document(params).await {
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("remark-lsp: showDocument failed, disabling: {err}"),
                )
                .await;
            if let Ok(mut guard) = self.show_document.write() {
                *guard = Some(false);
            }
        }
    }

    async fn sync_draft_if_needed(&self, uri: &Url) -> bool {
        let Ok(path) = uri.to_file_path() else {
            return false;
        };
        let repo = match self.open_repo() {
            Ok(repo) => repo,
            Err(err) => {
                self.log_error(err).await;
                return false;
            }
        };
        let draft_path = match crate::add_cmd::draft_path(&repo) {
            Ok(path) => path,
            Err(err) => {
                self.log_error(err).await;
                return false;
            }
        };
        if path != draft_path {
            return false;
        }
        if let Err(err) = crate::add_cmd::sync_draft_notes_on_save(
            &repo,
            &self.notes_ref,
            self.base_ref.as_deref(),
        ) {
            self.log_error(err).await;
        }
        self.refresh_open_diagnostics(Some(uri)).await;
        true
    }

    async fn remove_from_draft(
        &self,
        file: &str,
        line: Option<u32>,
        side: Option<LineSide>,
        file_comment: bool,
    ) -> Result<()> {
        let repo = self.open_repo()?;
        crate::add_cmd::remove_from_draft(
            &repo,
            self.base_ref.as_deref(),
            file,
            line,
            side,
            file_comment,
        )
    }
}

fn line_end_utf16(path: &Path, line_idx: u32) -> Option<u32> {
    let content = std::fs::read_to_string(path).ok()?;
    let line = content.lines().nth(line_idx as usize)?;
    Some(line.encode_utf16().count() as u32)
}

fn sync_once(root: &Path, notes_ref: &str, base_ref: Option<&str>) -> Result<()> {
    let repo = gix::open(root).context("open repository")?;
    crate::add_cmd::sync_draft_notes(&repo, notes_ref, base_ref)?;
    Ok(())
}

fn should_sync_event(kind: &notify::EventKind) -> bool {
    use notify::event::EventKind;

    !matches!(kind, EventKind::Access(_) | EventKind::Other)
}

fn default_ignore_file() -> &'static PathBuf {
    static DEFAULT_IGNORE_FILE: OnceLock<PathBuf> = OnceLock::new();

    DEFAULT_IGNORE_FILE.get_or_init(|| {
        let ignore_path = std::env::temp_dir().join("remark-default-ignore");
        let _ = std::fs::write(&ignore_path, DEFAULT_IGNORE_PATTERNS);
        ignore_path
    })
}

fn walker_includes_path(project_root: &Path, path: &Path) -> bool {
    let mut ob = OverrideBuilder::new(project_root);
    let candidate = match path.strip_prefix(project_root) {
        Ok(rel) => rel,
        Err(_) => path,
    };
    if ob.add(&candidate.to_string_lossy()).is_err() {
        return false;
    }
    let overrides = match ob.build() {
        Ok(o) => o,
        Err(_) => return false,
    };

    let mut builder = WalkBuilder::new(project_root);
    let default_ignore = default_ignore_file();
    if default_ignore.exists() {
        builder.add_ignore(default_ignore);
    }
    builder.overrides(overrides);

    for entry in builder.build().flatten() {
        if entry.path() == path {
            return entry.file_type().is_some_and(|ft| ft.is_file());
        }
    }

    false
}

fn should_sync_path(root: &Path, gitdir: Option<&Path>, path: &Path) -> bool {
    // Check if path is within root
    let (abs, root_abs) = match (std::fs::canonicalize(path), std::fs::canonicalize(root)) {
        (Ok(abs), Ok(root_abs)) => (abs, root_abs),
        _ => return false,
    };
    if !abs.starts_with(&root_abs) {
        return false;
    }

    if let Some(gitdir) = gitdir
        && let Ok(git_abs) = std::fs::canonicalize(gitdir)
        && abs.starts_with(&git_abs)
    {
        return false;
    }

    // Check if it's a regular file (not a directory or symlink)
    // This is much more efficient than walking the entire tree
    let metadata = match std::fs::metadata(&abs) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };

    if !metadata.is_file() {
        return false;
    }

    walker_includes_path(&root_abs, &abs)
}

fn inlay_label(comment: &Comment, side: Option<LineSide>) -> String {
    let mut snippet = comment.body.lines().next().unwrap_or("").trim().to_string();
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
        title: format!("Remark: {}", title),
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

fn make_add_line_comment_action(file: &str, line: u32, side: LineSide) -> CodeActionOrCommand {
    let title = format!("Remark: Add line comment{}", format_side_label(side));
    let args = DraftArgs {
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
        command: COMMAND_ADD_DRAFT_COMMENT.to_string(),
        arguments: Some(vec![serde_json::to_value(args).unwrap_or(Value::Null)]),
    };
    CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        command: Some(cmd),
        ..Default::default()
    })
}

fn make_add_file_comment_action(file: &str) -> CodeActionOrCommand {
    let title = "Remark: Add file comment".to_string();
    let args = DraftArgs {
        file: file.to_string(),
        line: None,
        side: None,
        file_comment: true,
    };
    let cmd = Command {
        title: title.clone(),
        command: COMMAND_ADD_DRAFT_COMMENT.to_string(),
        arguments: Some(vec![serde_json::to_value(args).unwrap_or(Value::Null)]),
    };
    CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        command: Some(cmd),
        ..Default::default()
    })
}

fn make_open_prompt_action() -> CodeActionOrCommand {
    let title = "Remark: Open prompt".to_string();
    let cmd = Command {
        title: title.clone(),
        command: COMMAND_OPEN_PROMPT.to_string(),
        arguments: None,
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
    async fn initialize(&self, params: InitializeParams) -> LspResult<InitializeResult> {
        if let Some(root) = extract_root_path(&params)
            && let Ok(mut guard) = self.repo_root.write()
        {
            *guard = Some(root);
        }
        let show_document = params
            .capabilities
            .window
            .as_ref()
            .and_then(|window| window.show_document.as_ref())
            .map(|caps| caps.support);
        if let Ok(mut guard) = self.show_document.write() {
            *guard = show_document;
        }
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                hover_provider: Some(true.into()),
                inlay_hint_provider: self.enable_inlay_hints.then(|| {
                    OneOf::Right(InlayHintServerCapabilities::Options(InlayHintOptions {
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
                    }))
                }),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![
                            CodeActionKind::QUICKFIX,
                            CodeActionKind::REFACTOR,
                        ]),
                        work_done_progress_options: Default::default(),
                        resolve_provider: Some(false),
                    },
                )),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        COMMAND_RESOLVE.to_string(),
                        COMMAND_UNRESOLVE.to_string(),
                        COMMAND_ADD_DRAFT_COMMENT.to_string(),
                        COMMAND_OPEN_PROMPT.to_string(),
                    ],
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
        self.start_fs_watcher();
        let repo_root = self.repo_root.read().ok().and_then(|root| root.clone());
        let notes_ref = self.notes_ref.clone();
        let base_ref = self.base_ref.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            let result = (|| -> Result<()> {
                let Some(root) = repo_root else {
                    return Ok(());
                };
                let repo = gix::open(&root).context("open repository")?;
                crate::add_cmd::ensure_draft_exists(&repo, &notes_ref, base_ref.as_deref())?;
                crate::add_cmd::sync_draft_notes(&repo, &notes_ref, base_ref.as_deref())?;
                Ok(())
            })();

            if let Err(err) = result {
                client
                    .log_message(
                        MessageType::ERROR,
                        format!("remark-lsp: init sync failed: {err:#}"),
                    )
                    .await;
            }
        });
    }

    async fn shutdown(&self) -> LspResult<()> {
        self.stop_fs_watcher();
        Ok(())
    }

    async fn did_open(&self, params: tower_lsp::lsp_types::DidOpenTextDocumentParams) {
        if let Ok(mut docs) = self.open_documents.write() {
            docs.insert(params.text_document.uri.clone());
        }
        if self.sync_draft_if_needed(&params.text_document.uri).await {
            return;
        }
        self.publish_diagnostics(&params.text_document.uri).await;
    }

    async fn did_change(&self, params: tower_lsp::lsp_types::DidChangeTextDocumentParams) {
        self.publish_diagnostics(&params.text_document.uri).await;
    }

    async fn did_save(&self, params: tower_lsp::lsp_types::DidSaveTextDocumentParams) {
        if self.sync_draft_if_needed(&params.text_document.uri).await {
            return;
        }
        self.publish_diagnostics(&params.text_document.uri).await;
    }

    async fn did_close(&self, params: tower_lsp::lsp_types::DidCloseTextDocumentParams) {
        if let Ok(mut docs) = self.open_documents.write() {
            docs.remove(&params.text_document.uri);
        }
        let _ = self.sync_draft_if_needed(&params.text_document.uri).await;
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
        if !self.enable_inlay_hints {
            return Ok(None);
        }
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
        match params.command.as_str() {
            COMMAND_RESOLVE | COMMAND_UNRESOLVE => {
                let Some(args) = params
                    .arguments
                    .into_iter()
                    .next()
                    .and_then(|value| serde_json::from_value::<ResolveArgs>(value).ok())
                else {
                    return Ok(None);
                };
                self.execute_remark_command(&params.command, args).await;
            }
            COMMAND_ADD_DRAFT_COMMENT => {
                let Some(args) = params
                    .arguments
                    .into_iter()
                    .next()
                    .and_then(|value| serde_json::from_value::<DraftArgs>(value).ok())
                else {
                    return Ok(None);
                };
                self.add_draft_comment(args).await;
            }
            COMMAND_OPEN_PROMPT => {
                self.open_prompt().await;
            }
            _ => {}
        }

        Ok(None)
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.fs_watcher.lock() {
            let _ = guard.take();
        }
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

fn extract_root_path(params: &InitializeParams) -> Option<PathBuf> {
    params
        .root_uri
        .as_ref()
        .and_then(|uri| uri.to_file_path().ok())
        .or_else(|| {
            params
                .workspace_folders
                .as_ref()
                .and_then(|folders| folders.first())
                .and_then(|folder| folder.uri.to_file_path().ok())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Once;

    fn comment(body: &str, resolved: bool) -> Comment {
        Comment {
            body: body.to_string(),
            resolved,
            snippet_hash: None,
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
        ensure_git_identity();
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
        let commit_id = repo.write_object(commit).expect("write commit").detach();

        repo.reference("HEAD", commit_id, PreviousValue::Any, "test commit")
            .expect("update HEAD");

        (td, repo)
    }

    fn ensure_git_identity() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            // Safe in tests: single-threaded init, process-scoped env vars.
            unsafe {
                std::env::set_var("GIT_AUTHOR_NAME", "remark-test");
                std::env::set_var("GIT_AUTHOR_EMAIL", "remark-test@localhost");
                std::env::set_var("GIT_COMMITTER_NAME", "remark-test");
                std::env::set_var("GIT_COMMITTER_EMAIL", "remark-test@localhost");
            }
        });
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
        let key =
            crate::git::note_file_key_oid(repo, head, view, base_ref, path).expect("note key");
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

        let loaded = load_file_review(&repo, crate::git::DEFAULT_NOTES_REF, "src/lib.rs", None)
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

        let loaded = load_file_review(&repo, crate::git::DEFAULT_NOTES_REF, "src/lib.rs", None)
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
        write
            .write_all(header.as_bytes())
            .await
            .expect("write header");
        write.write_all(body.as_bytes()).await.expect("write body");
        write.flush().await.expect("flush");
    }

    async fn read_lsp_message<R: tokio::io::AsyncRead + Unpin>(read: &mut R) -> serde_json::Value {
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

    async fn wait_for_diagnostics<R: tokio::io::AsyncRead + Unpin>(
        read: &mut R,
        uri: &Url,
    ) -> serde_json::Value {
        let uri = uri.as_str().to_string();
        wait_for_message(read, |msg| {
            msg.get("method")
                .and_then(|m| m.as_str())
                .is_some_and(|m| m == "textDocument/publishDiagnostics")
                && msg
                    .get("params")
                    .and_then(|params| params.get("uri"))
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| value == uri)
        })
        .await
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
                Some(repo_root.clone()),
                crate::git::DEFAULT_NOTES_REF.to_string(),
                None,
                false,
                true,
                true,
            )
        });
        let (server_read, server_write) = tokio::io::split(server_stream);
        let server_task = tokio::spawn(async move {
            Server::new(server_read, server_write, socket)
                .serve(service)
                .await
        });

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
        let _init =
            wait_for_message(&mut client_read, |msg| msg.get("id") == Some(&1.into())).await;

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
            msg.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics")
        })
        .await;
        let diagnostics = diagnostics_msg["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics array");
        let mut messages: Vec<String> = diagnostics
            .iter()
            .filter_map(|diag| {
                diag.get("message")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string())
            })
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
        let inlay_msg =
            wait_for_message(&mut client_read, |msg| msg.get("id") == Some(&2.into())).await;
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
        let hover_msg =
            wait_for_message(&mut client_read, |msg| msg.get("id") == Some(&3.into())).await;
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
        let actions_msg =
            wait_for_message(&mut client_read, |msg| msg.get("id") == Some(&4.into())).await;
        let actions = actions_msg["result"].as_array().expect("actions array");
        let draft_line_action = actions
            .iter()
            .find(|action| {
                action
                    .get("title")
                    .and_then(|title| title.as_str())
                    .is_some_and(|title| title == "Remark: Add line comment")
            })
            .expect("draft line action");
        let draft_file_action = actions
            .iter()
            .find(|action| {
                action
                    .get("title")
                    .and_then(|title| title.as_str())
                    .is_some_and(|title| title == "Remark: Add file comment")
            })
            .expect("draft file action");
        let open_prompt_action = actions
            .iter()
            .find(|action| {
                action
                    .get("title")
                    .and_then(|title| title.as_str())
                    .is_some_and(|title| title == "Remark: Open prompt")
            })
            .expect("open prompt action");
        assert_eq!(
            draft_line_action.get("kind").and_then(|k| k.as_str()),
            Some("quickfix")
        );
        assert_eq!(
            draft_file_action.get("kind").and_then(|k| k.as_str()),
            Some("quickfix")
        );
        assert_eq!(
            open_prompt_action.get("kind").and_then(|k| k.as_str()),
            Some("quickfix")
        );
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
        let _exec =
            wait_for_message(&mut client_read, |msg| msg.get("id") == Some(&5.into())).await;

        let diagnostics_msg = wait_for_message(&mut client_read, |msg| {
            msg.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics")
        })
        .await;
        let diagnostics = diagnostics_msg["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics array");
        let messages: Vec<String> = diagnostics
            .iter()
            .filter_map(|diag| {
                diag.get("message")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string())
            })
            .collect();
        assert_eq!(messages, vec!["file note".to_string()]);

        server_task.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lsp_clears_diagnostics_after_draft_save() {
        let (td, repo) = init_repo_with_commit("src/lib.rs", "fn main() {}\n");
        let workdir = repo.workdir().expect("workdir");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("add")
            .arg("src/lib.rs")
            .status()
            .expect("git add");
        assert!(status.success(), "git add failed");
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
        crate::add_cmd::sync_draft_notes(&repo, crate::git::DEFAULT_NOTES_REF, None)
            .expect("sync draft");

        let repo_root = td.path().to_path_buf();
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let (service, socket) = LspService::new(|client| {
            Backend::new(
                client,
                Some(repo_root.clone()),
                crate::git::DEFAULT_NOTES_REF.to_string(),
                None,
                false,
                true,
                true,
            )
        });
        let (server_read, server_write) = tokio::io::split(server_stream);
        let server_task = tokio::spawn(async move {
            Server::new(server_read, server_write, socket)
                .serve(service)
                .await
        });

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
        let _init =
            wait_for_message(&mut client_read, |msg| msg.get("id") == Some(&1.into())).await;

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

        let diagnostics_msg = wait_for_diagnostics(&mut client_read, &file_uri).await;
        let diagnostics = diagnostics_msg["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics array");
        assert_eq!(diagnostics.len(), 2);

        let draft_path = crate::add_cmd::draft_path(&repo).expect("draft path");
        let empty = crate::review::render_prompt(&crate::review::Review::new(), |_, _| None);
        std::fs::write(&draft_path, empty).expect("write draft");
        let draft_uri = Url::from_file_path(&draft_path).expect("draft uri");
        write_lsp_message(
            &mut client_write,
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": draft_uri,
                        "languageId": "markdown",
                        "version": 1,
                        "text": "No comments.\n"
                    }
                }
            }),
        )
        .await;

        let diagnostics_msg = wait_for_diagnostics(&mut client_read, &file_uri).await;
        let diagnostics = diagnostics_msg["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics array");
        assert!(
            diagnostics.is_empty(),
            "expected no diagnostics after draft save, got: {diagnostics:?}"
        );

        server_task.abort();
    }
}
