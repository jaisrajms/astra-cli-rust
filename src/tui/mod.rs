//! Default full-screen TUI (E-01).
//!
//! Layout of the module mirrors the reference implementation split: a **pure**
//! [`App`] state machine ([`app`]), a pure [`render`] function ([`render`]),
//! and a thin terminal + daemon event loop here ([`run`]). The state machine is
//! unit-testable without a terminal; `render` is exercisable against ratatui's
//! `TestBackend`; `run` is the only place that touches crossterm.
//!
//! `astra` with no arguments dispatches to [`run`] (see `main.rs`).

pub mod app;
pub mod render;
mod stream;
pub mod theme;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;

use astra_proto::astra::engine::v1::agent_service_client::AgentServiceClient;
use astra_proto::astra::engine::v1::{
    chat_client_msg, chat_event, ChatClientMsg, ListAgentsRequest, SendMessage,
};
use astra_proto::SessionId;

use self::app::{App, Prompt};
use self::stream::DaemonEvent;

/// Enter the full-screen TUI. Blocks until the user quits (Ctrl-C / `q` / Esc)
/// or the daemon stream closes.
pub async fn run(channel: Channel) -> anyhow::Result<()> {
    let mut terminal = ratatui::try_init().context("failed to initialize the terminal")?;
    let result = run_inner(&mut terminal, channel).await;
    ratatui::restore();
    result
}

async fn run_inner(
    terminal: &mut ratatui::DefaultTerminal,
    channel: Channel,
) -> anyhow::Result<()> {
    let agents = load_agents(&channel).await;
    let mut app = App::new(agents);
    // Fresh session: `None` until the daemon's `SessionStarted` event returns the real id.
    let session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Open the persistent bidi chat stream up front; a failure here surfaces as
    // a clean error before the UI paints anything.
    let (sink, mut event_rx) = stream::spawn(channel).await?;

    let mut key_events = crossterm::event::EventStream::new();
    let mut tick = interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            key_event = key_events.next() => {
                match key_event {
                    Some(Ok(event)) => handle_terminal_event(&mut app, event, &sink, &session_id).await?,
                    Some(Err(_)) => {}
                    None => break,
                }
            }
            chat_event = event_rx.recv() => {
                match chat_event {
                    Some(DaemonEvent::Event(event)) => {
                        // Capture the real session id the daemon assigns on first create.
                        if let Some(chat_event::Payload::SessionStarted(_)) = &event.payload {
                            if let Some(sid) = &event.session_id {
                                *session_id.lock().unwrap() = Some(sid.value.clone());
                            }
                        }
                        app.apply_chat_event(&event)
                    }
                    // The daemon closed the stream cleanly.
                    Some(DaemonEvent::Closed) | None => break,
                    // The daemon died mid-stream: surface it, then stop.
                    Some(DaemonEvent::Error(message)) => {
                        app.error = Some(message.clone());
                        app.items.push(app::Item::Error(message));
                        app.running = false;
                        break;
                    }
                }
            }
            _ = tick.tick() => {
                app.tick();
            }
        }

        terminal.draw(|frame| render::render(frame, &app))?;

        if app.quit {
            break;
        }
    }

    // Drop the outbound sink to close the daemon stream cleanly.
    drop(sink);
    Ok(())
}

