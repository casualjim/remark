mod app;
mod clipboard;
mod diff;
mod git;
mod highlight;
mod notes;
mod prompt_cmd;
mod resolve_cmd;
mod review;
mod ui;

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("prompt") => prompt_cmd::run(),
        Some("resolve") => resolve_cmd::run(),
        _ => app::run(),
    }
}
