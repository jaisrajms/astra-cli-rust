//! Plain-text / table rendering helpers for stdout.

/// Render an aligned plain-text table with a header row and a `─` separator.
pub fn table(headers: &[&str], rows: &[Vec<String>]) {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut header = String::new();
    for (i, h) in headers.iter().enumerate() {
        header.push_str(&pad(h, widths[i]));
        if i + 1 < cols {
            header.push_str("  ");
        }
    }
    println!("{header}");
    println!("{}", "─".repeat(header.chars().count()));

    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            line.push_str(&pad(cell, widths[i]));
            if i + 1 < cols {
                line.push_str("  ");
            }
        }
        println!("{line}");
    }
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