/// Translate one terminal event into `App` mutations. Sending a submitted
/// message is the only daemon interaction here.
async fn handle_terminal_event(
    app: &mut App,
    event: Event,
    sink: &mpsc::Sender<ChatClientMsg>,
    session_id: &Arc<Mutex<Option<String>>>,
) -> anyhow::Result<()> {
    let Event::Key(key) = event else {
        return Ok(());
    };

    if key.kind == KeyEventKind::Release {
        return Ok(());
    }

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.request_quit();
        }
        // Command palette (leader key Ctrl-X).
        KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.toggle_palette();
        }
        KeyCode::Esc if app.palette.is_some() => app.palette = None,
        KeyCode::Enter if app.palette.is_some() => {
            let label = app.palette_selected();
            app.palette = None;
            match label {
                Some("quit") => app.request_quit(),
                Some("theme") => app.next_theme(),
                Some("agent") => app.next_agent(),
                Some("clear input") => {
                    app.input.clear();
                    app.cursor = 0;
                }
                _ => {}
            }
        }
        KeyCode::Up if app.palette.is_some() => app.palette_up(),
        KeyCode::Down if app.palette.is_some() => app.palette_down(),
        KeyCode::Esc => {
            let sid = session_id.lock().unwrap().clone();
            match app.pending.clone() {
                Some(Prompt::Permission { .. }) => {
                    if let Some(msg) = app.resolve_permission(false, sid) {
                        sink.send(msg).await.context("chat stream closed")?;
                    }
                }
                Some(Prompt::Question { .. }) => {
                    if let Some(msg) = app.resolve_question(String::new(), sid) {
                        sink.send(msg).await.context("chat stream closed")?;
                    }
                }
                None => app.request_quit(),
            }
        }
        KeyCode::Enter => {
            let sid = session_id.lock().unwrap().clone();
            match app.pending.clone() {
                Some(Prompt::Permission { .. }) => {
                    if let Some(msg) = app.resolve_permission(true, sid) {
                        sink.send(msg).await.context("chat stream closed")?;
                    }
                }
                Some(Prompt::Question { .. }) => {
                    let answer = app.input.trim().to_string();
                    app.input.clear();
                    app.cursor = 0;
                    if let Some(msg) = app.resolve_question(answer, sid) {
                        sink.send(msg).await.context("chat stream closed")?;
                    }
                }
                None => {
                    if let Some(content) = app.submit() {
                        // `!`-prefixed input runs a LOCAL shell command (not through the LLM).
                        if let Some(cmd) = content.strip_prefix('!').map(str::trim) {
                            if !cmd.is_empty() {
                                app.record_shell(cmd.to_string());
                                let out = tokio::task::spawn_blocking({
                                    let cmd = cmd.to_string();
                                    move || run_local_shell(&cmd)
                                })
                                .await
                                .unwrap_or_else(|_| ("(shell task failed)".to_string(), false));
                                app.record_shell_output(out.0, out.1);
                            }
                            return Ok(());
                        }
                        let agent = app.current_agent().to_string();
                        app.begin_turn(content.clone());
                        let sid = session_id.lock().unwrap().clone();
                        sink.send(build_send_message(sid, content, &agent))
                            .await
                            .context("chat stream closed")?;
                    }
                }
            }
        }
        KeyCode::Char('y') if matches!(app.pending, Some(Prompt::Permission { .. })) => {
            let sid = session_id.lock().unwrap().clone();
            if let Some(msg) = app.resolve_permission(true, sid) {
                sink.send(msg).await.context("chat stream closed")?;
            }
        }
        KeyCode::Char('n') if matches!(app.pending, Some(Prompt::Permission { .. })) => {
            let sid = session_id.lock().unwrap().clone();
            if let Some(msg) = app.resolve_permission(false, sid) {
                sink.send(msg).await.context("chat stream closed")?;
            }
        }
        // Match the reference CLI: `q` quits only when the prompt is empty.
        KeyCode::Char('q') if app.input.is_empty() && app.pending.is_none() => {
            app.request_quit();
        }
        KeyCode::Tab if app.pending.is_none() => app.next_agent(),
        KeyCode::BackTab if app.pending.is_none() => app.prev_agent(),
        KeyCode::Up if app.pending.is_none() => app.recall_older(),
        KeyCode::Down if app.pending.is_none() => app.recall_newer(),
        KeyCode::F(2) if app.pending.is_none() => app.next_theme(),
        KeyCode::Left => app.cursor_left(),
        KeyCode::Right => app.cursor_right(),
        KeyCode::Home => app.cursor_home(),
        KeyCode::End => app.cursor_end(),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => app.delete_forward(),
        KeyCode::Char(c) => app.push_char(c),
        _ => {}
    }

    Ok(())
}

fn build_send_message(session_id: Option<String>, content: String, agent: &str) -> ChatClientMsg {
    ChatClientMsg {
        session_id: session_id.map(|v| SessionId { value: v }),
        payload: Some(chat_client_msg::Payload::SendMessage(SendMessage {
            content,
            agent: agent.to_string(),
            model: None,
            images: Vec::new(),
            exec: None,
        })),
    }
}

