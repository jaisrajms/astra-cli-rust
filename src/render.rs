//! Plain-text / table rendering helpers for stdout.

/// Render an aligned plain-text table with a header row and a `─` separator.
pub fn table(headers: &[&str], rows: &[Vec<String>]) {
    print!("{}", table_string(headers, rows));
}

/// Build the same aligned table as a string (trailing newline included), so
/// callers can render to a buffer or assert on it in tests.
pub fn table_string(headers: &[&str], rows: &[Vec<String>]) -> String {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    let mut header = String::new();
    for (i, h) in headers.iter().enumerate() {
        header.push_str(&pad(h, widths[i]));
        if i + 1 < cols {
            header.push_str("  ");
        }
    }
    out.push_str(&header);
    out.push('\n');
    out.push_str(&"─".repeat(header.chars().count()));
    out.push('\n');

    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            line.push_str(&pad(cell, widths[i]));
            if i + 1 < cols {
                line.push_str("  ");
            }
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_short_and_long() {
        assert_eq!(pad("a", 3), "a  ");
        assert_eq!(pad("abcd", 2), "abcd");
        assert_eq!(pad("", 2), "  ");
    }
}
