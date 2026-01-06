use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use confique::Config as _;
use confique::Layer as _;

use crate::git::ViewKind;
use crate::review::LineSide;

#[derive(confique::Config, Debug, Clone)]
pub struct AppConfig {
    #[config(env = "REMARK_NOTES_REF")]
    pub notes_ref: Option<String>,
    #[config(env = "REMARK_BASE_REF")]
    pub base_ref: Option<String>,
    #[config(default = false, env = "REMARK_SHOW_IGNORED")]
    pub show_ignored: bool,
    #[config(default = true, env = "REMARK_FETCH_NOTES")]
    pub fetch_notes: bool,
}

#[derive(Parser)]
#[command(
    name = "remark",
    version,
    about = "Terminal-first code review notes for Git repos."
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub ui: UiArgs,
}

#[derive(Args, Debug, Clone, Default)]
pub struct GlobalArgs {
    /// Optional path to a config file to load in addition to the standard locations.
    #[arg(long = "config-file", global = true)]
    pub config_file: Option<PathBuf>,

    /// Notes ref to read/write.
    #[arg(long = "ref", global = true)]
    pub notes_ref: Option<String>,

    /// Base ref for base view (prompt/resolve UI base context).
    #[arg(long = "base", global = true)]
    pub base_ref: Option<String>,

    /// Fetch notes ref when missing (true/false).
    #[arg(
        long = "fetch-notes",
        global = true,
        value_parser = clap::builder::BoolishValueParser::new(),
        default_missing_value = "true",
        num_args = 0..=1
    )]
    pub fetch_notes: Option<bool>,
}

#[derive(Subcommand)]
pub enum Command {
    Prompt(PromptCli),
    Resolve(ResolveCli),
    Add(AddCli),
    New(NewCli),
    Purge(PurgeCli),
    Lsp(LspCli),
}

#[derive(Args, Debug, Clone, Default)]
pub struct UiArgs {
    /// Include gitignored files in the file list.
    #[arg(
        long = "ignored",
        value_parser = clap::builder::BoolishValueParser::new(),
        default_missing_value = "true",
        num_args = 0..=1
    )]
    pub show_ignored: Option<bool>,

    /// Start in view (all/unstaged/staged/base).
    #[arg(long = "view", value_enum)]
    pub view: Option<ViewKind>,

    /// Preselect a file when launching the UI.
    #[arg(long = "file")]
    pub file: Option<String>,

    /// Preselect a 1-based line (requires --file).
    #[arg(long = "line", value_parser = clap::value_parser!(u32).range(1..))]
    pub line: Option<u32>,

    /// Which side for line (default: new).
    #[arg(long = "side", value_enum)]
    pub side: Option<LineSide>,
}

#[derive(Args, Debug, Clone)]
pub struct PromptCli {
    /// Filter files (default: all).
    #[arg(long = "filter", value_enum, default_value_t = PromptFilter::All)]
    pub filter: PromptFilter,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptFilter {
    All,
    Unstaged,
    Staged,
    Base,
}

#[derive(Args, Debug, Clone)]
pub struct ResolveCli {
    /// File to resolve a comment on.
    #[arg(long = "file")]
    pub file: Option<String>,

    /// Line number (1-based).
    #[arg(long = "line", value_parser = clap::value_parser!(u32).range(1..))]
    pub line: Option<u32>,

    /// Which side for line comments (default: new).
    #[arg(long = "side", value_enum)]
    pub side: Option<LineSide>,

    /// Resolve the file-level comment.
    #[arg(long = "file-comment", action = ArgAction::SetTrue)]
    pub file_comment: bool,

    /// Mark comment as unresolved.
    #[arg(long = "unresolve", action = ArgAction::SetTrue)]
    pub unresolve: bool,
}

#[derive(Args, Debug, Clone)]
pub struct AddCli {
    /// File to add a comment on.
    #[arg(long = "file")]
    pub file: Option<String>,

    /// Line number (1-based).
    #[arg(long = "line", value_parser = clap::value_parser!(u32).range(1..))]
    pub line: Option<u32>,

    /// Which side for line comments (default: new).
    #[arg(long = "side", value_enum)]
    pub side: Option<LineSide>,

    /// Add/update the file-level comment.
    #[arg(long = "file-comment", action = ArgAction::SetTrue)]
    pub file_comment: bool,

