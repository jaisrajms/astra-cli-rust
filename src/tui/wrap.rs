//! Display-width-aware text wrapping (Plan §5.4): a pure, deterministic helper that turns one
//! logical text line into zero-or-more visual rows at a given column width, using display width
//! (not byte length) so CJK/emoji/wide glyphs measure correctly.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The display width of `s` in columns (uses Unicode width, so wide glyphs count as 2).
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Split `text` into maximal runs of whitespace and non-whitespace, in order.
fn runs(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(first) = chars.next() {
        let is_ws = first.is_whitespace();
        let mut run = String::new();
        run.push(first);
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() == is_ws {
                run.push(c);
                chars.next();
            } else {
                break;
            }
        }
        out.push((run, is_ws));
    }
    out
}

/// Append `s` to the current row one character at a time, flushing full rows.
fn fill(s: &str, width: usize, cur: &mut String, cur_width: &mut usize, rows: &mut Vec<String>) {
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if !cur.is_empty() && *cur_width + cw > width {
            rows.push(std::mem::take(cur));
            *cur_width = 0;
        }
        cur.push(ch);
        *cur_width += cw;
    }
}

/// Wrap `text` into visual rows of at most `width` display columns.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    if text.chars().all(|c| c.is_whitespace()) {
        return vec![text.to_string()];
    }

    let mut rows: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_width = 0usize;
    let mut has_word = false;

    for (run, is_ws) in runs(text) {
        if is_ws {
            // Leading whitespace is preserved on the first row only; internal/trailing
            // whitespace collapses to a single space at the next word boundary.
            if !has_word && rows.is_empty() {
                fill(&run, width, &mut cur, &mut cur_width, &mut rows);
            }
            continue;
        }

        let word_width = display_width(&run);
        if has_word {
            if cur_width + 1 + word_width <= width {
                cur.push(' ');
                cur.push_str(&run);
                cur_width += 1 + word_width;
            } else {
                rows.push(std::mem::take(&mut cur));
                cur_width = 0;
                fill(&run, width, &mut cur, &mut cur_width, &mut rows);
                has_word = true;
            }
        } else if cur_width + word_width <= width {
            cur.push_str(&run);
            cur_width += word_width;
            has_word = true;
        } else {
            fill(&run, width, &mut cur, &mut cur_width, &mut rows);
            has_word = true;
        }
    }

    if !cur.is_empty() {
        rows.push(cur);
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str, width: usize, expected: &[&str]) {
        let rows = wrap_text(text, width);
        let expected_rows: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        assert_eq!(rows, expected_rows, "text={text:?} width={width}");
        if width > 0 {
            for row in &rows {
                assert!(
                    display_width(row) <= width,
                    "row {row:?} (width {}) exceeds {width}",
                    display_width(row)
                );
            }
        }
    }

    #[test]
    fn short_line_fits() {
        check("hello", 20, &["hello"]);
    }

    #[test]
    fn prose_word_wrap() {
        check("the quick brown fox", 10, &["the quick", "brown fox"]);
    }

    #[test]
    fn long_token_hard_breaks() {
        check("abcdefghij", 4, &["abcd", "efgh", "ij"]);
    }

    #[test]
    fn leading_whitespace_indentation() {
        check("    hello", 20, &["    hello"]);
        check("    indented", 8, &["    inde", "nted"]);
    }

    #[test]
    fn empty_string() {
        check("", 5, &[""]);
    }

    #[test]
    fn width_zero_is_identity() {
        check("hello world", 0, &["hello world"]);
    }

    #[test]
    fn exactly_at_width() {
        check("abc", 3, &["abc"]);
        check("abcdefgh", 4, &["abcd", "efgh"]);
    }

    #[test]
    fn cjk_wide_glyphs() {
        check("你好世界", 4, &["你好", "世界"]);
    }

    #[test]
    fn emoji() {
        check("😀😀", 2, &["😀", "😀"]);
    }

    #[test]
    fn tab_collapses_to_space() {
        check("a\tb", 10, &["a b"]);
    }

    #[test]
    fn whitespace_only_is_preserved() {
        assert_eq!(wrap_text("   ", 2), vec!["   ".to_string()]);
    }

    #[test]
    fn mixed_wide_glyph_word_wrap() {
        check("hello 世界", 8, &["hello", "世界"]);
    }

    #[test]
    fn wide_glyph_narrow_width_does_not_panic() {
        let rows = wrap_text("你好", 1);
        assert_eq!(rows.len(), 2);
    }
}
