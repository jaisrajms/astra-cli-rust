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
    // Light-background variants. Avoid Yellow/White/Light* accents, which lose contrast on white;
    // the "normal" ANSI hues (Blue/Magenta/Green/Red/Cyan) map to dark-enough shades on light
    // terminals to stay readable.
    Theme {
        name: "default-light",
        accent: Color::Blue,
        tool: Color::Magenta,
        ok: Color::Green,
        err: Color::Red,
        notice: Color::Cyan,
        dim: Color::DarkGray,
    },
    Theme {
        name: "dracula-light",
        accent: Color::Magenta,
        tool: Color::Blue,
        ok: Color::Green,
        err: Color::Red,
        notice: Color::Cyan,
        dim: Color::DarkGray,
    },
    Theme {
        name: "mono-light",
        accent: Color::Black,
        tool: Color::Black,
        ok: Color::Green,
        err: Color::Red,
        notice: Color::DarkGray,
        dim: Color::DarkGray,
    },
];

/// The index of the first light theme.
const LIGHT_THEMES_START: usize = 3;

/// The theme for the given index (wrapped into range).
pub fn theme_at(index: usize) -> &'static Theme {
    &THEMES[index % THEMES.len()]
}

/// The starting theme index for the current terminal: the first light theme when the terminal
/// reports a light background, else the default dark theme.
///
/// Detection is best-effort and conservative (defaults to dark — the common dev-terminal case):
///   - `$TERM_BACKGROUND` == "light" wins;
///   - else `$COLORFGBG` (the `fg;bg` convention) where the background is a light ANSI color
///     (7 = white, 15 = bright white).
pub fn default_theme_index() -> usize {
    if is_light_terminal() {
        LIGHT_THEMES_START
    } else {
        0
    }
}

fn is_light_terminal() -> bool {
    let term_background = std::env::var("TERM_BACKGROUND").ok();
    let colorfgbg = std::env::var("COLORFGBG").ok();
    is_light(term_background.as_deref(), colorfgbg.as_deref())
}

/// Pure form of [`is_light_terminal`] (testable without touching the process env).
fn is_light(term_background: Option<&str>, colorfgbg: Option<&str>) -> bool {
    if let Some(bg) = term_background {
        // An explicit `TERM_BACKGROUND` wins (only "light" selects the light themes).
        return bg.eq_ignore_ascii_case("light");
    }
    if let Some(value) = colorfgbg {
        let bg = value.split(';').nth(1).unwrap_or("");
        return matches!(bg.trim(), "15" | "7");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_at_wraps_and_light_themes_exist() {
        assert_eq!(theme_at(0).name, "default");
        assert_eq!(theme_at(3).name, "default-light");
        assert_eq!(theme_at(THEMES.len()).name, "default");
    }

    #[test]
    fn light_detection_prefers_explicit_term_background() {
        assert!(is_light(Some("light"), None));
        assert!(!is_light(Some("dark"), Some("0;15")));
        assert!(!is_light(None, None));
    }

    #[test]
    fn light_detection_reads_colorfgbg_background() {
        assert!(is_light(None, Some("0;15")));
        assert!(is_light(None, Some("0;7")));
        assert!(!is_light(None, Some("15;0")));
        assert!(!is_light(None, Some("garbage")));
    }
}
