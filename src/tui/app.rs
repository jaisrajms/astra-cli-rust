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

use std::collections::HashSet;

use astra_proto::astra::engine::v1::{
    agent_event, chat_client_msg, chat_event, AgentEvent, ChatClientMsg, ChatEvent, ResolveAskUser,
    ResolveDiffReview, ResolveToolPermission, SessionId,
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

/// Which surface owns keyboard focus. Routing precedence is applied by the event layer (Phase 2);
/// this is only the *state* of focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Focus variants are wired to routing in Phase 2.
pub enum Focus {
    Prompt,
    Transcript,
    Sidebar,
    Popup(PopupKind),
}

/// The popup kinds the focus model tracks (each maps to an existing `pending`/`palette`/`mention`
/// surface; the consolidation of those flags onto this enum lands with the event layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // PopupKind variants are consolidated with pending/palette/mention in Phase 2.
pub enum PopupKind {
    Palette,
    Mention,
    Permission,
    Question,
    DiffReview,
}

/// How the sidebar is presented on a narrow terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Docked/Overlay are applied by the layout layer in Phase 5.
pub enum SidebarMode {
    Hidden,
    Docked,
    Overlay,
}

/// Vertical scroll position + auto-follow flag for one scroll domain (transcript, sidebar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScrollState {
    pub offset: u16,
    pub follow_bottom: bool,
}

#[allow(dead_code)] // Scroll methods are wired into transcript scrolling in Phase 3.
impl ScrollState {
    pub fn at_bottom() -> Self {
        Self {
            offset: 0,
            follow_bottom: true,
        }
    }

    /// Clamp `offset` into `[0, max(total, viewport)]`.
    pub fn clamp(&mut self, total: usize, viewport: usize) {
        self.offset = self.offset.min(max_offset(total, viewport));
    }

    /// Scroll by `delta` rows. A negative delta (scrolling up) disables auto-follow; landing back at
    /// the bottom re-enables it.
    pub fn scroll_by(&mut self, delta: i32, total: usize, viewport: usize) {
        if delta < 0 {
            self.follow_bottom = false;
        }
        let max = max_offset(total, viewport) as i32;
        let next = (self.offset as i32 + delta).clamp(0, max);
        self.offset = next as u16;
        if next >= max {
            self.follow_bottom = true;
        }
    }

    /// Jump to the top; auto-follow stays off unless the content fits in the viewport.
    pub fn scroll_to_start(&mut self, total: usize, viewport: usize) {
        self.offset = 0;
        self.follow_bottom = total <= viewport;
    }

    /// Jump to the bottom and resume auto-follow.
    pub fn scroll_to_end(&mut self, total: usize, viewport: usize) {
        self.offset = max_offset(total, viewport);
        self.follow_bottom = true;
    }

    /// Called after content changes: clamp, then move to the new bottom only while following.
    pub fn on_content_changed(&mut self, total: usize, viewport: usize) {
        self.clamp(total, viewport);
        if self.follow_bottom {
            self.offset = max_offset(total, viewport);
        }
    }
}

/// The largest legal scroll offset for a `total`-row body in a `viewport`-row window.
#[allow(dead_code)] // used only via the ScrollState methods (wired in Phase 3).
fn max_offset(total: usize, viewport: usize) -> u16 {
    total.saturating_sub(viewport).min(u16::MAX as usize) as u16
}

/// Presentation-only state, kept separate from daemon truth (`items`, `streaming`, `tool`). This is
/// the ratatui-native equivalent of the reference CLI's sticky scrollbox + component-local collapse
/// signals: ratatui owns no widget state, so we hold it here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiState {
    pub focus: Focus,
    pub transcript: ScrollState,
    pub sidebar_offset: u16,
    pub sidebar: SidebarMode,
    /// Tool items expanded past their bounded preview, keyed by stable tool id.
    pub expanded_tools: HashSet<String>,
    /// Reasoning blocks expanded, keyed by stable reasoning id.
    pub expanded_reasoning: HashSet<String>,
    /// The resolved model id + context window from the latest `UsageEvent` (status/context meters).
    pub model: Option<String>,
    pub context_limit: Option<u64>,
    /// MCP server connection status from `ConnectionStatusEvent` (name, status).
    pub mcp: Vec<(String, String)>,
    /// Whether an LSP provider is attached.
    pub lsp_enabled: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            focus: Focus::Prompt,
            transcript: ScrollState::at_bottom(),
            sidebar_offset: 0,
            sidebar: SidebarMode::Hidden,
            expanded_tools: HashSet::new(),
            expanded_reasoning: HashSet::new(),
            model: None,
            context_limit: None,
            mcp: Vec::new(),
            lsp_enabled: false,
        }
    }
}

