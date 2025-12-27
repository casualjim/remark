use anyhow::{Context, Result};
use base64::Engine;

pub fn copy(text: &str) -> Result<&'static str> {
    if let Ok(method) = copy_desktop(text) {
        return Ok(method);
    }

    copy_osc52(text)?;
    Ok("osc52")
}

fn copy_desktop(text: &str) -> Result<&'static str> {
    use copypasta::{ClipboardContext, ClipboardProvider};
    let mut ctx = ClipboardContext::new()
        .map_err(|e| anyhow::anyhow!("init clipboard context: {e}"))?;
    ctx.set_contents(text.to_string())
        .map_err(|e| anyhow::anyhow!("set clipboard contents: {e}"))?;
    Ok("desktop")
}

fn copy_osc52(text: &str) -> Result<()> {
    // OSC 52 clipboard: ESC ] 52 ; c ; <base64> BEL
    // Works in many terminals (xterm, iTerm2, kitty, wezterm, tmux w/ passthrough, etc).
    let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{b64}\x07");
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    out.write_all(seq.as_bytes())
        .context("write OSC52 sequence")?;
    out.flush().ok();
    Ok(())
}
