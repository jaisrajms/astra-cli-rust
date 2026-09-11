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
use super::theme::{theme_at, Theme};
use crate::tool::{summarize_input, tool_icon};

/// Fixed layout rows (top → bottom): title, history, tool status, agent tabs,
/// input, statusline.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let theme = *theme_at(app.theme_index);

    // A right-hand sidebar when the terminal is wide enough (the reference CLI shows it above
    // ~120 cols; we use a lower threshold for smaller terminals).
    let (main, sidebar) = if area.width >= 100 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(60), Constraint::Length(22)])
            .split(area);
        (cols[0], Some(cols[1]))
    } else {
        (area, None)
    };

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
        .split(main);

    render_title(frame, app, chunks[0], theme);
    render_history(frame, app, chunks[1], theme);
    render_tool_status(frame, app, chunks[2], theme);
    render_agent_tabs(frame, app, chunks[3], theme);
    match &app.pending {
        Some(_) => render_prompt(frame, app, chunks[4], theme),
        None => render_input(frame, app, chunks[4], theme),
    }
    render_status(frame, app, chunks[5], theme);

    if let Some(sidebar) = sidebar {
        render_sidebar(frame, app, sidebar, theme);
    }
    if app.palette.is_some() {
        render_palette(frame, app, area, theme);
    }
    if app.mention.is_some() {
        render_mention(frame, app, area, theme);
    }
}

fn render_title(frame: &mut Frame, app: &App, area: Rect, theme: Theme) {
    let title = app.title.clone().unwrap_or_else(|| "Astra".to_string());
    let running = if app.running { " ●" } else { "" };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(Line::from(vec![
            Span::raw(" agent: "),
            Span::styled(
                app.current_agent(),
                Style::default().fg(theme.accent).bold(),
            ),
            Span::raw(running),
        ]));
    frame.render_widget(block, area);
}

