//! Default full-screen TUI (opencode parity, E-01).
//!
//! Layout of the module mirrors the opencode ground truth split: a **pure**
//! [`App`] state machine ([`app`]), a pure [`render`] function ([`render`]),
//! and a thin terminal + daemon event loop here ([`run`]). The state machine is
//! unit-testable without a terminal; `render` is exercisable against ratatui's
//! `TestBackend`; `run` is the only place that touches crossterm.
//!
//! `astra` with no arguments dispatches to [`run`] (see `main.rs`).

pub mod app;
pub mod render;
mod stream;

use std::time::Duration;

use anyhow::Context;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};
use tonic::transport::Channel;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;

use astra_proto::astra::engine::v1::agent_service_client::AgentServiceClient;
use astra_proto::astra::engine::v1::{
    chat_client_msg, ChatClientMsg, ListAgentsRequest, SendMessage,
};
use astra_proto::SessionId;

use self::app::App;
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
    let session_id = crate::ids::fresh_session_id();

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
                    Some(DaemonEvent::Event(event)) => app.apply_chat_event(&event),
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
    session_id: &str,
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
        // Match opencode: `q` quits only when the prompt is empty.
        KeyCode::Char('q') if app.input.is_empty() => {
            app.request_quit();
        }
        KeyCode::Esc => app.request_quit(),
        KeyCode::Enter => {
            if let Some(content) = app.submit() {
                let agent = app.current_agent().to_string();
                app.begin_turn(content.clone());
                sink.send(build_send_message(session_id, content, &agent))
                    .await
                    .context("chat stream closed")?;
            }
        }
        KeyCode::Tab => app.next_agent(),
        KeyCode::BackTab => app.prev_agent(),
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

fn build_send_message(session_id: &str, content: String, agent: &str) -> ChatClientMsg {
    ChatClientMsg {
        session_id: Some(SessionId {
            value: session_id.to_string(),
        }),
        payload: Some(chat_client_msg::Payload::SendMessage(SendMessage {
            content,
            agent: agent.to_string(),
            model: None,
            images: Vec::new(),
            exec: None,
        })),
    }
}

/// List the agents available to drive a session. Falls back to the built-in set
/// when the daemon is unreachable or returns none.
async fn load_agents(channel: &Channel) -> Vec<String> {
    let mut client = AgentServiceClient::new(channel.clone());
    let Ok(resp) = client.list_agents(ListAgentsRequest {}).await else {
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
        let msg = build_send_message("s-1", "hello".into(), "plan");
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
                "s-9",
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
                "s",
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
                "s",
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
                "s",
            )
            .await
            .unwrap();
        });
        assert!(!app.quit);
        // `q` does not quit when the prompt is non-empty; it is typed normally.
        assert_eq!(app.input, "qq");
    }
}
