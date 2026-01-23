use anyhow::{Context, Result};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use syntastica::language_set::SupportedLanguage;
use syntastica::style::Style as SynStyle;
use syntastica::theme::ResolvedTheme;
use syntastica_parsers::{Lang, LanguageSetImpl};

pub struct Highlighter {
  language_set: LanguageSetImpl,
  theme: ResolvedTheme,
}

impl Highlighter {
  pub fn new() -> Result<Self> {
    Ok(Self {
      language_set: LanguageSetImpl::new(),
      theme: syntastica_themes::catppuccin::mocha(),
    })
  }

  pub fn highlight_lang(&self, lang: Lang, text: &str) -> Result<Vec<Vec<Span<'static>>>> {
    let highlights = syntastica::Processor::process_once(text, lang, &self.language_set)
      .context("syntastica process")?;
    let themed = syntastica::renderer::resolve_styles(&highlights, &self.theme);

    let mut lines = Vec::with_capacity(themed.len());
    for line in themed {
      let mut spans = Vec::with_capacity(line.len());
      for (chunk, style) in line {
        let span = match style {
          Some(s) => Span::styled(chunk.to_string(), syn_style_to_tui(&s)),
          None => Span::raw(chunk.to_string()),
        };
        spans.push(span);
      }
      lines.push(spans);
    }
    Ok(lines)
  }

  pub fn highlight_diff(&self, text: &str) -> Result<Vec<Vec<Span<'static>>>> {
    self.highlight_lang(Lang::Diff, text)
  }

  pub fn detect_file_lang(&self, repo: &gix::Repository, repo_relative_path: &str) -> Option<Lang> {
    if let Some(lang) = resolve_lang_token(repo_relative_path, &self.language_set) {
      return Some(lang);
    }

    if let Some(wt) = repo.workdir() {
      let abs = wt.join(repo_relative_path);
      if abs.is_file()
        && let Ok(Some(d)) = hyperpolyglot::detect(&abs)
        && let Some(lang) = resolve_lang_token(d.language(), &self.language_set)
      {
        return Some(lang);
      }
    }

    // Fallback: extension/name based detection (e.g. `Cargo.toml`, `.zshrc`, etc).
    Lang::for_injection(repo_relative_path, &self.language_set)
  }

  #[allow(dead_code)]
  pub fn highlight_diff_text(&self, text: &str) -> Result<Text<'static>> {
    let lines = self.highlight_diff(text)?;
    Ok(Text::from(
      lines.into_iter().map(Line::from).collect::<Vec<_>>(),
    ))
  }
}

fn syn_style_to_tui(style: &SynStyle) -> Style {
  let mut s = Style::default();
  let c = style.color();
  s = s.fg(Color::Rgb(c.red, c.green, c.blue));
  if let Some(bg) = style.bg() {
    s = s.bg(Color::Rgb(bg.red, bg.green, bg.blue));
  }

  let mut mods = Modifier::empty();
  if style.bold() {
    mods |= Modifier::BOLD;
  }
  if style.italic() {
    mods |= Modifier::ITALIC;
  }
  if style.underline() {
    mods |= Modifier::UNDERLINED;
  }
  if style.strikethrough() {
    mods |= Modifier::CROSSED_OUT;
  }
  s.add_modifier(mods)
}

fn resolve_lang_token(info: &str, language_set: &LanguageSetImpl) -> Option<Lang> {
  let raw = info.split_whitespace().next().unwrap_or("").trim();
  if raw.is_empty() {
    return None;
  }

  // Prefer hyperpolyglot detection without hardcoding alias tables.
  //
  // Note: `hyperpolyglot::detect()` will sometimes attempt to open the path if an extension is
  // ambiguous. To keep extension-based detection working even when the actual file isn't present
  // (e.g. deleted files in a staged diff), fall back to probing a tiny temp file by extension.
  let mut candidates: Vec<String> = Vec::new();
  if let Some(name) = hyperpolyglot_name_for_token(raw) {
    candidates.push(name);
  }
  candidates.push(raw.to_string());

  candidates.retain(|s| !s.trim().is_empty());
  candidates.dedup();

  for candidate in candidates {
    let candidate = candidate.to_ascii_lowercase();
    if let Ok(lang) =
      <Lang as SupportedLanguage<'_, LanguageSetImpl>>::for_name(candidate.as_str(), language_set)
    {
      return Some(lang);
    }
    if let Some(lang) = <Lang as SupportedLanguage<'_, LanguageSetImpl>>::for_injection(
      candidate.as_str(),
      language_set,
    ) {
      return Some(lang);
    }
  }

  None
}

fn hyperpolyglot_name_for_token(raw: &str) -> Option<String> {
  let probes = [raw.to_string(), format!("file.{raw}")];
  for probe in probes {
    match hyperpolyglot::detect(std::path::Path::new(&probe)) {
      Ok(Some(d)) => return Some(d.language().to_string()),
      Ok(None) => {}
      Err(_) => {
        if let Some(name) = hyperpolyglot_name_via_tempfile(&probe) {
          return Some(name);
        }
      }
    }
  }
  None
}

fn hyperpolyglot_name_via_tempfile(probe: &str) -> Option<String> {
  use std::io::Write;

  let ext = std::path::Path::new(probe)
    .extension()?
    .to_str()?
    .to_ascii_lowercase();
  if ext.is_empty() {
    return None;
  }

  let base = format!(
    "remark-hyperpolyglot-probe-{}-{}",
    std::process::id(),
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .ok()?
      .as_nanos()
  );

  for attempt in 0..8u8 {
    let name = format!("{base}-{attempt}.{ext}");
    let path = std::env::temp_dir().join(name);
    let created = std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&path);
    let mut file = match created {
      Ok(f) => f,
      Err(_) => continue,
    };

    // Empty content can lead to unreliable classification for ambiguous extensions; give it a
    // tiny hint without hardcoding language tables.
    let _ = file.write_all(b"\n");

    let res = hyperpolyglot::detect(&path)
      .ok()
      .flatten()
      .map(|d| d.language().to_string());
    let _ = std::fs::remove_file(&path);
    return res;
  }

  None
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn resolves_ts_and_tsx() {
    let language_set = LanguageSetImpl::new();
    assert!(resolve_lang_token("ts", &language_set).is_some());
    assert!(resolve_lang_token("tsx", &language_set).is_some());
    assert!(resolve_lang_token("TypeScript", &language_set).is_some());
    assert!(resolve_lang_token("TSX", &language_set).is_some());
  }

  #[test]
  fn detects_typescript_from_repo_path() {
    let td = tempfile::tempdir().expect("tempdir");
    let repo = gix::init(td.path()).expect("init repo");
    std::fs::create_dir_all(td.path().join("src")).expect("mkdir src");
    std::fs::write(td.path().join("src/example.ts"), "const x: number = 1;\n").expect("write ts");

    let hl = Highlighter::new().expect("highlighter");
    let lang = hl.detect_file_lang(&repo, "src/example.ts");
    assert!(lang.is_some());
  }
}
