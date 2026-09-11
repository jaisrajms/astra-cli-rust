//! Bounded tool-output helpers (Plan §5.4/§9): strip ANSI escapes before display, and collapse
//! oversized output to a bounded preview with an overflow flag.

/// The result of collapsing oversized output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collapsed {
    /// The (possibly truncated) preview text.
    pub output: String,
    /// Whether the input was truncated (i.e. `output` is shorter than the input).
    pub overflow: bool,
}

/// Remove ANSI escape sequences (CSI, OSC, and single-char `ESC` sequences) from `s`.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            // CSI: `ESC '['` params/intermediates (0x30..=0x3F) then one final byte (0x40..=0x7E).
            Some('[') => {
                chars.next();
                for p in chars.by_ref() {
                    let b = p as u32;
                    if (0x30..=0x3F).contains(&b) {
                        continue;
                    }
                    break;
                }
            }
            // OSC: `ESC ']'` up to and including BEL (0x07) or ST (`ESC '\\'`).
            Some(']') => {
                chars.next();
                for p in chars.by_ref() {
                    if p == '\x07' {
                        break;
                    }
                    if p == '\x1b' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Any other two-byte `ESC X` sequence.
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Collapse `s` to at most `max_lines` lines and `max_chars` Unicode scalar values (code points).
pub fn collapse_output(s: &str, max_lines: usize, max_chars: usize) -> Collapsed {
    let lines: Vec<&str> = s.split('\n').collect();
    if lines.len() <= max_lines && s.chars().count() <= max_chars {
        return Collapsed {
            output: s.to_string(),
            overflow: false,
        };
    }

    let preview: String = lines
        .iter()
        .take(max_lines)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if preview.chars().count() > max_chars {
        let take = max_chars.saturating_sub(1);
        let head: String = preview.chars().take(take).collect();
        Collapsed {
            output: format!("{head}…"),
            overflow: true,
        }
    } else {
        Collapsed {
            output: format!("{preview}\n…"),
            overflow: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_plain_text_passthrough() {
        assert_eq!(strip_ansi("hello world"), "hello world");
        assert_eq!(strip_ansi(""), "");
        assert_eq!(strip_ansi("no escape here [31m"), "no escape here [31m");
    }

    #[test]
    fn strip_csi_color() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn strip_csi_many_params() {
        assert_eq!(strip_ansi("\x1b[0;1;34mblue\x1b[m"), "blue");
        assert_eq!(strip_ansi("a\x1b[2Jb"), "ab");
    }

    #[test]
    fn strip_osc_title_bel() {
        assert_eq!(strip_ansi("\x1b]0;title\x07prompt"), "prompt");
    }

    #[test]
    fn strip_osc_title_st() {
        assert_eq!(strip_ansi("\x1b]0;title\x1b\\prompt"), "prompt");
    }

    #[test]
    fn strip_two_byte_escape() {
        assert_eq!(strip_ansi("\x1bcreset"), "reset");
    }

    #[test]
    fn strip_utf8_passthrough() {
        assert_eq!(strip_ansi("你好世界"), "你好世界");
        assert_eq!(strip_ansi("emoji 😀 and more"), "emoji 😀 and more");
    }

    #[test]
    fn collapse_under_both_limits() {
        let c = collapse_output("abc\ndef", 10, 100);
        assert_eq!(c.output, "abc\ndef");
        assert!(!c.overflow);
    }

    #[test]
    fn collapse_by_line_count() {
        let c = collapse_output("a\nb\nc\nd\ne", 3, 100);
        assert_eq!(c.output, "a\nb\nc\n…");
        assert!(c.overflow);
    }

    #[test]
    fn collapse_by_char_count() {
        let c = collapse_output("abcdefghij", 100, 5);
        assert_eq!(c.output, "abcd…");
        assert!(c.overflow);
    }

    #[test]
    fn collapse_by_char_count_multibyte_boundary() {
        // The cut lands right after a multi-byte CJK char without splitting it.
        let c = collapse_output("abc你好def", 100, 6);
        assert_eq!(c.output, "abc你好…");
        assert!(c.overflow);
        assert_eq!(c.output.chars().count(), 6);
    }

    #[test]
    fn collapse_ellipsis_suffix_behavior() {
        // Char-limited: ellipsis directly appended, bounded to max_chars code points.
        let by_chars = collapse_output("hello world", 100, 6);
        assert_eq!(by_chars.output, "hello…");
        assert!(by_chars.output.ends_with('…'));
        assert!(!by_chars.output.contains('\n'));

        // Line-limited: ellipsis on its own trailing line.
        let by_lines = collapse_output("one\ntwo", 1, 100);
        assert_eq!(by_lines.output, "one\n…");
        assert!(by_lines.output.ends_with("\n…"));
    }
}
