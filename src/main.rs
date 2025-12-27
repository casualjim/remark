mod app;
mod git;
mod highlight;
mod ui;

fn main() -> anyhow::Result<()> {
    app::run()
}
