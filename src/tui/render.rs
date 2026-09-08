//! Rendering for the TUI: `render(frame, app)`.
//!
//! This is the only layer (besides the event loop) that knows about ratatui.
//! It is deliberately a pure function of `(&mut Frame, &App)` so it can be
//! exercised against ratatui's [`ratatui::backend::TestBackend`] without a real
//! terminal.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use super::app::{App, Item, ToolStatus};

/// Fixed layout rows (top → bottom): title, history, tool status, agent tabs,
/// input, help.
const HELP: &str = "[Tab/Shift-Tab] agent   [Enter] send   [Ctrl-C / q] quit";

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
    render_input(frame, app, chunks[4]);
    render_help(frame, chunks[5]);
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
        lines.push(Line::from(Span::raw(app.streaming.as_str())));
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
        Item::Assistant(text) => vec![Line::from(Span::raw(text.clone()))],
        Item::ToolCall { name } => vec![Line::from(vec![
            Span::styled(" ⚙ ", Style::default().fg(Color::Yellow)),
            Span::styled(name.clone(), Style::default().fg(Color::Yellow).italic()),
        ])],
        Item::ToolResult { name, summary, ok } => {
            let mark = if *ok { "✓" } else { "✗" };
            let color = if *ok { Color::Green } else { Color::Red };
            vec![Line::from(vec![
                Span::styled(format!("   {mark} {name}"), Style::default().fg(color)),
                Span::styled(
                    format!(" — {summary}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ])]
        }
        Item::Usage {
            input,
            output,
            cost,
        } => {
            let cost = cost
                .map(|c| format!("${c:.4}"))
                .unwrap_or_else(|| "?".to_string());
            vec![Line::from(Span::styled(
                format!("   ↑{input} ↓{output} tokens · {cost}"),
                Style::default().fg(Color::DarkGray),
            ))]
        }
        Item::Notice(text) => vec![Line::from(Span::styled(
            text.clone(),
            Style::default().fg(Color::Magenta),
        ))],
        Item::Reasoning(text) => vec![Line::from(Span::styled(
            format!(" … {text}"),
            Style::default().fg(Color::DarkGray).italic(),
        ))],
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

fn render_help(frame: &mut Frame, area: Rect) {
    let line = Line::from(Span::styled(HELP, Style::default().fg(Color::DarkGray)));
    let paragraph = Paragraph::new(line).alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
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
}
