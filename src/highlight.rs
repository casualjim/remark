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

    pub fn detect_file_lang(
        &self,
        repo: &gix::Repository,
        repo_relative_path: &str,
    ) -> Option<Lang> {
        if let Some(wt) = repo.workdir() {
            let abs = wt.join(repo_relative_path);
            if abs.is_file() {
                if let Ok(Some(d)) = hyperpolyglot::detect(&abs) {
                    if let Ok(lang) = Lang::for_name(d.language(), &self.language_set) {
                        return Some(lang);
                    }
                }
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
