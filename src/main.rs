mod app;
mod git;
mod notes;
mod review;
mod ui;

fn main() -> anyhow::Result<()> {
    app::run()
}
