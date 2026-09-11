//! Transcript measurement (Plan §5.4): turn `App.items` + streaming text into measured,
//! already-wrapped rows (with a stable item id for hit-testing) so the scroll offset is computed in
//! visual rows rather than source lines. `render` consumes these rows and paints only the visible
//! window, so the scroll math and the paint pass agree exactly.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::app::{App, Item, ToolOutcome};
use super::theme::Theme;
use super::wrap::{display_width, wrap_text};
use crate::tool::{tool_icon, tool_label};

/// One measured, already-wrapped transcript row.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasuredRow {
    /// The stable id of the item this row belongs to (tool/reasoning), or `None`.
    pub item_id: Option<String>,
    pub line: Line<'static>,
}

/// Build every transcript row and wrap it at `width` display columns.
pub fn measure(app: &App, width: u16, theme: Theme) -> Vec<MeasuredRow> {
    let width = width as usize;
    let mut rows: Vec<MeasuredRow> = Vec::new();

    for item in &app.items {
        let id = match item {
            Item::ToolCall { id, .. } => Some(id.clone()),
            Item::Reasoning { id, .. } => Some(id.clone()),
            _ => None,
        };
        for line in item_lines(app, item, theme) {
            for wrapped in wrap_line(line, width) {
                rows.push(MeasuredRow {
                    item_id: id.clone(),
                    line: wrapped,
                });
            }
        }
    }

    if !app.streaming.is_empty() {
        for line in markdown_lines(&app.streaming, theme) {
            for wrapped in wrap_line(line, width) {
                rows.push(MeasuredRow {
                    item_id: None,
                    line: wrapped,
                });
            }
        }
    }

    if rows.is_empty() {
        rows.push(MeasuredRow {
            item_id: None,
            line: Line::from(Span::styled(
                "No messages yet — type a prompt and press Enter.",
                Style::default().fg(theme.dim).italic(),
            )),
        });
    }

    rows
}

/// Wrap one `Line` at `width` columns, preserving leading indentation on the first row. A line that
/// already fits is returned as-is (keeping its span styling); a wrapped line is rebuilt as plain
/// rows (the common wrapped content — prose/code/output — is single-styled anyway).
fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let text = line.to_string();
    if width == 0 || display_width(&text) <= width {
        return vec![line];
    }
    let rows = wrap_text(&text, width);
    if rows.len() <= 1 {
        return vec![line];
    }
    rows.into_iter().map(Line::raw).collect()
}

fn item_lines(app: &App, item: &Item, theme: Theme) -> Vec<Line<'static>> {
    match item {
        Item::User(text) => vec![Line::from(vec![
            Span::styled("❯ ", Style::default().fg(theme.accent).bold()),
            Span::raw(text.clone()),
        ])],
        Item::Assistant(text) => markdown_lines(text, theme),
        Item::ToolCall {
            name,
            input,
            outcome,
            ..
        } => tool_lines(app, name, input, outcome.as_ref(), theme),
        Item::Notice(text) => vec![Line::from(Span::styled(
            text.clone(),
            Style::default().fg(theme.notice),
        ))],
        Item::Reasoning { text, .. } => {
            // Collapsed: a "Thinking" header with a short preview (the reference CLI collapses
            // reasoning; the body is only shown on expand).
            let preview: String = text.lines().next().unwrap_or("").chars().take(60).collect();
            let ellipsis = if text.chars().count() > 60 { "…" } else { "" };
            vec![Line::from(vec![
                Span::styled(" ✦ Thinking · ", Style::default().fg(theme.dim).italic()),
                Span::styled(
                    format!("{preview}{ellipsis}"),
                    Style::default().fg(theme.dim),
                ),
            ])]
        }
        Item::Error(text) => vec![Line::from(Span::styled(
            format!(" ✗ {text}"),
            Style::default().fg(theme.err),
        ))],
        Item::Done(result) => vec![Line::from(Span::styled(
            result.clone(),
            Style::default().fg(theme.accent),
        ))],
    }
}