fn render_history(frame: &mut Frame, app: &App, area: Rect, theme: Theme) {
    let mut lines: Vec<Line> = Vec::new();
    for item in &app.items {
        lines.extend(item_lines(item, theme));
    }
    if !app.streaming.is_empty() {
        lines.extend(markdown_lines(&app.streaming, theme));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No messages yet — type a prompt and press Enter.",
            Style::default().fg(theme.dim).italic(),
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

fn item_lines(item: &Item, theme: Theme) -> Vec<Line<'static>> {
    match item {
        Item::User(text) => vec![Line::from(vec![
            Span::styled("❯ ", Style::default().fg(theme.accent).bold()),
            Span::raw(text.clone()),
        ])],
        Item::Assistant(text) => markdown_lines(text, theme),
        Item::ToolCall { name, input } => {
            let args = summarize_input(input);
            vec![Line::from(vec![
                Span::styled(
                    format!(" {} ", tool_icon(name)),
                    Style::default().fg(theme.tool),
                ),
                Span::styled(
                    format!("{name}{args}"),
                    Style::default().fg(theme.tool).italic(),
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
            let color = if *ok { theme.ok } else { theme.err };
            let mut lines = vec![Line::from(vec![
                Span::styled(format!("   {mark} {name}"), Style::default().fg(color)),
                Span::styled(format!(" — {summary}"), Style::default().fg(theme.dim)),
            ])];
            let shown = if output.is_empty() {
                summary.as_str()
            } else {
                output.as_str()
            };
            for out_line in shown.lines().take(6) {
                lines.push(Line::from(Span::styled(
                    format!("     {out_line}"),
                    Style::default().fg(theme.dim),
                )));
            }
            lines
        }
        Item::Notice(text) => vec![Line::from(Span::styled(
            text.clone(),
            Style::default().fg(theme.notice),
        ))],
        Item::Reasoning(text) => {
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

/// Render markdown-ish assistant text into styled lines: fenced code blocks (```), headings
/// (#/##), bullets (- / *), inline code (`) and bold (**) get basic styling; everything else is
/// plain. This is a lightweight approximation of the reference CLI's markdown renderer (no
/// full syntax highlighter).
fn markdown_lines(text: &str, theme: Theme) -> Vec<Line<'static>> {
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
fn inline_spans(text: &str, theme: Theme) -> Vec<Span<'static>> {
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

fn render_tool_status(frame: &mut Frame, app: &App, area: Rect, theme: Theme) {
    let line = match &app.tool {
        ToolStatus::Idle => Line::from(Span::styled(" idle", Style::default().fg(theme.dim))),
        ToolStatus::Running { name, .. } => Line::from(vec![
            Span::styled(
                format!(" {} ", app.spinner()),
                Style::default().fg(theme.tool),
            ),
            Span::styled(format!("running {name}…"), Style::default().fg(theme.tool)),
        ]),
        ToolStatus::Done { name, ok } => {
            let (glyph, color) = if *ok {
                ("✓", theme.ok)
            } else {
                ("✗", theme.err)
            };
            Line::from(vec![
                Span::styled(format!(" {glyph} "), Style::default().fg(color).bold()),
                Span::styled(name.clone(), Style::default().fg(color)),
            ])
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_agent_tabs(frame: &mut Frame, app: &App, area: Rect, theme: Theme) {
    let titles: Vec<String> = app.agents.iter().map(|a| format!(" {a} ")).collect();
    let tabs = Tabs::new(titles)
        .select(app.agent_index)
        .style(Style::default().fg(theme.dim))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .divider("│");
    frame.render_widget(tabs, area);
}

fn render_input(frame: &mut Frame, app: &App, area: Rect, theme: Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" prompt ")
        .border_style(if app.running {
            Style::default().fg(theme.tool)
        } else {
            Style::default().fg(theme.accent)
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

fn render_status(frame: &mut Frame, app: &App, area: Rect, theme: Theme) {
    let mut spans: Vec<Span> = vec![Span::styled(
        app.current_agent(),
        Style::default().fg(theme.accent).bold(),
    )];
    spans.push(Span::styled(
        format!("  {}", theme.name),
        Style::default().fg(theme.dim),
    ));
    if app.running {
        spans.push(Span::raw(format!("  {} ", app.spinner())));
    }
    if let Some((input, output, cost)) = app.last_usage {
        spans.push(Span::styled(
            format!("  ↑{input} ↓{output}"),
            Style::default().fg(theme.dim),
        ));
        if let Some(cost) = cost {
            spans.push(Span::styled(
                format!("  ${cost:.4}"),
                Style::default().fg(theme.dim),
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
        Style::default().fg(theme.dim).italic(),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_prompt(frame: &mut Frame, app: &App, area: Rect, theme: Theme) {
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
        .border_style(Style::default().fg(theme.tool));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![Line::from(Span::styled(
        title,
        Style::default().fg(theme.tool).bold(),
    ))];
    if show_input {
        let text = format!("❯ {}", app.input);
        lines.push(Line::from(Span::raw(text)));
    }
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(theme.dim).italic(),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    if show_input {
        let prefix = "❯ ";
        let cursor_x =
            inner.x + (prefix.chars().count() + app.cursor).min(inner.width as usize) as u16;
        frame.set_cursor_position((cursor_x, inner.y + 1));
    }
}

fn render_sidebar(frame: &mut Frame, app: &App, area: Rect, theme: Theme) {
    let title = app.title.clone().unwrap_or_else(|| "Astra".to_string());
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            title,
            Style::default().fg(theme.accent).bold(),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("agent  ", Style::default().fg(theme.dim)),
            Span::styled(app.current_agent(), Style::default().fg(theme.accent)),
        ]),
        Line::from(vec![
            Span::styled("theme  ", Style::default().fg(theme.dim)),
            Span::raw(theme.name),
        ]),
    ];
    if let Some((input, output, cost)) = app.last_usage {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "usage",
            Style::default().fg(theme.dim),
        )));
        lines.push(Line::from(vec![
            Span::styled("  ↑ ", Style::default().fg(theme.dim)),
            Span::raw(input.to_string()),
            Span::styled("  ↓ ", Style::default().fg(theme.dim)),
            Span::raw(output.to_string()),
        ]));
        if let Some(cost) = cost {
            lines.push(Line::from(vec![
                Span::styled("  $ ", Style::default().fg(theme.dim)),
                Span::raw(format!("{cost:.4}")),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Astra",
        Style::default().fg(theme.dim),
    )));

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::LEFT).title(" session "))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_palette(frame: &mut Frame, app: &App, area: Rect, theme: Theme) {
    use super::app::PALETTE_COMMANDS;

    let width = 26_u16;
    let height = PALETTE_COMMANDS.len() as u16 + 2;
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let palette_area = Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    };

    let items: Vec<Line> = PALETTE_COMMANDS
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if Some(i) == app.palette {
                Line::from(Span::styled(
                    format!("▌ {c}"),
                    Style::default().fg(theme.accent).bold(),
                ))
            } else {
                Line::from(Span::raw(format!("  {c}")))
            }
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" commands ")
        .border_style(Style::default().fg(theme.accent));
    frame.render_widget(Paragraph::new(items).block(block), palette_area);
}

fn render_mention(frame: &mut Frame, app: &App, area: Rect, theme: Theme) {
    use super::app::MentionItem;

    let Some(m) = &app.mention else {
        return;
    };
    if m.items.is_empty() {
        return;
    }

    let max_show = 8usize;
    let visible = &m.items[..m.items.len().min(max_show)];
    let width = 44_u16;
    let height = visible.len() as u16 + 2;
    let x = area.x + 2;
    let y = area.y + area.height.saturating_sub(height + 5);
    let popup = Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    };

    let items: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let (kind, label) = match item {
                MentionItem::Agent(n) => ("@", n.as_str()),
                MentionItem::File(p) => ("#", p.as_str()),
            };
            let line = format!("{kind} {label}");
            if i == m.selection {
                Line::from(Span::styled(line, Style::default().fg(theme.accent).bold()))
            } else {
                Line::from(Span::raw(line))
            }
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" mentions ")
        .border_style(Style::default().fg(theme.accent));
    frame.render_widget(Paragraph::new(items).block(block), popup);
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
        let lines = markdown_lines(text, *theme_at(0));
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
