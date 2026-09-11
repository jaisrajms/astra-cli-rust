//! Pure TUI application state machine.
//!
//! `App` owns every piece of mutable UI state and exposes side-effect-free
//! transition methods. It has **no** dependency on crossterm or ratatui, so the
//! state transitions are unit-testable without a real terminal. Rendering (in
//! [`super::render`]) and the terminal event loop (in [`super::run`]) are the
//! only layers that touch terminal-specific types.
//!
//! The daemon-facing half of the machine is [`App::apply_chat_event`] /
//! [`App::apply_agent_event`], which translate the `astra.engine.v1`
//! [`AgentEvent`] union (text / tool-call / tool-result / usage / notice /
//! reasoning / done / error) into history lines and tool status. Streaming
//! text is appended incrementally into [`App::streaming`] and only flushed to
//! a history item at the next tool/control boundary, so a long stream never
//! triggers a redraw-from-scratch.

use astra_proto::astra::engine::v1::{
    agent_event, chat_client_msg, chat_event, AgentEvent, ChatClientMsg, ChatEvent, ResolveAskUser,
    ResolveToolPermission, SessionId,
};

/// Braille spinner frames, indexed by [`App::tick`].
pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Agents shown when the daemon cannot be reached (or returns none).
pub const DEFAULT_AGENTS: &[&str] = &["build", "plan", "explore", "general"];

/// Client-side command-palette entries (the leader key opens it).
pub const PALETTE_COMMANDS: &[&str] = &["quit", "theme", "agent", "clear input"];

/// Whether a tool is currently running, and how the most recent one finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStatus {
    Idle,
    Running { id: String, name: String },
    Done { name: String, ok: bool },
}

/// One rendered history line.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    User(String),
    Assistant(String),
    ToolCall {
        name: String,
        input: String,
    },
    ToolResult {
        name: String,
        summary: String,
        ok: bool,
        output: String,
    },
    Notice(String),
    Reasoning(String),
    Error(String),
    Done(String),
}

