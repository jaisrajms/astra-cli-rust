//! Color themes for the TUI.
//!
//! A theme is a flat palette of semantic colors (accent, tool, ok, err, notice, dim). A few
//! built-in themes are shipped; the user cycles them at runtime.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    /// Primary accent (user prompt, agent, headings, borders, active tab highlight).
    pub accent: Color,
    /// Tool calls / running state.
    pub tool: Color,
    /// Success (checkmarks, inline code).
    pub ok: Color,
    /// Errors / denied.
    pub err: Color,
    /// Notices.
    pub notice: Color,
    /// Muted text (dim, code blocks, hints).
    pub dim: Color,
}

pub const THEMES: &[Theme] = &[
    Theme {
        name: "default",
        accent: Color::Cyan,
        tool: Color::Yellow,
        ok: Color::Green,
        err: Color::Red,
        notice: Color::Magenta,
        dim: Color::DarkGray,
    },
    Theme {
        name: "dracula",
        accent: Color::Magenta,
        tool: Color::Yellow,
        ok: Color::Green,
        err: Color::Red,
        notice: Color::LightMagenta,
        dim: Color::DarkGray,
    },
    Theme {
        name: "mono",
        accent: Color::White,
        tool: Color::White,
        ok: Color::Green,
        err: Color::Red,
        notice: Color::Gray,
        dim: Color::DarkGray,
    },
];

/// The theme for the given index (wrapped into range).
pub fn theme_at(index: usize) -> &'static Theme {
    &THEMES[index % THEMES.len()]
}
