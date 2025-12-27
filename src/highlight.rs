use anyhow::{Context, Result};
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span, Text};

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
            theme: syntastica_themes::gruvbox::dark(),
        })
    }

    pub fn highlight_diff(&self, text: &str) -> Result<Text<'static>> {
        let highlights = syntastica::Processor::process_once(text, Lang::Diff, &self.language_set)
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
            lines.push(Line::from(spans));
        }

        Ok(Text::from(lines))
    }
}

fn syn_style_to_tui(style: &SynStyle) -> ratatui::style::Style {
    let mut s = ratatui::style::Style::default();
    let c = style.color();
    s = s.fg(Color::Rgb(c.red, c.green, c.blue));
    if let Some(c) = style.bg() {
        s = s.bg(Color::Rgb(c.red, c.green, c.blue));
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
