mod app;
mod clipboard;
mod diff;
mod file_tree;
mod git;
mod highlight;
mod new_cmd;
mod notes;
mod prompt_cmd;
mod purge_cmd;
mod resolve_cmd;
mod review;
mod ui;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("prompt") => prompt_cmd::run(),
        Some("resolve") => resolve_cmd::run(),
        Some("new") => new_cmd::run(),
        Some("purge") => purge_cmd::run(),
        _ => app::run(),
    }
}
