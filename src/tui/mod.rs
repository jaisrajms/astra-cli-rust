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
pub mod event;
pub mod layout;
pub mod render;
mod stream;
pub mod theme;
pub mod tool_output;
pub mod transcript;
pub mod wrap;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;

use futures::StreamExt;

use astra_proto::astra::engine::v1::agent_service_client::AgentServiceClient;
use astra_proto::astra::engine::v1::{
    chat_client_msg, chat_event, ChatClientMsg, ListAgentsRequest, SendMessage,
};
use astra_proto::SessionId;

use self::app::{App, Prompt};
use self::event::{route, UiCommand};
use self::stream::DaemonEvent;

/// An RAII guard that enables crossterm mouse capture on construction and disables it on drop, so a
/// wheel event never falls through to the terminal emulator's scrollback (Plan §6.1).
struct MouseCaptureGuard;

impl MouseCaptureGuard {
    fn enable() -> anyhow::Result<Self> {
        crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for MouseCaptureGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    }
}

/// Enter the full-screen TUI. Blocks until the user quits (Ctrl-C / `q` / Esc)
/// or the daemon stream closes.
pub async fn run(channel: Channel) -> anyhow::Result<()> {
    let mut terminal = ratatui::try_init().context("failed to initialize the terminal")?;
    let capture = match MouseCaptureGuard::enable() {
        Ok(capture) => capture,
        Err(e) => {
            // Restore the terminal (already in raw mode) before failing out.
            ratatui::restore();
            return Err(e).context("failed to enable mouse capture");
        }
    };
    let result = run_inner(&mut terminal, channel).await;
    drop(capture);
    ratatui::restore();
    result
}