/// Run a command in the local shell (`sh -c`), returning `(output, ok)`.
fn run_local_shell(command: &str) -> (String, bool) {
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
    {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            let text = if !stdout.trim().is_empty() {
                stdout.to_string()
            } else {
                stderr.to_string()
            };
            (text, o.status.success())
        }
        Err(e) => (format!("failed to run: {e}"), false),
    }
}

/// List the agents available to drive a session. Falls back to the built-in set
/// when the daemon is unreachable or returns none.
async fn load_agents(channel: &Channel) -> Vec<String> {
    let mut client = AgentServiceClient::new(channel.clone());
    let Ok(resp) = client
        .list_agents(with_workspace(Request::new(ListAgentsRequest {})))
        .await
    else {
        return app::DEFAULT_AGENTS.iter().map(|s| s.to_string()).collect();
    };

    let mut agents: Vec<String> = resp
        .into_inner()
        .agents
        .into_iter()
        .filter(|a| a.hidden != Some(true))
        .map(|a| a.name)
        .collect();
    agents.sort();
    agents.dedup();

    if agents.is_empty() {
        app::DEFAULT_AGENTS.iter().map(|s| s.to_string()).collect()
    } else {
        agents
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_send_message_carries_content_and_agent() {
        let msg = build_send_message(Some("s-1".into()), "hello".into(), "plan");
        assert_eq!(msg.session_id.as_ref().unwrap().value, "s-1");
        match msg.payload {
            Some(chat_client_msg::Payload::SendMessage(sm)) => {
                assert_eq!(sm.content, "hello");
                assert_eq!(sm.agent, "plan");
                assert!(sm.exec.is_none());
            }
            other => panic!("expected SendMessage payload, got {other:?}"),
        }
    }

    #[test]
    fn enter_submits_and_starts_a_turn() {
        let (sink, mut rx) = mpsc::channel::<ChatClientMsg>(4);
        let mut app = App::new(vec!["build".into(), "plan".into()]);
        app.push_char('h');
        app.push_char('i');

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            handle_terminal_event(
                &mut app,
                Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )),
                &sink,
                &Arc::new(Mutex::new(Some("s-9".to_string()))),
            )
            .await
            .unwrap();
        });

        assert!(app.running);
        assert_eq!(app.items[0], app::Item::User("hi".into()));
        let msg = rt.block_on(async { rx.recv().await }).unwrap();
        match msg.payload {
            Some(chat_client_msg::Payload::SendMessage(sm)) => {
                assert_eq!(sm.content, "hi");
                assert_eq!(sm.agent, "build");
            }
            other => panic!("expected SendMessage payload, got {other:?}"),
        }
    }

    #[test]
    fn ctrl_c_and_empty_q_quit() {
        let (sink, _rx) = mpsc::channel::<ChatClientMsg>(4);
        let mut app = App::new(vec!["build".into()]);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            handle_terminal_event(
                &mut app,
                Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                )),
                &sink,
                &Arc::new(Mutex::new(None)),
            )
            .await
            .unwrap();
        });
        assert!(app.quit);
        app.quit = false;

        rt.block_on(async {
            handle_terminal_event(
                &mut app,
                Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('q'),
                    KeyModifiers::NONE,
                )),
                &sink,
                &Arc::new(Mutex::new(None)),
            )
            .await
            .unwrap();
        });
        assert!(app.quit);
    }

    #[test]
    fn q_with_nonempty_input_does_not_quit() {
        let (sink, _rx) = mpsc::channel::<ChatClientMsg>(4);
        let mut app = App::new(vec!["build".into()]);
        app.push_char('q');

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            handle_terminal_event(
                &mut app,
                Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('q'),
                    KeyModifiers::NONE,
                )),
                &sink,
                &Arc::new(Mutex::new(None)),
            )
            .await
            .unwrap();
        });
        assert!(!app.quit);
        // `q` does not quit when the prompt is non-empty; it is typed normally.
        assert_eq!(app.input, "qq");
    }
}