impl UiState {
    /// Clamp the sidebar offset into `[0, max(total, viewport)]`.
    #[allow(dead_code)] // wired into sidebar scrolling in Phase 5.
    pub fn clamp_sidebar(&mut self, total: usize, viewport: usize) {
        self.sidebar_offset = self.sidebar_offset.min(max_offset(total, viewport));
    }
}

/// The finished state of a tool call (reference CLI: tool part `state.output`/`state.error`).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub ok: bool,
    pub summary: String,
    pub output: String,
}

/// One rendered history line.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    User(String),
    Assistant(String),
    ToolCall {
        /// Stable daemon id (the `ToolCallEvent.id`), so expansion state survives item insertion.
        id: String,
        name: String,
        input: String,
        /// `None` while in flight; `Some` once the tool has finished.
        outcome: Option<ToolOutcome>,
    },
    Notice(String),
    Reasoning {
        /// Stable block id (the block's `start_ms`, or the first delta's `seq`).
        id: String,
        text: String,
        start_ms: Option<i64>,
        end_ms: Option<i64>,
    },
    Error(String),
    Done(String),
}

/// A single session task (reference CLI: the TodoWrite tool's `todos` array).
#[derive(Debug, Clone, PartialEq)]
pub struct Todo {
    pub content: String,
    pub status: String,
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
    /// A diff-review for a proposed Write/Edit: accept, reject, or edit the content.
    DiffReview {
        id: String,
        tool_name: String,
        file_path: String,
        before: String,
        after: String,
        unified_diff: String,
    },
}

/// An @-mention candidate: an agent or a workspace file.
#[derive(Debug, Clone, PartialEq)]
pub enum MentionItem {
    Agent(String),
    File(String),
}

impl MentionItem {
    pub fn label(&self) -> &str {
        match self {
            MentionItem::Agent(n) => n,
            MentionItem::File(p) => p,
        }
    }
}

/// The open @-mention popup.
#[derive(Debug, Clone)]
pub struct Mention {
    /// Character index in `input` where the `@` that opened this mention sits.
    pub trigger: usize,
    pub query: String,
    pub items: Vec<MentionItem>,
    pub selection: usize,
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
    /// The current session task list, as last written by the TodoWrite tool.
    pub todos: Vec<Todo>,
    /// Index into the built-in theme list.
    pub theme_index: usize,
    /// Open command-palette selection (None = closed).
    pub palette: Option<usize>,
    /// The open @-mention popup, if any.
    pub mention: Option<Mention>,
    /// Cached workspace file list (relative paths) for @-file mentions.
    pub files: Vec<String>,
    /// Set once the user requests a clean exit (Ctrl-C / `q` / Esc).
    pub quit: bool,
    /// Monotonic frame counter driving the spinner animation.
    pub tick: u64,
    /// Presentation-only state (focus, scroll, expansion, sidebar, model/context/mcp).
    pub ui: UiState,
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
            todos: Vec::new(),
            theme_index: 0,
            palette: None,
            mention: None,
            files: Vec::new(),
            quit: false,
            tick: 0,
            ui: UiState::default(),
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

    /// Record a shell-mode command (`!command`) as an in-flight Bash tool line.
    pub fn record_shell(&mut self, command: String) {
        let input = serde_json::json!({ "command": command }).to_string();
        self.items.push(Item::ToolCall {
            id: format!("shell-{}", self.tick),
            name: "Bash".to_string(),
            input,
            outcome: None,
        });
    }