/// An interactive request the daemon parked, waiting for the user's answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Prompt {
    /// A tool-permission escalation: approve or deny the pending tool call.
    Permission {
        id: String,
        tool: String,
        summary: String,
    },
    /// An ask-user question: answer with free text or a selected option label.
    Question {
        id: String,
        question: String,
        options: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub struct App {
    /// Agent names offered by the picker (already de-duplicated/sorted).
    pub agents: Vec<String>,
    pub agent_index: usize,
    /// The in-progress chat input buffer.
    pub input: String,
    pub cursor: usize,
    /// Prior submitted prompts (oldest first) for up/down recall.
    pub history: Vec<String>,
    /// Position into `history` when recalling (None = editing a fresh prompt).
    pub recall_index: Option<usize>,
    /// Completed history lines, in order.
    pub items: Vec<Item>,
    /// In-flight assistant text (streamed incrementally).
    pub streaming: String,
    pub tool: ToolStatus,
    /// True while a turn is in flight (between a send and its done/error).
    pub running: bool,
    pub error: Option<String>,
    pub title: Option<String>,
    /// The active interactive prompt (permission / ask-user), if any.
    pub pending: Option<Prompt>,
    /// The most recent usage sample (input, output, cost) for the footer statusline.
    pub last_usage: Option<(i64, i64, Option<f64>)>,
    /// Index into the built-in theme list.
    pub theme_index: usize,
    /// Open command-palette selection (None = closed).
    pub palette: Option<usize>,
    /// Set once the user requests a clean exit (Ctrl-C / `q` / Esc).
    pub quit: bool,
    /// Monotonic frame counter driving the spinner animation.
    pub tick: u64,
}

impl App {
    pub fn new(agents: Vec<String>) -> Self {
        let agents = if agents.is_empty() {
            DEFAULT_AGENTS.iter().map(|s| s.to_string()).collect()
        } else {
            agents
        };
        let agent_index = agents.iter().position(|a| a == "build").unwrap_or(0);
        Self {
            agents,
            agent_index,
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            recall_index: None,
            items: Vec::new(),
            streaming: String::new(),
            tool: ToolStatus::Idle,
            running: false,
            error: None,
            title: None,
            pending: None,
            last_usage: None,
            theme_index: 0,
            palette: None,
            quit: false,
            tick: 0,
        }
    }

    /// The name of the currently selected agent.
    pub fn current_agent(&self) -> &str {
        self.agents
            .get(self.agent_index)
            .map(String::as_str)
            .unwrap_or("build")
    }

    /// The spinner glyph for the current tick.
    pub fn spinner(&self) -> &'static str {
        SPINNER[(self.tick as usize) % SPINNER.len()]
    }

    /// Advance the animation frame counter.
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    // --- input editing (all bounds-safe, cursor stays within char count) ---

    pub fn push_char(&mut self, c: char) {
        self.cursor = self.cursor.min(self.input.chars().count());
        let mut chars: Vec<char> = self.input.chars().collect();
        chars.insert(self.cursor, c);
        self.input = chars.into_iter().collect();
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = self.input.chars().collect();
        chars.remove(self.cursor - 1);
        self.input = chars.into_iter().collect();
        self.cursor -= 1;
    }

    pub fn delete_forward(&mut self) {
        let mut chars: Vec<char> = self.input.chars().collect();
        if self.cursor < chars.len() {
            chars.remove(self.cursor);
            self.input = chars.into_iter().collect();
        }
    }

    pub fn cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn cursor_right(&mut self) {
        let len = self.input.chars().count();
        if self.cursor < len {
            self.cursor += 1;
        }
    }

    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor = self.input.chars().count();
    }

    /// Submit the input buffer: returns the message to send when non-empty (and
    /// resets the buffer), or `None` when the input is blank.
    pub fn submit(&mut self) -> Option<String> {
        let content = self.input.trim().to_string();
        if content.is_empty() {
            return None;
        }
        self.input.clear();
        self.cursor = 0;
        Some(content)
    }

    /// Record a user message + begin a new turn.
    pub fn begin_turn(&mut self, content: String) {
        self.items.push(Item::User(content.clone()));
        self.history.push(content);
        self.recall_index = None;
        self.streaming.clear();
        self.tool = ToolStatus::Idle;
        self.error = None;
        self.running = true;
    }

    // --- prompt history recall ---

    /// Recall the previous prompt (Up), or the first previous one if not yet recalling.
    pub fn recall_older(&mut self) {
        if self.history.is_empty() {
            return;
        }
        self.recall_index = Some(match self.recall_index {
            None => self.history.len() - 1,
            Some(i) if i > 0 => i - 1,
            Some(i) => i,
        });
        if let Some(i) = self.recall_index {
            self.input = self.history[i].clone();
            self.cursor = self.input.chars().count();
        }
    }

    /// Recall the next prompt (Down), clearing to a fresh buffer past the newest.
    pub fn recall_newer(&mut self) {
        let Some(i) = self.recall_index else {
            return;
        };
        if i + 1 < self.history.len() {
            self.recall_index = Some(i + 1);
            self.input = self.history[i + 1].clone();
        } else {
            self.recall_index = None;
            self.input.clear();
        }
        self.cursor = self.input.chars().count();
    }

    // --- shell mode ---

    /// Record a shell-mode command (`!command`) as a user line.
    pub fn record_shell(&mut self, command: String) {
        self.items.push(Item::User(format!("!{command}")));
    }

    /// Record the output of a local shell command as a tool-result line.
    pub fn record_shell_output(&mut self, output: String, ok: bool) {
        let summary = if ok { "exit 0" } else { "failed" }.to_string();
        self.items.push(Item::ToolResult {
            name: "shell".to_string(),
            summary,
            ok,
            output,
        });
    }

    // --- agent picker ---

    pub fn next_agent(&mut self) {
        if self.agents.is_empty() {
            return;
        }
        self.agent_index = (self.agent_index + 1) % self.agents.len();
    }

    pub fn prev_agent(&mut self) {
        if self.agents.is_empty() {
            return;
        }
        self.agent_index = (self.agent_index + self.agents.len() - 1) % self.agents.len();
    }

    pub fn request_quit(&mut self) {
        self.quit = true;
    }

    /// Cycle to the next built-in theme.
    pub fn next_theme(&mut self) {
        self.theme_index = (self.theme_index + 1) % super::theme::THEMES.len();
    }

    // --- command palette ---

    pub fn toggle_palette(&mut self) {
        self.palette = if self.palette.is_some() {
            None
        } else {
            Some(0)
        };
    }

    pub fn palette_up(&mut self) {
        if let Some(i) = self.palette {
            self.palette = Some((i + PALETTE_COMMANDS.len() - 1) % PALETTE_COMMANDS.len());
        }
    }

    pub fn palette_down(&mut self) {
        if let Some(i) = self.palette {
            self.palette = Some((i + 1) % PALETTE_COMMANDS.len());
        }
    }

    /// The selected palette command label, or `None` when the palette is closed.
    pub fn palette_selected(&self) -> Option<&'static str> {
        self.palette.map(|i| PALETTE_COMMANDS[i])
    }

    /// Resolve the pending permission prompt: `allow` true → allow, false → deny. Returns the
    /// resolver message to send, or `None` when there is no pending permission prompt.
    pub fn resolve_permission(
        &mut self,
        allow: bool,
        session_id: Option<String>,
    ) -> Option<ChatClientMsg> {
        let Some(Prompt::Permission { id, .. }) = self.pending.take() else {
            return None;
        };
        Some(ChatClientMsg {
            session_id: session_id.map(|v| SessionId { value: v }),
            payload: Some(chat_client_msg::Payload::ResolveToolPermission(
                ResolveToolPermission {
                    id,
                    allow,
                    persist: String::new(),
                },
            )),
        })
    }

    /// Resolve the pending ask-user prompt with a free-text answer. Returns the resolver message,
    /// or `None` when there is no pending question.
    pub fn resolve_question(
        &mut self,
        answer: String,
        session_id: Option<String>,
    ) -> Option<ChatClientMsg> {
        let Some(Prompt::Question { id, .. }) = self.pending.take() else {
            return None;
        };
        Some(ChatClientMsg {
            session_id: session_id.map(|v| SessionId { value: v }),
            payload: Some(chat_client_msg::Payload::ResolveAskUser(ResolveAskUser {
                id,
                answer,
            })),
        })
    }

    // --- daemon events ---

    /// Apply one multiplexed [`ChatEvent`]: forwards agent events to
    /// [`App::apply_agent_event`] and handles host-synthesized control events.
    pub fn apply_chat_event(&mut self, event: &ChatEvent) {
        match &event.payload {
            Some(chat_event::Payload::AgentEvent(env)) => {
                if !crate::protocol::compatible(env.protocol_version) {
                    self.items.push(Item::Notice(format!(
                        "skipped agent event with incompatible protocol version {}",
                        env.protocol_version
                    )));
                } else if let Some(event) = &env.event {
                    self.apply_agent_event(event);
                }
            }
            Some(chat_event::Payload::SessionError(e)) => {
                self.flush_streaming();
                self.items.push(Item::Error(e.message.clone()));
                self.error = Some(e.message.clone());
                self.running = false;
            }
            Some(chat_event::Payload::TitleChanged(t)) => {
                self.title = Some(t.title.clone());
            }
            Some(chat_event::Payload::SessionEnded(_)) => {
                self.running = false;
            }
            Some(chat_event::Payload::ToolPermissionRequest(r)) => {
                self.flush_streaming();
                self.pending = Some(Prompt::Permission {
                    id: r.id.clone(),
                    tool: r.tool_name.clone(),
                    summary: r.summary.clone(),
                });
            }
            Some(chat_event::Payload::AskUserRequest(r)) => {
                self.flush_streaming();
                self.pending = Some(Prompt::Question {
                    id: r.id.clone(),
                    question: r.question.clone(),
                    options: r.options.clone(),
                });
            }
            _ => {}
        }
    }

    /// Apply one [`AgentEvent`] variant.
    pub fn apply_agent_event(&mut self, event: &AgentEvent) {
        match &event.kind {
            Some(agent_event::Kind::Text(t)) => self.streaming.push_str(&t.text),
            Some(agent_event::Kind::ToolCall(t)) => {
                self.flush_streaming();
                self.items.push(Item::ToolCall {
                    name: t.name.clone(),
                    input: t.input.clone(),
                });
                self.tool = ToolStatus::Running {
                    id: t.id.clone(),
                    name: t.name.clone(),
                };
            }
            Some(agent_event::Kind::ToolResult(t)) => {
                self.items.push(Item::ToolResult {
                    name: t.name.clone(),
                    summary: t.summary.clone(),
                    ok: !t.is_error,
                    output: t.output.clone(),
                });
                self.tool = ToolStatus::Done {
                    name: t.name.clone(),
                    ok: !t.is_error,
                };
            }
            Some(agent_event::Kind::Usage(u)) => {
                // Usage is surfaced in the footer statusline, not the message scrollback
                // (reference-CLI parity); the last sample is kept for rendering.
                self.last_usage = Some((u.input_tokens, u.output_tokens, u.cost_usd));
            }
            Some(agent_event::Kind::Notice(n)) => self.items.push(Item::Notice(n.text.clone())),
            Some(agent_event::Kind::Reasoning(r)) => {
                self.items.push(Item::Reasoning(r.text.clone()))
            }
            Some(agent_event::Kind::Done(d)) => {
                self.flush_streaming();
                if let Some(result) = &d.result {
                    if !result.is_empty() {
                        self.items.push(Item::Done(result.clone()));
                    }
                }
                self.running = false;
            }
            Some(agent_event::Kind::Error(e)) => {
                self.flush_streaming();
                self.items.push(Item::Error(e.message.clone()));
                self.error = Some(e.message.clone());
                self.running = false;
            }
            None => {}
        }
    }

    /// Move any buffered streaming text into a completed `Assistant` history
    /// item, clearing the buffer.
    fn flush_streaming(&mut self) {
        let text = std::mem::take(&mut self.streaming);
        if !text.is_empty() {
            self.items.push(Item::Assistant(text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::{
        agent_event, AgentEvent, DoneEvent, ErrorEvent, NoticeEvent, ReasoningEvent, TextEvent,
        ToolCallEvent, ToolResultEvent, UsageEvent,
    };

    fn ev(kind: agent_event::Kind) -> AgentEvent {
        AgentEvent { kind: Some(kind) }
    }

    fn text(s: &str) -> AgentEvent {
        ev(agent_event::Kind::Text(TextEvent { text: s.into() }))
    }

    fn tool_call(id: &str, name: &str) -> AgentEvent {
        ev(agent_event::Kind::ToolCall(ToolCallEvent {
            id: id.into(),
            name: name.into(),
            input: "{}".into(),
        }))
    }

    fn tool_result(name: &str, ok: bool) -> AgentEvent {
        ev(agent_event::Kind::ToolResult(ToolResultEvent {
            id: "t1".into(),
            name: name.into(),
            summary: "done".into(),
            output: "".into(),
            is_error: !ok,
            truncated: false,
            full_output: None,
        }))
    }

    fn usage() -> AgentEvent {
        ev(agent_event::Kind::Usage(UsageEvent {
            input_tokens: 12,
            output_tokens: 7,
            cost_usd: Some(0.0042),
            cache_read_tokens: None,
            cache_write_tokens: None,
        }))
    }

    fn done() -> AgentEvent {
        ev(agent_event::Kind::Done(DoneEvent {
            result: None,
            num_turns: Some(1),
        }))
    }

    #[test]
    fn scripted_sequence_produces_correct_state() {
        let mut app = App::new(vec!["build".into(), "plan".into()]);
        app.begin_turn("do the thing".into());

        app.apply_agent_event(&text("hello "));
        app.apply_agent_event(&text("world"));
        assert_eq!(app.streaming, "hello world");
        assert_eq!(app.tool, ToolStatus::Idle);

        app.apply_agent_event(&tool_call("t1", "bash"));
        // streaming text is flushed to history at the tool boundary
        assert_eq!(app.streaming, "");
        assert!(matches!(app.items[1], Item::Assistant(ref s) if s == "hello world"));
        assert!(matches!(app.items[2], Item::ToolCall { ref name, .. } if name == "bash"));
        assert_eq!(
            app.tool,
            ToolStatus::Running {
                id: "t1".into(),
                name: "bash".into()
            }
        );

        app.apply_agent_event(&tool_result("bash", true));
        assert_eq!(
            app.tool,
            ToolStatus::Done {
                name: "bash".into(),
                ok: true
            }
        );

        app.apply_agent_event(&usage());
        assert_eq!(
            app.last_usage,
            Some((12, 7, Some(0.0042))),
            "usage is tracked in the footer, not the scrollback"
        );

        assert!(app.running);
        app.apply_agent_event(&done());
        assert!(!app.running);

        // full history order: user, assistant(text), tool-call, tool-result
        assert_eq!(app.items.len(), 4);
        assert!(matches!(app.items[0], Item::User(ref s) if s == "do the thing"));
        assert!(matches!(app.items[1], Item::Assistant(ref s) if s == "hello world"));
        assert!(matches!(app.items[2], Item::ToolCall { ref name, .. } if name == "bash"));
        assert!(
            matches!(app.items[3], Item::ToolResult { ref name, ok: true, .. } if name == "bash")
        );
    }

    #[test]
    fn failed_tool_result_marks_tool_failed() {
        let mut app = App::new(vec![]);
        app.apply_agent_event(&tool_call("t2", "edit"));
        app.apply_agent_event(&tool_result("edit", false));
        assert_eq!(
            app.tool,
            ToolStatus::Done {
                name: "edit".into(),
                ok: false
            }
        );
        assert!(matches!(
            app.items.last(),
            Some(Item::ToolResult { ok: false, .. })
        ));
    }

    #[test]
    fn every_event_variant_is_rendered_to_history() {
        let mut app = App::new(vec![]);
        app.apply_agent_event(&ev(agent_event::Kind::Notice(NoticeEvent {
            text: "n".into(),
        })));
        app.apply_agent_event(&ev(agent_event::Kind::Reasoning(ReasoningEvent {
            text: "r".into(),
            seq: 0,
        })));
        app.apply_agent_event(&ev(agent_event::Kind::Error(ErrorEvent {
            message: "boom".into(),
        })));
        assert!(matches!(app.items[0], Item::Notice(ref s) if s == "n"));
        assert!(matches!(app.items[1], Item::Reasoning(ref s) if s == "r"));
        assert!(matches!(app.items[2], Item::Error(ref s) if s == "boom"));
        assert_eq!(app.error.as_deref(), Some("boom"));
        assert!(!app.running);
    }

    #[test]
    fn done_flushes_streaming_and_stops_running() {
        let mut app = App::new(vec![]);
        app.running = true;
        app.apply_agent_event(&text("final"));
        assert_eq!(app.streaming, "final");
        app.apply_agent_event(&done());
        assert_eq!(app.streaming, "");
        assert!(matches!(app.items.last(), Some(Item::Assistant(ref s)) if s == "final"));
        assert!(!app.running);
    }

    #[test]
    fn input_editing_respects_cursor() {
        let mut app = App::new(vec![]);
        app.push_char('a');
        app.push_char('c');
        app.cursor_left();
        app.push_char('b'); // insert before 'c'
        assert_eq!(app.input, "abc");
        app.cursor_home();
        app.delete_forward();
        assert_eq!(app.input, "bc");
        app.cursor_end();
        app.backspace();
        assert_eq!(app.input, "b");
    }

    #[test]
    fn submit_trims_and_resets() {
        let mut app = App::new(vec![]);
        app.push_char(' ');
        app.push_char('x');
        app.push_char(' ');
        assert_eq!(app.submit(), Some("x".into()));
        assert!(app.input.is_empty());
        assert_eq!(app.cursor, 0);
        assert_eq!(app.submit(), None);
    }

    #[test]
    fn agent_picker_wraps_and_defaults_to_build() {
        let mut app = App::new(vec!["build".into(), "plan".into(), "explore".into()]);
        assert_eq!(app.current_agent(), "build");
        app.prev_agent();
        assert_eq!(app.current_agent(), "explore");
        app.next_agent();
        assert_eq!(app.current_agent(), "build");
    }

    #[test]
    fn empty_agents_fall_back_to_defaults() {
        let app = App::new(vec![]);
        assert_eq!(app.agents, DEFAULT_AGENTS);
        assert_eq!(app.current_agent(), "build");
    }

    #[test]
    fn tick_cycles_spinner() {
        let mut app = App::new(vec![]);
        assert_eq!(app.spinner(), SPINNER[0]);
        for _ in 0..SPINNER.len() {
            app.tick();
        }
        assert_eq!(app.spinner(), SPINNER[0]);
    }

    #[test]
    fn control_events_are_applied() {
        use astra_proto::astra::engine::v1::{
            chat_event, AgentEventEnvelope, ChatEvent, SessionError, TitleChanged,
        };
        let mut app = App::new(vec![]);

        let chat = ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::TitleChanged(TitleChanged {
                title: "my session".into(),
            })),
        };
        app.apply_chat_event(&chat);
        assert_eq!(app.title.as_deref(), Some("my session"));

        let err = ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::SessionError(SessionError {
                message: "nope".into(),
            })),
        };
        app.apply_chat_event(&err);
        assert!(matches!(app.items.last(), Some(Item::Error(ref s)) if s == "nope"));

        // A chat event wrapping an agent event reaches the same code path.
        let env = ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::AgentEvent(AgentEventEnvelope {
                protocol_version: 1.3,
                event: Some(text("via chat")),
            })),
        };
        app.apply_chat_event(&env);
        assert_eq!(app.streaming, "via chat");
    }

    #[test]
    fn incompatible_protocol_version_is_skipped_with_notice() {
        use astra_proto::astra::engine::v1::{chat_event, AgentEventEnvelope, ChatEvent};
        let mut app = App::new(vec![]);

        let chat = ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::AgentEvent(AgentEventEnvelope {
                protocol_version: 2.0,
                event: Some(text("should not render")),
            })),
        };
        app.apply_chat_event(&chat);

        assert!(app.streaming.is_empty(), "mismatched event must be skipped");
        assert!(matches!(
            app.items.last(),
            Some(Item::Notice(ref s)) if s.contains("incompatible protocol version")
        ));
    }

    #[test]
    fn unknown_event_variant_is_skipped_gracefully() {
        use astra_proto::astra::engine::v1::{chat_event, AgentEventEnvelope, ChatEvent};
        let mut app = App::new(vec![]);

        // A future/unknown oneof variant yields `None` for `env.event`.
        let chat = ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::AgentEvent(AgentEventEnvelope {
                protocol_version: 1.3,
                event: None,
            })),
        };
        app.apply_chat_event(&chat);

        assert!(app.items.is_empty(), "unknown event must not add history");
        assert!(app.streaming.is_empty());
    }

    #[test]
    fn permission_request_parks_and_resolves() {
        use astra_proto::astra::engine::v1::{chat_event, ChatEvent, ToolPermissionRequest};
        let mut app = App::new(vec!["build".into()]);

        let event = ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::ToolPermissionRequest(
                ToolPermissionRequest {
                    id: "p1".into(),
                    tool_name: "Bash".into(),
                    summary: "Bash command".into(),
                    prefix: String::new(),
                },
            )),
        };
        app.apply_chat_event(&event);
        assert!(matches!(app.pending, Some(Prompt::Permission { .. })));

        let msg = app
            .resolve_permission(true, Some("s1".into()))
            .expect("resolver");
        assert!(matches!(
            msg.payload,
            Some(chat_client_msg::Payload::ResolveToolPermission(r)) if r.allow
        ));
        assert!(app.pending.is_none());
    }

    #[test]
    fn ask_user_request_parks_and_resolves() {
        use astra_proto::astra::engine::v1::{chat_event, AskUserRequest, ChatEvent};
        let mut app = App::new(vec!["build".into()]);

        let event = ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::AskUserRequest(AskUserRequest {
                id: "q1".into(),
                question: "Which?".into(),
                options: vec!["A".into(), "B".into()],
                header: None,
                multi_select: false,
                descriptions: vec![],
            })),
        };
        app.apply_chat_event(&event);
        assert!(matches!(app.pending, Some(Prompt::Question { .. })));

        let msg = app
            .resolve_question("B".into(), Some("s1".into()))
            .expect("resolver");
        assert!(matches!(
            msg.payload,
            Some(chat_client_msg::Payload::ResolveAskUser(r)) if r.answer == "B"
        ));
        assert!(app.pending.is_none());
    }

    #[test]
    fn prompt_history_recall_navigates_prior_prompts() {
        let mut app = App::new(vec![]);
        app.begin_turn("first".into());
        app.begin_turn("second".into());

        app.recall_older();
        assert_eq!(app.input, "second");
        app.recall_older();
        assert_eq!(app.input, "first");
        app.recall_older(); // stays at the oldest
        assert_eq!(app.input, "first");

        app.recall_newer();
        assert_eq!(app.input, "second");
        app.recall_newer(); // past newest -> fresh buffer
        assert_eq!(app.input, "");
    }
}
