//! Rendering for the TUI: `render(frame, app)`.
//!
//! This is the only layer (besides the event loop) that knows about ratatui.
//! It is deliberately a pure function of `(&mut Frame, &App)` so it can be
//! exercised against ratatui's [`ratatui::backend::TestBackend`] without a real
//! terminal.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use super::app::{App, Item, ToolStatus};
use crate::tool::{summarize_input, tool_icon};

/// Fixed layout rows (top → bottom): title, history, tool status, agent tabs,
/// input, statusline.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    render_title(frame, app, chunks[0]);
    render_history(frame, app, chunks[1]);
    render_tool_status(frame, app, chunks[2]);
    render_agent_tabs(frame, app, chunks[3]);
    match &app.pending {
        Some(_) => render_prompt(frame, app, chunks[4]),
        None => render_input(frame, app, chunks[4]),
    }
    render_status(frame, app, chunks[5]);
}

fn render_title(frame: &mut Frame, app: &App, area: Rect) {
    let title = app.title.clone().unwrap_or_else(|| "Astra".to_string());
    let running = if app.running { " ●" } else { "" };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(Line::from(vec![
            Span::raw(" agent: "),
            Span::styled(app.current_agent(), Style::default().fg(Color::Cyan).bold()),
            Span::raw(running),
        ]));
    frame.render_widget(block, area);
}

fn render_history(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for item in &app.items {
        lines.extend(item_lines(item));
    }
    if !app.streaming.is_empty() {
        lines.extend(markdown_lines(&app.streaming));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No messages yet — type a prompt and press Enter.",
            Style::default().fg(Color::DarkGray).italic(),
        )));
    }

    let content_height = area.height as usize;
    let scroll = lines.len().saturating_sub(content_height) as u16;

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" session "))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