    /// Attach the output of a local shell command to the last in-flight Bash tool line.
    pub fn record_shell_output(&mut self, output: String, ok: bool) {
        if let Some(Item::ToolCall { outcome, .. }) = self
            .items
            .iter_mut()
            .rev()
            .find(|i| matches!(i, Item::ToolCall { outcome: None, .. }))
        {
            *outcome = Some(ToolOutcome {
                ok,
                summary: if ok {
                    "exit 0".to_string()
                } else {
                    "failed".to_string()
                },
                output,
            });
        }
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

    // --- @-mentions ---

    /// Set the cached workspace file list (for @-file mentions).
    pub fn set_files(&mut self, files: Vec<String>) {
        self.files = files;
    }

    /// Open the @-mention popup; `trigger` is the index of the `@` in `input` (the `@` was just
    /// pushed, so it is `input.len() - 1`).
    pub fn open_mention(&mut self) {
        if self.mention.is_some() {
            return;
        }
        let trigger = self.input.chars().count().saturating_sub(1);
        let items = self.mention_candidates("");
        self.mention = Some(Mention {
            trigger,
            query: String::new(),
            items,
            selection: 0,
        });
    }

    /// Re-filter the mention popup for the query (the text after the trigger `@`).
    pub fn mention_update(&mut self, query: String) {
        let items = self.mention_candidates(&query);
        if let Some(m) = &mut self.mention {
            m.query = query;
            m.selection = 0;
            m.items = items;
        }
    }

    fn mention_candidates(&self, query: &str) -> Vec<MentionItem> {
        let q = query.to_lowercase();
        let mut items: Vec<MentionItem> = self
            .agents
            .iter()
            .filter(|a| a.to_lowercase().contains(&q))
            .map(|a| MentionItem::Agent(a.clone()))
            .collect();
        items.extend(
            self.files
                .iter()
                .filter(|f| f.to_lowercase().contains(&q))
                .map(|f| MentionItem::File(f.clone())),
        );
        items.truncate(60);
        items
    }

    pub fn mention_up(&mut self) {
        if let Some(m) = &mut self.mention {
            if !m.items.is_empty() {
                m.selection = (m.selection + m.items.len() - 1) % m.items.len();
            }
        }
    }

    pub fn mention_down(&mut self) {
        if let Some(m) = &mut self.mention {
            if !m.items.is_empty() {
                m.selection = (m.selection + 1) % m.items.len();
            }
        }
    }

    pub fn mention_selected(&self) -> Option<MentionItem> {
        self.mention
            .as_ref()
            .and_then(|m| m.items.get(m.selection).cloned())
    }

    /// Replace the `@query` span (from the trigger `@` to the end) with the selected item's label.
    pub fn mention_insert(&mut self, item: &MentionItem) {
        let Some(m) = &self.mention else {
            return;
        };
        let chars: Vec<char> = self.input.chars().collect();
        let trigger = m.trigger.min(chars.len());
        let label = item.label();
        let mut new_input: String = chars[..trigger].iter().collect();
        new_input.push_str(label);
        new_input.push(' ');
        self.input = new_input;
        self.cursor = self.input.chars().count();
        self.mention = None;
    }

    /// The query text after the trigger `@`, or empty when no mention is open.
    pub fn mention_query(&self) -> String {
        match &self.mention {
            Some(m) => self.input.chars().skip(m.trigger + 1).collect(),
            None => String::new(),
        }
    }

    pub fn mention_dismiss(&mut self) {
        self.mention = None;
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

    /// Resolve the pending diff-review prompt with a verdict (`"accept"` | `"reject"` | `"edit"`)
    /// and, for `"edit"`, the hand-edited content. Returns the resolver message, or `None` when no
    /// diff-review prompt is pending.
    pub fn resolve_diff_review(
        &mut self,
        verdict: &str,
        new_content: Option<String>,
        session_id: Option<String>,
    ) -> Option<ChatClientMsg> {
        let Some(Prompt::DiffReview { id, .. }) = self.pending.take() else {
            return None;
        };
        Some(ChatClientMsg {
            session_id: session_id.map(|v| SessionId { value: v }),
            payload: Some(chat_client_msg::Payload::ResolveDiffReview(
                ResolveDiffReview {
                    id,
                    verdict: verdict.to_string(),
                    new_content,
                },
            )),
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
            Some(chat_event::Payload::ConnectionStatusEvent(s)) => {
                self.ui.lsp_enabled = s.lsp_enabled;
                self.ui.mcp = s
                    .mcp_servers
                    .iter()
                    .map(|m| (m.name.clone(), m.status.clone()))
                    .collect();
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
            Some(chat_event::Payload::DiffReviewRequest(r)) => {
                self.flush_streaming();
                self.pending = Some(Prompt::DiffReview {
                    id: r.id.clone(),
                    tool_name: r.tool_name.clone(),
                    file_path: r.file_path.clone(),
                    before: r.before.clone(),
                    after: r.after.clone(),
                    unified_diff: r.unified_diff.clone(),
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
                // Track the session task list whenever the TodoWrite tool rewrites it.
                if t.name == "TodoWrite" {
                    self.todos = parse_todos(&t.input);
                }
                self.items.push(Item::ToolCall {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    input: t.input.clone(),
                    outcome: None,
                });
                self.tool = ToolStatus::Running {
                    id: t.id.clone(),
                    name: t.name.clone(),
                };
            }
            Some(agent_event::Kind::ToolResult(t)) => {
                // Fold the outcome into the matching in-flight tool line (the reference CLI renders
                // a tool as a single line whose color flips once it finishes).
                if let Some(Item::ToolCall { outcome, .. }) = self
                    .items
                    .iter_mut()
                    .rev()
                    .find(|i| matches!(i, Item::ToolCall { outcome: None, .. }))
                {
                    *outcome = Some(ToolOutcome {
                        ok: !t.is_error,
                        summary: t.summary.clone(),
                        output: t.output.clone(),
                    });
                }
                self.tool = ToolStatus::Done {
                    name: t.name.clone(),
                    ok: !t.is_error,
                };
            }
            Some(agent_event::Kind::Usage(u)) => {
                // Usage is surfaced in the footer statusline, not the message scrollback
                // (reference-CLI parity); the last sample is kept for rendering.
                self.last_usage = Some((u.input_tokens, u.output_tokens, u.cost_usd));
                // The resolved model + context window feed the status/context meters.
                self.ui.model = u.model.clone();
                self.ui.context_limit = u.context_limit.map(|v| v as u64);
            }
            Some(agent_event::Kind::Notice(n)) => self.items.push(Item::Notice(n.text.clone())),
            Some(agent_event::Kind::Reasoning(r)) => self.apply_reasoning(r),
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

    /// Accumulate one reasoning delta into the open reasoning block (or start a new one). Blocks are
    /// keyed by `start_ms` (the daemon stamps the block start on every delta); a delta whose block
    /// differs from the last open item starts a fresh block.
    fn apply_reasoning(&mut self, r: &astra_proto::astra::engine::v1::ReasoningEvent) {
        let appended = match self.items.last_mut() {
            Some(Item::Reasoning {
                text,
                start_ms: last_start,
                end_ms,
                ..
            }) if last_start == &r.start_ms => {
                text.push_str(&r.text);
                if r.end_ms.is_some() {
                    *end_ms = r.end_ms;
                }
                true
            }
            _ => false,
        };
        if !appended {
            let id = r
                .start_ms
                .map(|v| v.to_string())
                .unwrap_or_else(|| format!("r{}", r.seq));
            self.items.push(Item::Reasoning {
                id,
                text: r.text.clone(),
                start_ms: r.start_ms,
                end_ms: r.end_ms,
            });
        }
    }
}

/// Parse the TodoWrite tool's `todos` array into `[{ content, status }, ...]`.
fn parse_todos(input: &str) -> Vec<Todo> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        return Vec::new();
    };
    let Some(items) = value.get("todos").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            Some(Todo {
                content: item.get("content")?.as_str()?.to_string(),
                status: item.get("status")?.as_str()?.to_string(),
            })
        })
        .collect()
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
            model: None,
            context_limit: None,
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
        assert!(
            matches!(app.items[2], Item::ToolCall { ref name, outcome: None, .. } if name == "bash")
        );
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

        // full history order: user, assistant(text), tool-call (folded with its outcome)
        assert_eq!(app.items.len(), 3);
        assert!(matches!(app.items[0], Item::User(ref s) if s == "do the thing"));
        assert!(matches!(app.items[1], Item::Assistant(ref s) if s == "hello world"));
        assert!(matches!(app.items[2],
                Item::ToolCall { ref name, outcome: Some(ToolOutcome { ok: true, .. }), .. }
                if name == "bash"));
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
            Some(Item::ToolCall {
                outcome: Some(ToolOutcome { ok: false, .. }),
                ..
            })
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
            start_ms: None,
            end_ms: None,
        })));
        app.apply_agent_event(&ev(agent_event::Kind::Error(ErrorEvent {
            message: "boom".into(),
        })));
        assert!(matches!(app.items[0], Item::Notice(ref s) if s == "n"));
        assert!(matches!(app.items[1], Item::Reasoning { ref text, .. } if text == "r"));
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
    fn diff_review_request_parks_and_resolves_verdicts() {
        use astra_proto::astra::engine::v1::{chat_event, ChatEvent, DiffReviewRequest};
        let mut app = App::new(vec!["build".into()]);

        let event = ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::DiffReviewRequest(DiffReviewRequest {
                id: "d1".into(),
                tool_name: "Write".into(),
                file_path: "../out.txt".into(),
                before: String::new(),
                after: "hello".into(),
                unified_diff: "--- ../out.txt\n+++ ../out.txt\n+hello\n".into(),
            })),
        };
        app.apply_chat_event(&event);
        assert!(matches!(app.pending, Some(Prompt::DiffReview { .. })));

        // Accept -> ResolveDiffReview { verdict: "accept" }.
        let msg = app
            .resolve_diff_review("accept", None, Some("s1".into()))
            .expect("resolver");
        assert!(matches!(
            msg.payload,
            Some(chat_client_msg::Payload::ResolveDiffReview(ref r))
                if r.verdict == "accept" && r.new_content.is_none()
        ));
        assert!(app.pending.is_none());

        // Edit -> ResolveDiffReview { verdict: "edit", new_content }.
        app.apply_chat_event(&event);
        let msg = app
            .resolve_diff_review("edit", Some("edited".into()), Some("s1".into()))
            .expect("resolver");
        assert!(matches!(
            msg.payload,
            Some(chat_client_msg::Payload::ResolveDiffReview(ref r))
                if r.verdict == "edit" && r.new_content.as_deref() == Some("edited")
        ));
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

    #[test]
    fn mention_filters_agents_and_files_and_inserts() {
        let mut app = App::new(vec!["build".into(), "plan".into()]);
        app.set_files(vec!["src/main.rs".into(), "src/lib.rs".into()]);

        app.push_char('@');
        app.open_mention();
        assert!(app.mention.is_some());
        let items = &app.mention.as_ref().unwrap().items;
        assert!(items
            .iter()
            .any(|i| matches!(i, MentionItem::Agent(a) if a == "build")));
        assert!(items
            .iter()
            .any(|i| matches!(i, MentionItem::File(p) if p == "src/main.rs")));

        // Typing narrows to `m` matches only.
        app.push_char('m');
        app.mention_update(app.mention_query());
        let items = &app.mention.as_ref().unwrap().items;
        assert!(!items.is_empty());
        assert!(items.iter().all(|i| i.label().contains('m')));

        // Insert the selected item; the `@` is consumed.
        let sel = app.mention_selected().unwrap();
        app.mention_insert(&sel);
        assert!(app.mention.is_none());
        assert!(!app.input.contains('@'), "input: {}", app.input);
    }

    #[test]
    fn scroll_state_offsets_clamp_and_follow_bottom() {
        let mut s = ScrollState::at_bottom();
        assert!(s.follow_bottom);

        // Content that fits: max offset is zero.
        s.on_content_changed(5, 20);
        assert_eq!(s.offset, 0);
        assert!(s.follow_bottom);

        // Content that overflows: following sticks to the bottom.
        s.on_content_changed(100, 20);
        assert_eq!(s.offset, 80);
        assert!(s.follow_bottom);

        // Scrolling up disables auto-follow.
        s.scroll_by(-10, 100, 20);
        assert_eq!(s.offset, 70);
        assert!(!s.follow_bottom);

        // New content while NOT following does not move the viewport.
        s.on_content_changed(200, 20);
        assert_eq!(s.offset, 70);
        assert!(!s.follow_bottom);

        // Clamp handles a shrunken body (offset above the new max).
        s.on_content_changed(50, 20);
        assert_eq!(s.offset, 30);

        // Home goes to zero and stays unfollowing (content overflows).
        s.scroll_to_start(50, 20);
        assert_eq!(s.offset, 0);
        assert!(!s.follow_bottom);

        // End goes to the bottom and resumes following.
        s.scroll_to_end(50, 20);
        assert_eq!(s.offset, 30);
        assert!(s.follow_bottom);
    }

    #[test]
    fn scroll_state_clamps_to_zero_and_max() {
        let mut s = ScrollState {
            offset: u16::MAX,
            follow_bottom: false,
        };
        s.clamp(10, 5);
        assert_eq!(s.offset, 5);

        s.scroll_by(-999, 10, 5);
        assert_eq!(s.offset, 0);

        s.scroll_by(999, 10, 5);
        assert_eq!(s.offset, 5);
        assert!(s.follow_bottom, "reaching the bottom resumes following");
    }

    #[test]
    fn scroll_state_landing_on_bottom_resumes_follow() {
        let mut s = ScrollState::at_bottom();
        s.on_content_changed(100, 20); // offset 80, following
        s.scroll_by(-5, 100, 20); // offset 75, not following
        assert!(!s.follow_bottom);
        s.scroll_by(5, 100, 20); // back to 80 -> following again
        assert!(s.follow_bottom);
    }

    #[test]
    fn reasoning_deltas_accumulate_into_one_block() {
        use astra_proto::astra::engine::v1::{agent_event, ReasoningEvent};

        let mut app = App::new(vec![]);
        // Three deltas of one block (same start_ms), the last carrying end_ms.
        for (i, (text, start, end)) in [
            ("Let ", Some(1000i64), None),
            ("me ", Some(1000), None),
            ("think", Some(1000), Some(1200)),
        ]
        .iter()
        .enumerate()
        {
            let _ = i;
            app.apply_agent_event(&ev(agent_event::Kind::Reasoning(ReasoningEvent {
                text: (*text).to_string(),
                seq: 0,
                start_ms: *start,
                end_ms: *end,
            })));
        }

        // One accumulated block with the full text and the end bound.
        assert_eq!(app.items.len(), 1);
        match &app.items[0] {
            Item::Reasoning {
                text,
                start_ms,
                end_ms,
                ..
            } => {
                assert_eq!(text, "Let me think");
                assert_eq!(*start_ms, Some(1000));
                assert_eq!(*end_ms, Some(1200));
            }
            other => panic!("expected a reasoning block, got {other:?}"),
        }
    }

    #[test]
    fn tool_items_carry_a_stable_daemon_id() {
        use astra_proto::astra::engine::v1::{agent_event, ToolCallEvent};
        let mut app = App::new(vec![]);
        app.apply_agent_event(&ev(agent_event::Kind::ToolCall(ToolCallEvent {
            id: "tc-1".into(),
            name: "Read".into(),
            input: "{}".into(),
        })));
        assert!(matches!(
            app.items.first(),
            Some(Item::ToolCall { id, .. }) if id == "tc-1"
        ));
    }

    #[test]
    fn connection_status_populates_model_context_and_mcp() {
        use astra_proto::astra::engine::v1::{
            agent_event, chat_event, ChatEvent, ConnectionStatusEvent, McpServerStatus, UsageEvent,
        };
        let mut app = App::new(vec![]);

        let usage = ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::AgentEvent(
                astra_proto::astra::engine::v1::AgentEventEnvelope {
                    protocol_version: 1.3,
                    event: Some(ev(agent_event::Kind::Usage(UsageEvent {
                        input_tokens: 10,
                        output_tokens: 5,
                        cost_usd: Some(0.1),
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        model: Some("gpt-4o".into()),
                        context_limit: Some(128_000),
                    }))),
                },
            )),
        };
        app.apply_chat_event(&usage);
        assert_eq!(app.ui.model.as_deref(), Some("gpt-4o"));
        assert_eq!(app.ui.context_limit, Some(128_000));

        let status = ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::ConnectionStatusEvent(
                ConnectionStatusEvent {
                    lsp_enabled: false,
                    mcp_servers: vec![McpServerStatus {
                        name: "srv".into(),
                        status: "connected".into(),
                    }],
                },
            )),
        };
        app.apply_chat_event(&status);
        assert!(!app.ui.lsp_enabled);
        assert_eq!(
            app.ui.mcp,
            vec![("srv".to_string(), "connected".to_string())]
        );
    }
}
