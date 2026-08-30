//! Background tints for changed diff lines, matching git-delta's look
//! (delta 0.19.2, Catppuccin-mocha, dark, truecolor).

use ratatui::style::Color;

pub const MINUS_BG: Color = Color::Rgb(63, 0, 1); // #3f0001
pub const MINUS_EMPH_BG: Color = Color::Rgb(144, 16, 17); // #901011
pub const PLUS_BG: Color = Color::Rgb(0, 40, 0); // #002800
pub const PLUS_EMPH_BG: Color = Color::Rgb(0, 96, 0); // #006000
