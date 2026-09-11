//! Transcript measurement (Plan §5.4): turn `App.items` + streaming text into measured,
//! already-wrapped rows (with a stable item id for hit-testing) so the scroll offset is computed in
//! visual rows rather than source lines. `render` consumes these rows and paints only the visible
//! window, so the scroll math and the paint pass agree exactly.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::app::{App, Item, RowItem, ToolOutcome};
use super::theme::Theme;
use super::tool_output::{collapse_output, strip_ansi};
use super::wrap::{display_width, wrap_text};
use crate::tool::{tool_icon, tool_label};

/// The bounded line count for a collapsed shell tool block.
const SHELL_MAX_LINES: usize = 10;

/// One measured, already-wrapped transcript row.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasuredRow {
    /// The item this row belongs to (a tool/reasoning block), or `None`.
    pub item_id: Option<RowItem>,
    pub line: Line<'static>,
}

/// Build every transcript row and wrap it at `width` display columns.
pub fn measure(app: &App, width: u16, theme: Theme) -> Vec<MeasuredRow> {
    let width = width as usize;
    let mut rows: Vec<MeasuredRow> = Vec::new();

    for item in &app.items {
        let id = match item {
            Item::ToolCall { id, .. } => Some(RowItem::Tool(id.clone())),
            Item::Reasoning { id, .. } => Some(RowItem::Reasoning(id.clone())),
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
pub(crate) fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
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
            id,
            name,
            input,
            outcome,
        } => tool_lines(app, Some(id), name, input, outcome.as_ref(), theme),
        Item::Notice(text) => vec![Line::from(Span::styled(
            text.clone(),
            Style::default().fg(theme.notice),
        ))],
        Item::Reasoning {
            id,
            text,
            start_ms,
            end_ms,
        } => reasoning_lines(app, id, text, *start_ms, *end_ms, theme),
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

/// The reasoning block: a compact "Thinking" header (with duration when the daemon supplied
/// timing), plus the full body when expanded.
fn reasoning_lines(
    app: &App,
    id: &str,
    text: &str,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    theme: Theme,
) -> Vec<Line<'static>> {
    let duration = match (start_ms, end_ms) {
        (Some(s), Some(e)) => Some((e.saturating_sub(s)) as f64 / 1000.0),
        _ => None,
    };
    let preview: String = text.lines().next().unwrap_or("").chars().take(60).collect();

    let label = if text.is_empty() {
        "Thinking".to_string()
    } else {
        let mut s = format!("Thinking: {preview}");
        if text.chars().count() > 60 {
            s.push('…');
        }
        s
    };
    let mut spans = vec![Span::styled(" ✦ ", Style::default().fg(theme.dim).italic())];
    spans.push(Span::styled(label, Style::default().fg(theme.dim)));
    if let Some(secs) = duration {
        spans.push(Span::styled(
            format!(" · {secs:.1}s"),
            Style::default().fg(theme.dim),
        ));
    }
    let mut lines = vec![Line::from(spans)];

    if app.ui.expanded_reasoning.contains(id) && !text.is_empty() {
        lines.push(Line::from(""));
        lines.extend(markdown_lines(text, theme));
    }
    lines
}

/// Render a tool call as a single inline row (icon + label), the way the reference CLI does: a
/// spinner + active color while running, then the tool icon in muted/error color once finished.
/// `Bash`/`ShellBash` show their (ANSI-stripped, bounded) output beneath the command line, with an
/// expand/collapse affordance keyed by the tool's stable id.
fn tool_lines(
    app: &App,
    id: Option<&str>,
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

    // Decorate the tool line by status: green icon + label on success, red on failure.
    let status = if out.ok { theme.ok } else { theme.err };
    let mut lines = vec![Line::from(vec![
        Span::raw("   "),
        Span::styled(icon, Style::default().fg(status).bold()),
        Span::raw(" "),
        Span::styled(label, Style::default().fg(status)),
    ])];

    if is_shell {
        let raw = if out.output.is_empty() {
            out.summary.as_str()
        } else {
            out.output.as_str()
        };
        let clean = strip_ansi(raw);
        let expanded = id
            .map(|i| app.ui.expanded_tools.contains(i))
            .unwrap_or(false);

        let (shown, overflow) = if expanded {
            (clean.clone(), false)
        } else {
            let c = collapse_output(&clean, SHELL_MAX_LINES, SHELL_MAX_LINES * 120);
            (c.output, c.overflow)
        };

        for out_line in shown.lines() {
            lines.push(Line::from(Span::styled(
                format!("     {out_line}"),
                Style::default().fg(status),
            )));
        }
        if expanded {
            lines.push(Line::from(Span::styled(
                "     … click to collapse",
                Style::default().fg(theme.dim).italic(),
            )));
        } else if overflow {
            lines.push(Line::from(Span::styled(
                "     … click to expand",
                Style::default().fg(theme.dim).italic(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::theme_at;

    fn theme() -> Theme {
        *theme_at(0)
    }

    #[test]
    fn measure_tags_tool_rows_with_row_item() {
        let mut app = App::new(vec!["build".into()]);
        app.apply_agent_event(&astra_proto::astra::engine::v1::AgentEvent {
            kind: Some(astra_proto::astra::engine::v1::agent_event::Kind::ToolCall(
                astra_proto::astra::engine::v1::ToolCallEvent {
                    id: "tc".into(),
                    name: "Read".into(),
                    input: "{}".into(),
                },
            )),
        });
        let rows = measure(&app, 60, theme());
        assert!(matches!(
            rows[0].item_id,
            Some(RowItem::Tool(ref id)) if id == "tc"
        ));
    }

    #[test]
    fn shell_output_is_bounded_and_expandable() {
        let mut app = App::new(vec!["build".into()]);
        let output = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.items.push(Item::ToolCall {
            id: "sh".into(),
            name: "Bash".into(),
            input: r#"{"command":"ls"}"#.into(),
            outcome: Some(ToolOutcome {
                ok: true,
                summary: String::new(),
                output,
            }),
        });

        let collapsed = measure(&app, 60, theme());
        let text = collapsed
            .iter()
            .map(|r| r.line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("click to expand"));
        assert!(!text.contains("line 19"));

        app.ui.expanded_tools.insert("sh".into());
        let expanded = measure(&app, 60, theme());
        let text2 = expanded
            .iter()
            .map(|r| r.line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text2.contains("line 19"));
        assert!(text2.contains("click to collapse"));
    }

    #[test]
    fn reasoning_header_includes_duration_when_present() {
        let mut app = App::new(vec![]);
        app.items.push(Item::Reasoning {
            id: "r".into(),
            text: "planning the change".into(),
            start_ms: Some(1000),
            end_ms: Some(3400),
        });
        let rows = measure(&app, 80, theme());
        let text = rows
            .iter()
            .map(|r| r.line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Thinking: planning the change"));
        assert!(text.contains("2.4s"));
    }
}