fn item_lines(item: &Item) -> Vec<Line<'static>> {
    match item {
        Item::User(text) => vec![Line::from(vec![
            Span::styled("❯ ", Style::default().fg(Color::Cyan).bold()),
            Span::raw(text.clone()),
        ])],
        Item::Assistant(text) => markdown_lines(text),
        Item::ToolCall { name, input } => {
            let args = summarize_input(input);
            vec![Line::from(vec![
                Span::styled(
                    format!(" {} ", tool_icon(name)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{name}{args}"),
                    Style::default().fg(Color::Yellow).italic(),
                ),
            ])]
        }
        Item::ToolResult {
            name,
            summary,
            ok,
            output,
        } => {
            let mark = if *ok { "✓" } else { "✗" };
            let color = if *ok { Color::Green } else { Color::Red };
            let mut lines = vec![Line::from(vec![
                Span::styled(format!("   {mark} {name}"), Style::default().fg(color)),
                Span::styled(
                    format!(" — {summary}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ])];
            let shown = if output.is_empty() {
                summary.as_str()
            } else {
                output.as_str()
            };
            for out_line in shown.lines().take(6) {
                lines.push(Line::from(Span::styled(
                    format!("     {out_line}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            lines
        }
        Item::Notice(text) => vec![Line::from(Span::styled(
            text.clone(),
            Style::default().fg(Color::Magenta),
        ))],
        Item::Reasoning(text) => {
            // Collapsed: a "Thinking" header with a short preview (the reference CLI collapses
            // reasoning; the body is only shown on expand).
            let preview: String = text.lines().next().unwrap_or("").chars().take(60).collect();
            let ellipsis = if text.chars().count() > 60 { "…" } else { "" };
            vec![Line::from(vec![
                Span::styled(
                    " ✦ Thinking · ",
                    Style::default().fg(Color::DarkGray).italic(),
                ),
                Span::styled(
                    format!("{preview}{ellipsis}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ])]
        }
        Item::Error(text) => vec![Line::from(Span::styled(
            format!(" ✗ {text}"),
            Style::default().fg(Color::Red),
        ))],
        Item::Done(result) => vec![Line::from(Span::styled(
            result.clone(),
            Style::default().fg(Color::Cyan),
        ))],
    }
}

/// Render markdown-ish assistant text into styled lines: fenced code blocks (```), headings
/// (#/##), bullets (- / *), inline code (`) and bold (**) get basic styling; everything else is
/// plain. This is a lightweight approximation of the reference CLI's markdown renderer (no
/// full syntax highlighter).
fn markdown_lines(text: &str) -> Vec<Line<'static>> {
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
                Style::default().fg(Color::DarkGray),
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
                Style::default().fg(Color::Cyan).bold(),
            )));
        } else if let Some(rest) = body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
            let mut spans = vec![Span::styled("  • ", Style::default().fg(Color::DarkGray))];
            spans.extend(inline_spans(rest));
            lines.push(Line::from(spans));
        } else {
            lines.push(Line::from(inline_spans(body)));
        }
    }
    if in_code {
        lines.append(&mut code_buf);
    }
    lines
}

/// Tokenize inline `**bold**` and `` `code` `` spans; other text stays plain.
fn inline_spans(text: &str) -> Vec<Span<'static>> {
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
                        Style::default().fg(Color::Green),
                    ));
                    rest = &after[end + 2..];
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

fn render_tool_status(frame: &mut Frame, app: &App, area: Rect) {
    let line = match &app.tool {
        ToolStatus::Idle => Line::from(Span::styled(" idle", Style::default().fg(Color::DarkGray))),
        ToolStatus::Running { name, .. } => Line::from(vec![
            Span::styled(
                format!(" {} ", app.spinner()),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("running {name}…"),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        ToolStatus::Done { name, ok } => {
            let (glyph, color) = if *ok {
                ("✓", Color::Green)
            } else {
                ("✗", Color::Red)
            };
            Line::from(vec![
                Span::styled(format!(" {glyph} "), Style::default().fg(color).bold()),
                Span::styled(name.clone(), Style::default().fg(color)),
            ])
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_agent_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<String> = app.agents.iter().map(|a| format!(" {a} ")).collect();
    let tabs = Tabs::new(titles)
        .select(app.agent_index)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .divider("│");
    frame.render_widget(tabs, area);
}

fn render_input(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" prompt ")
        .border_style(if app.running {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Cyan)
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let prefix = "❯ ";
    let text = format!("{prefix}{}", app.input);
    let paragraph = Paragraph::new(Line::from(Span::raw(text)))
        .wrap(Wrap { trim: false })
        .scroll((0, 0));
    frame.render_widget(paragraph, inner);

    // Place the terminal cursor after the typed text (clamped to the inner
    // area so a long input line does not panic).
    let cursor_x = inner.x + (prefix.chars().count() + app.cursor).min(inner.width as usize) as u16;
    frame.set_cursor_position((cursor_x, inner.y));
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = vec![Span::styled(
        app.current_agent(),
        Style::default().fg(Color::Cyan).bold(),
    )];
    if app.running {
        spans.push(Span::raw(format!("  {} ", app.spinner())));
    }
    if let Some((input, output, cost)) = app.last_usage {
        spans.push(Span::styled(
            format!("  ↑{input} ↓{output}"),
            Style::default().fg(Color::DarkGray),
        ));
        if let Some(cost) = cost {
            spans.push(Span::styled(
                format!("  ${cost:.4}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    let hint = if app.running {
        "esc interrupt"
    } else {
        "ctrl+c / q exit"
    };
    spans.push(Span::styled(
        format!("    {hint}"),
        Style::default().fg(Color::DarkGray).italic(),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_prompt(frame: &mut Frame, app: &App, area: Rect) {
    use super::app::Prompt;

    let (title, hint, show_input) = match &app.pending {
        Some(Prompt::Permission { tool, summary, .. }) => (
            format!("△ Permission required — {tool}: {summary}"),
            "[Enter]/[y] allow   [Esc]/[n] deny".to_string(),
            false,
        ),
        Some(Prompt::Question {
            question, options, ..
        }) => {
            let opts = if options.is_empty() {
                String::new()
            } else {
                format!("  [{}]", options.join(" / "))
            };
            (
                format!("? {question}{opts}"),
                "[Enter] answer   [Esc] dismiss".to_string(),
                true,
            )
        }
        None => return,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" confirm ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![Line::from(Span::styled(
        title,
        Style::default().fg(Color::Yellow).bold(),
    ))];
    if show_input {
        let text = format!("❯ {}", app.input);
        lines.push(Line::from(Span::raw(text)));
    }
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::DarkGray).italic(),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    if show_input {
        let prefix = "❯ ";
        let cursor_x =
            inner.x + (prefix.chars().count() + app.cursor).min(inner.width as usize) as u16;
        frame.set_cursor_position((cursor_x, inner.y + 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render_to_string(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, app))
            .expect("draw to test backend");
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push(
                    terminal.backend().buffer()[(x, y)]
                        .symbol()
                        .chars()
                        .next()
                        .unwrap_or(' '),
                );
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_empty_state() {
        let app = App::new(vec!["build".into(), "plan".into()]);
        let out = render_to_string(&app, 60, 20);
        assert!(out.contains("Astra"));
        assert!(out.contains("build"));
        assert!(out.contains("No messages yet"));
    }

    #[test]
    fn renders_streamed_text_and_tool_status() {
        let mut app = App::new(vec!["build".into()]);
        app.begin_turn("hello".into());
        app.apply_agent_event(&astra_proto::astra::engine::v1::AgentEvent {
            kind: Some(astra_proto::astra::engine::v1::agent_event::Kind::ToolCall(
                astra_proto::astra::engine::v1::ToolCallEvent {
                    id: "t".into(),
                    name: "bash".into(),
                    input: "{}".into(),
                },
            )),
        });
        let out = render_to_string(&app, 60, 20);
        assert!(out.contains("bash"));
        assert!(out.contains("❯ hello"));
    }

    #[test]
    fn renders_spinner_while_running() {
        let mut app = App::new(vec!["build".into()]);
        app.begin_turn("x".into());
        app.apply_agent_event(&astra_proto::astra::engine::v1::AgentEvent {
            kind: Some(astra_proto::astra::engine::v1::agent_event::Kind::ToolCall(
                astra_proto::astra::engine::v1::ToolCallEvent {
                    id: "t".into(),
                    name: "read".into(),
                    input: "{}".into(),
                },
            )),
        });
        let out = render_to_string(&app, 60, 20);
        assert!(out.contains("running read"));
        assert!(out.contains("⠋"));
    }

    #[test]
    fn markdown_renders_code_fences_headings_and_inline() {
        let text = "# Title\n\nsome `code` and **bold**\n\n```\nlet x = 1;\n```\n- item";
        let lines = markdown_lines(text);
        // headings are bold (Style carries the bold modifier, not asserted via string here);
        // assert the structural transforms: code fence content is present, bullet is bulleted.
        assert!(lines.iter().any(|l| l.to_string().contains("let x = 1;")));
        assert!(lines.iter().any(|l| l.to_string().contains("• item")));
        assert!(lines.iter().any(|l| l.to_string().contains("Title")));
        assert!(
            !lines.iter().any(|l| l.to_string().contains("```")),
            "fences are stripped"
        );
    }
}
