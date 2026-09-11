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

/// The starting theme index from environment variables only (no terminal query): the first light
/// theme when `$TERM_BACKGROUND`/`$COLORFGBG` report a light background, else the default dark theme.
/// Used by [`crate::tui::app::App::new`], which must stay free of terminal I/O (tests construct it
/// without a terminal).
pub fn default_theme_index() -> usize {
    if env_light() == Some(true) {
        LIGHT_THEMES_START
    } else {
        0
    }
}

/// The full starting-theme detection: environment variables first, then an OSC 11 background-color
/// query as a fallback when neither env var is set, then dark. Must run AFTER raw mode is entered
/// (so the tty is non-canonical and the reply is readable), but BEFORE the crossterm event stream is
/// created (so the raw read does not contend with it).
pub fn detect_theme_index() -> usize {
    match env_light() {
        Some(true) => LIGHT_THEMES_START,
        Some(false) => 0,
        None => match query_background_via_osc11() {
            Some(true) => LIGHT_THEMES_START,
            _ => 0,
        },
    }
}

/// The env-var detection result, or `None` when neither variable is set (the OSC 11 fallback then
/// applies).
fn env_light() -> Option<bool> {
    if let Ok(bg) = std::env::var("TERM_BACKGROUND") {
        // An explicit `TERM_BACKGROUND` wins (only "light" selects the light themes).
        return Some(bg.eq_ignore_ascii_case("light"));
    }
    if let Ok(value) = std::env::var("COLORFGBG") {
        // Convention: "fg;bg" where 15 = bright white, 7 = white.
        let bg = value.split(';').nth(1).unwrap_or("");
        return Some(matches!(bg.trim(), "15" | "7"));
    }
    None
}

/// Query the terminal's default background via OSC 11, returning `Some(true)` for a light
/// background and `Some(false)` for dark; `None` when the terminal does not answer (timed out).
///
/// Reads the reply with a raw `poll` + `read` on fd 0 (stdin) rather than `tokio`/`std::io::stdin`,
/// so it neither blocks the async runtime nor takes over the fd that crossterm's event reader uses
/// next.
#[cfg(unix)]
fn query_background_via_osc11() -> Option<bool> {
    use std::io::Write;
    use std::os::fd::AsRawFd;

    let mut stdout = std::io::stdout();
    stdout.write_all(b"\x1b]11;?\x1b\\").ok()?;
    stdout.flush().ok()?;

    let fd = std::io::stdin().as_raw_fd();
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut pfd, 1, 150) };
    if ready <= 0 {
        return None; // timed out or poll failed
    }

    let mut buf = [0u8; 512];
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n <= 0 {
        return None;
    }
    parse_osc11_response(&String::from_utf8_lossy(&buf[..n as usize]))
}

#[cfg(not(unix))]
fn query_background_via_osc11() -> Option<bool> {
    None
}

/// Parse an OSC 11 reply (e.g. `ESC]11;rgb:RRRR/GGGG/BBBB` + BEL/ST) into a light/dark verdict from
/// its relative luminance (> 0.5 is light).
fn parse_osc11_response(reply: &str) -> Option<bool> {
    let body = reply
        .trim_start_matches('\x1b')
        .strip_prefix("]11;")?
        .trim();
    // Strip a trailing ST (`ESC \`) or BEL (`\x07`) terminator: drop the backslash first, then the
    // ESC, then a BEL.
    let body = body
        .trim_end_matches('\\')
        .trim_end_matches('\x1b')
        .trim_end_matches('\x07')
        .trim();
    let (r, g, b) = parse_rgb(body)?;
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    Some(lum > 0.5)
}

/// Parse `#RRGGBB` or `rgb:RRRR/GGGG/BBBB` (16-bit) / `rgb:RR/GG/BB` (8-bit) into normalized
/// `(r, g, b)` floats.
fn parse_rgb(body: &str) -> Option<(f64, f64, f64)> {
    if let Some(hex) = body.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some((r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0));
        }
    }
    if let Some(rgb) = body.strip_prefix("rgb:") {
        let parts: Vec<&str> = rgb.split('/').collect();
        if parts.len() == 3 {
            let parse = |p: &str| -> Option<f64> {
                if p.len() == 4 {
                    u16::from_str_radix(p, 16).ok().map(|v| v as f64 / 65535.0)
                } else if p.len() == 2 {
                    u8::from_str_radix(p, 16).ok().map(|v| v as f64 / 255.0)
                } else {
                    None
                }
            };
            return Some((parse(parts[0])?, parse(parts[1])?, parse(parts[2])?));
        }
    }
    None
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
    fn env_detection_prefers_explicit_term_background() {
        assert_eq!(env_light(), None); // no env vars in the test process
    }

    #[test]
    fn osc11_parses_rgb_and_luminance() {
        // White background -> light.
        assert_eq!(
            parse_osc11_response("\x1b]11;rgb:ffff/ffff/ffff\x1b\\"),
            Some(true)
        );
        // Black background -> dark.
        assert_eq!(
            parse_osc11_response("\x1b]11;rgb:0000/0000/0000\x07"),
            Some(false)
        );
        // A dark gray (#202020) -> dark.
        assert_eq!(parse_osc11_response("\x1b]11;#202020\x1b\\"), Some(false));
        // 8-bit form.
        assert_eq!(parse_osc11_response("\x1b]11;rgb:ff/ff/ff\x07"), Some(true));
        // Malformed -> None.
        assert_eq!(parse_osc11_response("garbage"), None);
        assert_eq!(
            parse_osc11_response("\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
            None
        );
    }

    #[test]
    fn rgb_parser_handles_forms() {
        assert_eq!(parse_rgb("#ffffff"), Some((1.0, 1.0, 1.0)));
        assert_eq!(parse_rgb("#000000"), Some((0.0, 0.0, 0.0)));
        assert_eq!(parse_rgb("rgb:0000/0000/0000"), Some((0.0, 0.0, 0.0)));
        assert_eq!(parse_rgb("rgb:ff/00/00"), Some((1.0, 0.0, 0.0)));
        assert_eq!(parse_rgb("nonsense"), None);
    }
}
