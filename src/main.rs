mod app;
mod clipboard;
mod config;
mod diff;
mod file_tree;
mod git;
mod highlight;
mod lsp;
mod new_cmd;
mod notes;
mod prompt_cmd;
mod prompt_code;
mod purge_cmd;
mod resolve_cmd;
mod review;
mod ui;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::Context;
use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cli = config::Cli::parse();
    config::validate_ui_args(&cli)?;
    let config::Cli {
        global,
        command,
        ui,
    } = cli;

    let repo = gix::discover(std::env::current_dir().context("get current directory")?)
        .context("discover git repository")?;

    match command {
        Some(config::Command::New(_)) => new_cmd::run(&repo, global.notes_ref.clone()),
        Some(config::Command::Purge(cmd)) => purge_cmd::run(&repo, cmd.yes),
        Some(config::Command::Prompt(cmd)) => {
            let cfg = config::load_config(&global, &ui)?;
            let notes_ref = config::resolve_notes_ref(&repo, &cfg, global.notes_ref.clone());
            let base_ref = config::resolve_base_ref_optional(&cfg, global.base_ref.clone());
            let fetch_notes = config::resolve_fetch_notes(&cfg, global.fetch_notes);
            maybe_fetch_notes(&repo, &notes_ref, fetch_notes);
            prompt_cmd::run(&repo, &notes_ref, cmd.filter, base_ref)
        }
        Some(config::Command::Resolve(cmd)) => {
            let cfg = config::load_config(&global, &ui)?;
            let notes_ref = config::resolve_notes_ref(&repo, &cfg, global.notes_ref.clone());
            let base_ref = config::resolve_base_ref_optional(&cfg, global.base_ref.clone());
            let fetch_notes = config::resolve_fetch_notes(&cfg, global.fetch_notes);
            maybe_fetch_notes(&repo, &notes_ref, fetch_notes);
            resolve_cmd::run(&repo, &notes_ref, base_ref, cmd)
        }
        Some(config::Command::Lsp(cmd)) => {
            let cfg = config::load_config(&global, &ui)?;
            let notes_ref = config::resolve_notes_ref(&repo, &cfg, global.notes_ref.clone());
            let base_ref = config::resolve_base_ref_optional(&cfg, global.base_ref.clone());
            let fetch_notes = config::resolve_fetch_notes(&cfg, global.fetch_notes);
            maybe_fetch_notes(&repo, &notes_ref, fetch_notes);
            lsp::run(repo, notes_ref, base_ref, cmd)
        }
        None => {
            let cfg = config::load_config(&global, &ui)?;
            let notes_ref = config::resolve_notes_ref(&repo, &cfg, global.notes_ref.clone());
            let base_ref = config::resolve_base_ref_for_ui(&repo, &cfg, global.base_ref.clone());
            let show_ignored = config::resolve_show_ignored(&cfg, ui.show_ignored);
            let fetch_notes = config::resolve_fetch_notes(&cfg, global.fetch_notes);
            let view = ui.view.unwrap_or(git::ViewKind::All);
            let jump_target = app::build_jump_target(&repo, ui.file.clone(), ui.line, ui.side)?;
            maybe_fetch_notes(&repo, &notes_ref, fetch_notes);
            let options = app::UiOptions {
                notes_ref,
                base_ref,
                show_ignored,
                view,
                jump_target,
            };
            app::run(repo, options)
        }
    }
}

fn maybe_fetch_notes(repo: &gix::Repository, notes_ref: &str, fetch_notes: bool) {
    if !fetch_notes {
        return;
    }
    if let Err(err) = git::ensure_notes_ref(repo, notes_ref) {
        eprintln!("remark: {err}");
    }
}
