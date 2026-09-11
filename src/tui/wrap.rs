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

/// Hard-break `s` into chunks of at most `width` display columns. Used for words wider than a row.
fn hard_break(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut chunk = String::new();
    let mut chunk_w = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if chunk_w + cw > width && !chunk.is_empty() {
            out.push(std::mem::take(&mut chunk));
            chunk_w = 0;
        }
        chunk.push(ch);
        chunk_w += cw;
    }
    if !chunk.is_empty() {
        out.push(chunk);
    }
    out
}

/// Hard-break `s` with a narrower first chunk (`first` columns) and `rest`-column chunks after.
fn hard_break_prefix(s: &str, first: usize, rest: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut chunk = String::new();
    let mut chunk_w = 0usize;
    let mut limit = first;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if chunk_w + cw > limit && !chunk.is_empty() {
            out.push(std::mem::take(&mut chunk));
            chunk_w = 0;
            limit = rest;
        }
        chunk.push(ch);
        chunk_w += cw;
    }
    if !chunk.is_empty() {
        out.push(chunk);
    }
    out
}

/// Merge a word (or its first hard-broken chunk) into the current row, which holds only leading
/// indentation, so the indentation stays attached to the first visual row.
fn merge_into_indent(run: &str, cur: &mut String, rows: &mut Vec<String>, width: usize) {
    let avail = width.saturating_sub(display_width(cur));
    let chunks = hard_break_prefix(run, avail.max(1), width);
    if let Some(first) = chunks.first() {
        cur.push_str(first);
    }
    rows.push(std::mem::take(cur));
    rows.extend(chunks.into_iter().skip(1));
}

/// Wrap `text` into visual rows of at most `width` display columns, preserving whitespace runs
/// verbatim (multiple spaces/tabs are never collapsed). Whitespace is held with the following word;
/// the separator at a wrap point is consumed by the line break, and leading indentation stays on the
/// first row.
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
    let mut pending_ws = String::new();

    for (run, is_ws) in runs(text) {
        if is_ws {
            // Leading whitespace of a fresh row is indentation; internal whitespace is held back so
            // it is only committed when the next word fits on the same row.
            if cur.is_empty() {
                cur.push_str(&run);
                cur_width += display_width(&run);
            } else {
                pending_ws.push_str(&run);
            }
            continue;
        }

        let word_w = display_width(&run);

        // A word wider than a whole row is hard-broken (its pending separator is dropped).
        if word_w > width {
            if cur.chars().all(char::is_whitespace) {
                merge_into_indent(&run, &mut cur, &mut rows, width);
                cur_width = 0;
            } else {
                if !cur.is_empty() {
                    rows.push(std::mem::take(&mut cur));
                    cur_width = 0;
                }
                rows.extend(hard_break(&run, width));
            }
            pending_ws.clear();
            continue;
        }

        let ws_w = display_width(&pending_ws);
        if cur_width + ws_w + word_w <= width {
            // Fits: commit the pending whitespace verbatim, then the word.
            cur.push_str(&pending_ws);
            cur.push_str(&run);
            cur_width += ws_w + word_w;
        } else {
            // Doesn't fit. If the current row is only indentation, merge the word's first chunk into
            // it so the indent stays attached; otherwise start a fresh row.
            if cur.chars().all(char::is_whitespace) {
                merge_into_indent(&run, &mut cur, &mut rows, width);
                cur_width = 0;
            } else {
                if !cur.is_empty() {
                    rows.push(std::mem::take(&mut cur));
                }
                cur.push_str(&run);
                cur_width = word_w;
            }
        }
        pending_ws.clear();
    }

    if !cur.is_empty() {
        rows.push(cur);
    }
    if rows.is_empty() {
        rows.push(String::new());
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
    fn tab_is_preserved() {
        check("a\tb", 10, &["a\tb"]);
    }

    #[test]
    fn multiple_spaces_are_preserved() {
        check("a  b", 20, &["a  b"]);
        // Two spaces kept on the row that fits; the separator at the wrap point is consumed.
        check("a  b  c", 4, &["a  b", "c"]);
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