async fn run_inner(
    terminal: &mut ratatui::DefaultTerminal,
    channel: Channel,
) -> anyhow::Result<()> {
    let agents = load_agents(&channel).await;
    let mut app = App::new(agents);
    // Pre-scan the workspace for @-file mentions (bounded; run once at startup).
    if let Ok(cwd) = std::env::current_dir() {
        app.set_files(scan_workspace_files(&cwd, 300));
    }
    // Fresh session: `None` until the daemon's `SessionStarted` event returns the real id.
    let session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Open the persistent bidi chat stream up front; a failure here surfaces as
    // a clean error before the UI paints anything.
    let (sink, mut event_rx) = stream::spawn(channel).await?;

    let mut term_events = crossterm::event::EventStream::new();
    let mut tick = interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            term_event = term_events.next() => {
                match term_event {
                    Some(Ok(event)) => {
                        let command = route(&event, &app);
                        apply_command(&mut app, command, &sink, &session_id).await?;
                    }
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
                        app.apply_chat_event(&event);
                        app.sync_focus();
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

        terminal.draw(|frame| render::render(frame, &mut app))?;

        if app.quit {
            break;
        }
    }

    // Drop the outbound sink to close the daemon stream cleanly.
    drop(sink);
    Ok(())
}

/// Apply one routed [`UiCommand`]: mutate `App` and/or send a daemon message. This is the
/// side-effectful half of the input path (event routing is the pure [`route`] function).
async fn apply_command(
    app: &mut App,
    command: UiCommand,
    sink: &mpsc::Sender<ChatClientMsg>,
    session_id: &Arc<Mutex<Option<String>>>,
) -> anyhow::Result<()> {
    match command {
        UiCommand::Noop => {}
        UiCommand::ScrollTranscript(delta) => {
            let total = app.ui.transcript.total;
            let viewport = app.ui.transcript.viewport;
            app.ui.transcript.scroll_by(delta, total, viewport);
        }
        UiCommand::ScrollSidebar(delta) => {
            // TODO(Phase 5): scroll against the measured sidebar height.
            let next = app.ui.sidebar_offset as i32 + delta;
            app.ui.sidebar_offset = next.clamp(0, u16::MAX as i32) as u16;
        }
        UiCommand::TranscriptStart => {
            let (total, viewport) = (app.ui.transcript.total, app.ui.transcript.viewport);
            app.ui.transcript.scroll_to_start(total, viewport);
        }
        UiCommand::TranscriptEnd => {
            let (total, viewport) = (app.ui.transcript.total, app.ui.transcript.viewport);
            app.ui.transcript.scroll_to_end(total, viewport);
        }
        UiCommand::Focus(focus) => app.ui.focus = focus,
        UiCommand::ToggleExpand(item) => app.toggle_expanded(&item),
        UiCommand::Insert(c) => app.push_char(c),
        UiCommand::Backspace => app.backspace(),
        UiCommand::DeleteForward => app.delete_forward(),
        UiCommand::CursorLeft => app.cursor_left(),
        UiCommand::CursorRight => app.cursor_right(),
        UiCommand::CursorHome => app.cursor_home(),
        UiCommand::CursorEnd => app.cursor_end(),
        UiCommand::RecallOlder => app.recall_older(),
        UiCommand::RecallNewer => app.recall_newer(),
        UiCommand::NextAgent => app.next_agent(),
        UiCommand::PrevAgent => app.prev_agent(),
        UiCommand::NextTheme => app.next_theme(),
        UiCommand::PaletteToggle => app.toggle_palette(),
        UiCommand::PaletteClose => app.palette = None,
        UiCommand::PaletteUp => app.palette_up(),
        UiCommand::PaletteDown => app.palette_down(),
        UiCommand::PaletteChoose => {
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
        UiCommand::MentionOpen => {
            app.push_char('@');
            app.open_mention();
        }
        UiCommand::MentionSelect => {
            if let Some(item) = app.mention_selected() {
                app.mention_insert(&item);
            }
        }
        UiCommand::MentionUp => app.mention_up(),
        UiCommand::MentionDown => app.mention_down(),
        UiCommand::MentionDismiss => app.mention_dismiss(),
        UiCommand::MentionBackspace => {
            let query_empty = app.mention_query().is_empty();
            app.backspace();
            if query_empty {
                app.mention_dismiss();
            } else {
                let q = app.mention_query();
                app.mention_update(q);
            }
        }
        UiCommand::MentionChar(c) => {
            app.push_char(c);
            let q = app.mention_query();
            app.mention_update(q);
        }
        UiCommand::Submit => {
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
        UiCommand::ResolvePermission(allow) => {
            let sid = session_id.lock().unwrap().clone();
            if let Some(msg) = app.resolve_permission(allow, sid) {
                sink.send(msg).await.context("chat stream closed")?;
            }
        }
        UiCommand::ResolveQuestion { answer } => {
            app.input.clear();
            app.cursor = 0;
            let sid = session_id.lock().unwrap().clone();
            if let Some(msg) = app.resolve_question(answer, sid) {
                sink.send(msg).await.context("chat stream closed")?;
            }
        }
        UiCommand::ResolveDiffReviewAccept => {
            let sid = session_id.lock().unwrap().clone();
            if let Some(msg) = app.resolve_diff_review("accept", None, sid) {
                sink.send(msg).await.context("chat stream closed")?;
            }
        }
        UiCommand::ResolveDiffReviewReject => {
            let sid = session_id.lock().unwrap().clone();
            if let Some(msg) = app.resolve_diff_review("reject", None, sid) {
                sink.send(msg).await.context("chat stream closed")?;
            }
        }
        UiCommand::EditDiffReview => {
            if let Some(content) = edit_diff_review_content(app) {
                let sid = session_id.lock().unwrap().clone();
                if let Some(msg) = app.resolve_diff_review("edit", Some(content), sid) {
                    sink.send(msg).await.context("chat stream closed")?;
                }
            }
        }
        UiCommand::Quit => app.request_quit(),
        UiCommand::Resize => {
            // Snap to the bottom on resize (a safe default); Phase 3 remeasures + clamps precisely.
            app.ui.transcript.scroll_to_end(0, 0);
            app.ui.sidebar_offset = 0;
        }
    }

    // Keep focus in sync with any popup that just opened/closed.
    app.sync_focus();

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

/// Open the pending diff-review's proposed content in `$EDITOR` (fallback `vi`), returning the
/// hand-edited content. Suspends raw mode + mouse capture around the editor so it can take over the
/// terminal cleanly, then re-enters both.
fn edit_diff_review_content(app: &App) -> Option<String> {
    let Some(Prompt::DiffReview { after, .. }) = &app.pending else {
        return None;
    };
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let path = std::env::temp_dir().join(format!("astra-edit-{}.txt", std::process::id()));
    std::fs::write(&path, after.as_bytes()).ok()?;

    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    let _ = std::process::Command::new(&editor).arg(&path).status().ok();
    ratatui::try_init().ok();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);

    let edited = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(edited)
}

/// A bounded workspace file scan for @-file mentions: relative file paths, skipping noise dirs
/// and hidden files, capped at `max` entries.
fn scan_workspace_files(cwd: &std::path::Path, max: usize) -> Vec<String> {
    const SKIP: &[&str] = &["target", "node_modules", ".git", "dist", "out", ".astra"];
    let mut out: Vec<String> = Vec::new();
    let mut stack = vec![cwd.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= max {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= max {
                break;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                if !SKIP.contains(&name.as_str()) {
                    stack.push(path);
                }
            } else if let Ok(rel) = path.strip_prefix(cwd) {
                out.push(rel.to_string_lossy().to_string());
            }
        }
    }
    out.sort();
    out
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
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    async fn apply(app: &mut App, event: Event, sink: &mpsc::Sender<ChatClientMsg>) {
        let sid = Arc::new(Mutex::new(Some("s-9".to_string())));
        let command = route(&event, app);
        apply_command(app, command, sink, &sid).await.unwrap();
    }

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
            apply(&mut app, key(KeyCode::Enter, KeyModifiers::NONE), &sink).await
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
            apply(
                &mut app,
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &sink,
            )
            .await
        });
        assert!(app.quit);
        app.quit = false;

        rt.block_on(async {
            apply(&mut app, key(KeyCode::Char('q'), KeyModifiers::NONE), &sink).await
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
            apply(&mut app, key(KeyCode::Char('q'), KeyModifiers::NONE), &sink).await
        });
        assert!(!app.quit);
        // `q` does not quit when the prompt is non-empty; it is typed normally.
        assert_eq!(app.input, "qq");
    }
}
