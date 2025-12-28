use anyhow::{Context, Result};
use base64::Engine;
use copypasta::{ClipboardContext, ClipboardProvider};
use std::io::IsTerminal;

pub fn copy(text: &str) -> Result<String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && copy_wayland(text).is_ok() {
        return Ok("wayland".to_string());
        // Fall through to desktop clipboard in case Wayland copy fails.
    }

    if copy_desktop(text).is_ok() {
        return Ok("desktop".to_string());
    }

    if osc52_enabled() {
        copy_osc52(text)?;
        return Ok("osc52".to_string());
    }

    anyhow::bail!("clipboard copy failed (set REMARK_OSC52=1 to enable OSC52 fallback)")
}

fn copy_desktop(text: &str) -> Result<()> {
    let mut ctx =
        ClipboardContext::new().map_err(|e| anyhow::anyhow!("init clipboard context: {e}"))?;
    ctx.set_contents(text.to_string())
        .map_err(|e| anyhow::anyhow!("set clipboard contents: {e}"))?;
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn copy_wayland(text: &str) -> Result<()> {
    use wl_clipboard_rs::copy::{MimeType, Options, Source};
    Options::new()
        .copy(
            Source::Bytes(text.as_bytes().to_vec().into()),
            MimeType::Text,
        )
        .context("wayland clipboard copy")?;
    Ok(())
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn copy_wayland(_text: &str) -> Result<()> {
    anyhow::bail!("wayland clipboard not supported on this platform")
}

fn osc52_enabled() -> bool {
    std::env::var("REMARK_OSC52")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn copy_osc52(text: &str) -> Result<()> {
    // OSC 52 clipboard: ESC ] 52 ; c ; <base64> BEL
    // Works in many terminals (xterm, iTerm2, kitty, wezterm, tmux w/ passthrough, etc).
    //
    // We write to stderr when possible so `remark prompt --copy > file` doesn't pollute stdout.
    // (Both stdout/stderr are typically connected to the same terminal.)
    let mut err = std::io::stderr().lock();
    let mut out = std::io::stdout().lock();
    let w: &mut dyn std::io::Write = if std::io::stderr().is_terminal() {
        &mut err
    } else if std::io::stdout().is_terminal() {
        &mut out
    } else {
        anyhow::bail!("OSC52 requires a terminal on stdout/stderr");
    };

    let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{b64}\x07");

    // tmux passthrough requires doubling ESC.
    let seq = if std::env::var_os("TMUX").is_some() {
        let escaped = seq.replace('\x1b', "\x1b\x1b");
        format!("\x1bPtmux;{escaped}\x1b\\")
    } else if std::env::var("TERM")
        .ok()
        .is_some_and(|t| t.starts_with("screen"))
    {
        format!("\x1bP{seq}\x1b\\")
    } else {
        seq
    };

    w.write_all(seq.as_bytes())
        .context("write OSC52 sequence")?;
    w.flush().ok();
    Ok(())
}
