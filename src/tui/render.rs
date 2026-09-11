//! Rendering for the TUI: `render(frame, app)`.
//!
//! This is the only layer (besides the event loop) that knows about ratatui. It consumes the
//! ratatui-free [`super::layout::Layout`] and the pre-measured rows from [`super::transcript`], so
//! it never re-derives scroll geometry and the paint pass agrees with the scroll math.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, Prompt, SidebarMode, ToolStatus};
use super::layout::{Layout, SIDEBAR_WIDTH};
use super::theme::{theme_at, Theme};
use super::transcript;

/// A ratatui `Rect` from four coordinates.
fn rr(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(x, y, width, height)
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let theme = *theme_at(app.theme_index);

    // A diff-review prompt needs room to show the proposed diff; otherwise the prompt row is short.
    let prompt_rows = match &app.pending {
        Some(Prompt::DiffReview { .. }) => 12,
        _ => 3,
    };
    let layout = Layout::compute(area.width, area.height, prompt_rows);
    app.ui.layout = Some(layout);

    let main_width = layout.transcript.width;
    let title = rr(0, 0, main_width, 3);
    let transcript = rr(
        layout.transcript.x,
        layout.transcript.y,
        layout.transcript.width,
        layout.transcript.height,
    );
    let tool_status = rr(
        0,
        layout.transcript.y + layout.transcript.height,
        main_width,
        1,
    );
    let prompt = rr(
        layout.prompt.x,
        layout.prompt.y,
        layout.prompt.width,
        layout.prompt.height,
    );
    let footer = rr(
        layout.footer.x,
        layout.footer.y,
        layout.footer.width,
        layout.footer.height,
    );

    render_title(frame, app, title, theme);
    render_history(frame, app, transcript, theme);
    render_tool_status(frame, app, tool_status, theme);
    match &app.pending {
        Some(_) => render_prompt(frame, app, prompt, theme),
        None => render_input(frame, app, prompt, theme),
    }
    render_status(frame, app, footer, theme);

    if let Some(sidebar) = layout.sidebar.or_else(|| {
        // Force-dock the sidebar as a right-edge overlay on a narrow terminal.
        if app.ui.sidebar == SidebarMode::Docked {
            Some(super::layout::Rect {
                x: area.width.saturating_sub(SIDEBAR_WIDTH),
                y: 0,
                width: SIDEBAR_WIDTH,
                height: area.height,
            })
        } else {
            None
        }
    }) {
        render_sidebar(
            frame,
            app,
            rr(sidebar.x, sidebar.y, sidebar.width, sidebar.height),
            theme,
        );
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

fn render_history(frame: &mut Frame, app: &mut App, area: Rect, theme: Theme) {
    // Measure at the inner width/height (the block border takes one column/row on each side).
    let inner_width = area.width.saturating_sub(2);
    let inner_height = area.height.saturating_sub(2);
    let rows = transcript::measure(app, inner_width, theme);

    let total = rows.len();
    let viewport = inner_height as usize;
    app.ui.transcript.total = total;
    app.ui.transcript.viewport = viewport;
    app.ui.transcript.on_content_changed(total, viewport);

    let offset = app.ui.transcript.offset as usize;
    let visible: Vec<Line> = rows
        .iter()
        .skip(offset)
        .take(viewport)
        .map(|r| r.line.clone())
        .collect();
    app.ui.visible_item_ids = rows
        .iter()
        .skip(offset)
        .take(viewport)
        .map(|r| r.item_id.clone())
        .collect();

    let paragraph = Paragraph::new(visible)
        .block(Block::default().borders(Borders::ALL).title(" session "))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
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
    if let Some(model) = &app.ui.model {
        spans.push(Span::styled(
            format!(" · {model}"),
            Style::default().fg(theme.dim),
        ));
    }
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
    } else if app.esc_armed {
        "esc again to quit"
    } else {
        "ctrl+c exit"
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
        Some(Prompt::DiffReview {
            tool_name,
            file_path,
            ..
        }) => (
            format!("← {tool_name} {file_path}"),
            "[Enter] accept   [e] edit   [Esc] reject".to_string(),
            false,
        ),
        None => return,
    };

    let is_diff = matches!(app.pending, Some(Prompt::DiffReview { .. }));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" review ")
        .border_style(Style::default().fg(theme.tool));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![Line::from(Span::styled(
        title,
        Style::default().fg(theme.tool).bold(),
    ))];

    if is_diff {
        // The proposed unified diff, `-` lines in error color, `+` lines in ok color.
        if let Some(Prompt::DiffReview { unified_diff, .. }) = &app.pending {
            for raw in unified_diff
                .lines()
                .take(inner.height.saturating_sub(3) as usize)
            {
                let color = if raw.starts_with('-') {
                    theme.err
                } else if raw.starts_with('+') {
                    theme.ok
                } else {
                    theme.dim
                };
                lines.push(Line::from(Span::styled(
                    format!("  {raw}"),
                    Style::default().fg(color),
                )));
            }
        }
    }

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

fn render_sidebar(frame: &mut Frame, app: &mut App, area: Rect, theme: Theme) {
    let title = app.title.clone().unwrap_or_else(|| "Astra".to_string());
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            title,
            Style::default().fg(theme.accent).bold(),
        )),
        Line::from(""),
    ];

    // Context (the reference CLI's sidebar-context feature): model, tokens, % used, spend.
    if app.last_usage.is_some() || app.ui.model.is_some() {
        lines.push(Line::from(Span::styled("Context", Style::default().bold())));
        if let Some(model) = &app.ui.model {
            lines.push(Line::from(Span::styled(
                model.clone(),
                Style::default().fg(theme.dim),
            )));
        }
        if let Some((input, output, cost)) = app.last_usage {
            let tokens = input + output;
            lines.push(Line::from(Span::styled(
                format!("{tokens} tokens"),
                Style::default().fg(theme.dim),
            )));
            if let Some(limit) = app.ui.context_limit {
                let pct = if limit > 0 {
                    (tokens as f64 / limit as f64 * 100.0) as u64
                } else {
                    0
                };
                lines.push(Line::from(Span::styled(
                    format!("{pct}% used"),
                    Style::default().fg(theme.dim),
                )));
            }
            if let Some(cost) = cost {
                lines.push(Line::from(Span::styled(
                    format!("${cost:.4} spent"),
                    Style::default().fg(theme.dim),
                )));
            }
        }
        lines.push(Line::from(""));
    }

    // LSP: the tool is an injected seam, so no server is attached.
    lines.push(Line::from(Span::styled("LSP", Style::default().bold())));
    lines.push(Line::from(Span::styled(
        if app.ui.lsp_enabled {
            "connected"
        } else {
            "LSPs are disabled"
        },
        Style::default().fg(theme.dim),
    )));

    // MCP servers (from the ConnectionStatusEvent snapshot).
    if !app.ui.mcp.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("MCP", Style::default().bold())));
        for (name, status) in &app.ui.mcp {
            let dot = if status == "connected" {
                theme.ok
            } else {
                theme.err
            };
            lines.push(Line::from(vec![
                Span::styled("• ", Style::default().fg(dot)),
                Span::styled(format!("{name} {status}"), Style::default().fg(theme.dim)),
            ]));
        }
    }

    // Todo (the reference CLI's sidebar-todo feature): the current session task list.
    if !app.todos.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Todo", Style::default().bold())));
        for todo in &app.todos {
            let (mark, color) = match todo.status.as_str() {
                "completed" => ("✓", theme.dim),
                "in_progress" => ("•", theme.tool),
                _ => (" ", theme.dim),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("[{mark}] "), Style::default().fg(color)),
                Span::styled(todo.content.clone(), Style::default().fg(theme.dim)),
            ]));
        }
    }

    // Measure + scroll the sidebar independently of the transcript.
    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    let wrapped: Vec<Line> = lines
        .into_iter()
        .flat_map(|l| transcript::wrap_line(l, inner_width))
        .collect();
    app.ui.sidebar_total = wrapped.len();
    app.ui.sidebar_viewport = inner_height;
    app.ui.clamp_sidebar();
    let offset = app.ui.sidebar_offset as usize;
    let visible: Vec<Line> = wrapped
        .into_iter()
        .skip(offset)
        .take(inner_height)
        .collect();

    let paragraph = Paragraph::new(visible)
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

    fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
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
        let mut app = App::new(vec!["build".into(), "plan".into()]);
        let out = render_to_string(&mut app, 60, 20);
        assert!(out.contains("Astra"));
        assert!(out.contains("build"));
        assert!(out.contains("No messages yet"));
    }

    #[test]
    fn sidebar_docks_on_wide_and_hides_on_narrow() {
        let mut app = App::new(vec!["build".into()]);
        // Wide (>=120): the sidebar (with its LSP section) is docked on the right.
        let wide = render_to_string(&mut app, 130, 30);
        assert!(wide.contains("LSP"), "wide layout should show the sidebar");

        // Narrow (<120): the sidebar is hidden.
        let narrow = render_to_string(&mut app, 80, 30);
        assert!(
            !narrow.contains("LSP"),
            "narrow layout should hide the sidebar"
        );

        // Force-docking the sidebar on narrow shows it again.
        app.ui.sidebar = SidebarMode::Docked;
        let forced = render_to_string(&mut app, 80, 30);
        assert!(
            forced.contains("LSP"),
            "force-docked sidebar should show on narrow"
        );
    }

    #[test]
    fn tool_line_renders_icon_and_label_not_raw_args() {
        let mut app = App::new(vec!["build".into()]);
        app.begin_turn("read a file".into());
        app.apply_agent_event(&astra_proto::astra::engine::v1::AgentEvent {
            kind: Some(astra_proto::astra::engine::v1::agent_event::Kind::ToolCall(
                astra_proto::astra::engine::v1::ToolCallEvent {
                    id: "t".into(),
                    name: "Read".into(),
                    input: r#"{"file_path":"src/main.rs"}"#.into(),
                },
            )),
        });
        app.apply_agent_event(&astra_proto::astra::engine::v1::AgentEvent {
            kind: Some(
                astra_proto::astra::engine::v1::agent_event::Kind::ToolResult(
                    astra_proto::astra::engine::v1::ToolResultEvent {
                        id: "t".into(),
                        name: "Read".into(),
                        summary: "done".into(),
                        output: "".into(),
                        is_error: false,
                        truncated: false,
                        full_output: None,
                    },
                ),
            ),
        });
        let out = render_to_string(&mut app, 60, 20);
        assert!(out.contains("Read src/main.rs"), "output: {out}");
        assert!(
            !out.contains("file_path"),
            "raw input keys must not leak into the tool line: {out}"
        );
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
        let out = render_to_string(&mut app, 60, 20);
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
        let out = render_to_string(&mut app, 60, 20);
        assert!(out.contains("running read"));
        assert!(out.contains("⠋"));
    }

    #[test]
    fn markdown_renders_code_fences_headings_and_inline() {
        let text = "# Title\n\nsome `code` and **bold**\n\n```\nlet x = 1;\n```\n- item";
        let lines = transcript::markdown_lines(text, *theme_at(0));
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

    #[test]
    fn inline_spans_never_panics_on_byte_boundaries() {
        let theme = *theme_at(0);
        // The closing-backtick-at-end case that used to over-slice by one byte.
        for text in [
            "`code`",
            "x `code` y",
            "`code`",
            "a`b`c`d`e",
            "é`code`",
            "`co`dé**bold**",
            "**bold**",
            "**bold**é",
            "`unclosed",
            "**unclosed",
            "`é`",
        ] {
            let _ = transcript::inline_spans(text, theme);
            let _ = transcript::markdown_lines(text, theme);
        }
    }
}