    /// Comment body (ignored when using --edit).
    #[arg(long = "message", short = 'm')]
    pub message: Option<String>,

    /// Edit the comment body in $VISUAL/$EDITOR.
    #[arg(long = "edit", action = ArgAction::SetTrue)]
    pub edit: bool,

    /// Editor command to use with --edit (overrides $VISUAL/$EDITOR).
    #[arg(long = "editor")]
    pub editor: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct NewCli {}

#[derive(Args, Debug, Clone)]
pub struct PurgeCli {
    /// Delete all remark notes refs (refs/notes/remark*).
    #[arg(long = "yes", short = 'y', action = ArgAction::SetTrue, required = true)]
    pub yes: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct LspCli {
    /// Include resolved comments in diagnostics/hover output.
    #[arg(long = "include-resolved", action = ArgAction::SetTrue)]
    pub include_resolved: bool,

    /// Disable inlay hints output.
    #[arg(long = "no-inlay-hints", action = ArgAction::SetTrue)]
    pub no_inlay_hints: bool,

    /// Disable diagnostics output.
    #[arg(long = "no-diagnostics", action = ArgAction::SetTrue)]
    pub no_diagnostics: bool,
}

pub fn load_config(global: &GlobalArgs, ui: &UiArgs) -> Result<AppConfig> {
    let mut cli_layer = <AppConfig as confique::Config>::Layer::empty();
    cli_layer.notes_ref = global.notes_ref.clone();
    cli_layer.base_ref = global.base_ref.clone();
    cli_layer.fetch_notes = global.fetch_notes;
    cli_layer.show_ignored = ui.show_ignored;

    let mut builder = AppConfig::builder().preloaded(cli_layer).env();
    if let Some(path) = &global.config_file {
        builder = builder.file(path);
    }

    if let Ok(cwd) = std::env::current_dir() {
        let local_root = cwd.join(".config");
        builder = add_if_exists(builder, local_root.join("remark.toml"));
        builder = add_if_exists(builder, local_root.join("remark").join("config.toml"));
    }

    if let Some(dir) = dirs::config_dir() {
        builder = add_if_exists(builder, dir.join("remark").join("config.toml"));
    }

    builder.load().context("load remark config")
}

fn add_if_exists(
    mut builder: confique::Builder<AppConfig>,
    path: impl AsRef<Path>,
) -> confique::Builder<AppConfig> {
    let path = path.as_ref();
    if path.exists() {
        builder = builder.file(path);
    }
    builder
}

pub fn validate_ui_args(cli: &Cli) -> Result<()> {
    if cli.command.is_none() {
        return Ok(());
    }

    if cli.ui.has_any() {
        anyhow::bail!(
            "UI-only flags (--ignored/--view/--file/--line/--side) require no subcommand"
        );
    }

    Ok(())
}

pub fn resolve_notes_ref(
    repo: &gix::Repository,
    config: &AppConfig,
    cli_notes_ref: Option<String>,
) -> String {
    cli_notes_ref
        .or_else(|| config.notes_ref.clone())
        .unwrap_or_else(|| crate::git::read_notes_ref(repo))
}

pub fn resolve_base_ref_for_ui(
    repo: &gix::Repository,
    config: &AppConfig,
    cli_base_ref: Option<String>,
) -> Option<String> {
    cli_base_ref
        .or_else(|| config.base_ref.clone())
        .or_else(|| crate::git::default_base_ref(repo))
}

pub fn resolve_base_ref_optional(
    config: &AppConfig,
    cli_base_ref: Option<String>,
) -> Option<String> {
    cli_base_ref.or_else(|| config.base_ref.clone())
}

pub fn resolve_show_ignored(config: &AppConfig, cli_show_ignored: Option<bool>) -> bool {
    cli_show_ignored.unwrap_or(config.show_ignored)
}

pub fn resolve_fetch_notes(config: &AppConfig, cli_fetch_notes: Option<bool>) -> bool {
    cli_fetch_notes.unwrap_or(config.fetch_notes)
}

impl UiArgs {
    fn has_any(&self) -> bool {
        self.show_ignored.is_some()
            || self.view.is_some()
            || self.file.is_some()
            || self.line.is_some()
            || self.side.is_some()
    }
}