/// Render a tool call as a single inline row (icon + label), the way the reference CLI does: a
/// spinner + active color while running, then the tool icon in muted/error color once finished.
/// `Bash`/`ShellBash` additionally show their output beneath the command line.
fn tool_lines(
    app: &App,
    name: &str,
    input: &str,
    outcome: Option<&ToolOutcome>,
    theme: Theme,
) -> Vec<Line<'static>> {
    let label = tool_label(name, input);
    let icon = tool_icon(name);
    let is_shell = name == "Bash" || name == "ShellBash";

    let Some(out) = outcome else {
        // In flight: spinner glyph in the icon column.
        return vec![Line::from(vec![
            Span::raw("   "),
            Span::styled(app.spinner(), Style::default().fg(theme.tool)),
            Span::raw(" "),
            Span::styled(label, Style::default().fg(theme.tool)),
        ])];
    };

    let color = if out.ok { theme.dim } else { theme.err };
    let mut lines = vec![Line::from(vec![
        Span::raw("   "),
        Span::styled(icon, Style::default().fg(color)),
        Span::raw(" "),
        Span::styled(label, Style::default().fg(color)),
    ])];

    if is_shell {
        let shown = if out.output.is_empty() {
            out.summary.as_str()
        } else {
            out.output.as_str()
        };
        for out_line in shown.lines().take(12) {
            lines.push(Line::from(Span::styled(
                format!("     {out_line}"),
                Style::default().fg(color),
            )));
        }
    } else if !out.ok && !out.summary.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("     {}", out.summary),
            Style::default().fg(theme.err),
        )));
    }
    lines
}

/// Render markdown-ish assistant text into styled lines: fenced code blocks (```), headings
/// (#/##), bullets (- / *), inline code (`) and bold (**) get basic styling; everything else is
/// plain. This is a lightweight approximation of the reference CLI's markdown renderer (no
/// full syntax highlighter).
pub(crate) fn markdown_lines(text: &str, theme: Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut code_buf: Vec<Line<'static>> = Vec::new();
    let mut in_code = false;

    for raw in text.lines() {
        if raw.trim_start().starts_with("```") {
            if in_code {
                lines.append(&mut code_buf);
                in_code = false;
            } else {
                in_code = true;
            }
            continue;
        }
        if in_code {
            code_buf.push(Line::from(Span::styled(
                raw.to_string(),
                Style::default().fg(theme.dim),
            )));
            continue;
        }
        if raw.trim().is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        let leading = raw.len() - raw.trim_start().len();
        let body = &raw[leading..];
        if let Some(rest) = body.strip_prefix('#') {
            lines.push(Line::from(Span::styled(
                rest.trim_start().to_string(),
                Style::default().fg(theme.accent).bold(),
            )));
        } else if let Some(rest) = body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
            let mut spans = vec![Span::styled("  • ", Style::default().fg(theme.dim))];
            spans.extend(inline_spans(rest, theme));
            lines.push(Line::from(spans));
        } else {
            lines.push(Line::from(inline_spans(body, theme)));
        }
    }
    if in_code {
        lines.append(&mut code_buf);
    }
    lines
}

/// Tokenize inline `**bold**` and `` `code` `` spans; other text stays plain.
pub(crate) fn inline_spans(text: &str, theme: Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(idx) = rest.find("**") {
            if idx > 0 {
                spans.push(Span::raw(rest[..idx].to_string()));
            }
            let after = &rest[idx + 2..];
            match after.find("**") {
                Some(end) => {
                    spans.push(Span::styled(
                        after[..end].to_string(),
                        Style::default().bold(),
                    ));
                    rest = &after[end + 2..];
                }
                None => {
                    spans.push(Span::raw(after.to_string()));
                    rest = "";
                }
            }
        } else if let Some(idx) = rest.find('`') {
            if idx > 0 {
                spans.push(Span::raw(rest[..idx].to_string()));
            }
            let after = &rest[idx + 1..];
            match after.find('`') {
                Some(end) => {
                    spans.push(Span::styled(
                        after[..end].to_string(),
                        Style::default().fg(theme.ok),
                    ));
                    // The closing backtick is 1 byte (not 2 like `**`).
                    rest = &after[end + 1..];
                }
                None => {
                    spans.push(Span::raw(after.to_string()));
                    rest = "";
                }
            }
        } else {
            spans.push(Span::raw(rest.to_string()));
            rest = "";
        }
    }
    spans
}
