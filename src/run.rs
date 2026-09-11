//! `astra run [message..]` — one-shot message over the chat stream.
//!
//! Output parity with the reference CLI's `run`: a single `> {agent} · {model}` header on
//! stderr, the assistant's text on stdout, `Thinking:` reasoning, inline tool
//! calls on stderr, and no `[usage]`/`[tool]` marker noise.

use std::io::{IsTerminal, Write};

use anyhow::Context;
use clap::Args;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;
use crate::tool::{summarize_input, tool_icon};

use astra_proto::astra::engine::v1::chat_service_client::ChatServiceClient;
use astra_proto::astra::engine::v1::session_service_client::SessionServiceClient;
use astra_proto::astra::engine::v1::{
    agent_event, chat_client_msg, chat_event, AgentEvent, ChatClientMsg, ChatEvent,
    ForkSessionRequest, ListSessionsRequest, ResolveAskUser, ResolveDiffReview,
    ResolveToolPermission, SendMessage,
};
use astra_proto::{MessageId, SessionId};

#[derive(Args)]
pub struct RunArgs {
    /// Message to send (joined with spaces; falls back to piped stdin).
    #[arg(value_name = "MESSAGE", num_args = 0..)]
    pub message: Vec<String>,
    /// Model alias in `provider/model` form.
    #[arg(long, short = 'm', value_name = "PROVIDER/MODEL")]
    pub model: Option<String>,
    /// Active agent/role name.
    #[arg(long, value_name = "AGENT")]
    pub agent: Option<String>,
    /// Continue the most recent session.
    #[arg(long, short = 'c')]
    pub r#continue: bool,
    /// Session id to continue.
    #[arg(long, short = 's', value_name = "SESSION_ID")]
    pub session: Option<String>,
    /// Fork the session before continuing (requires --continue or --session).
    #[arg(long)]
    pub fork: bool,
    /// Execution target: `local` | `split` | `container`.
    #[arg(long, value_name = "EXEC")]
    pub exec: Option<String>,
    /// Output format: `default` | `json`.
    #[arg(long, value_name = "FORMAT", default_value = "default")]
    pub format: String,
    /// Show thinking blocks.
    #[arg(long)]
    pub thinking: bool,
    /// Session title.
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,
    /// Model reasoning variant.
    #[arg(long, value_name = "VARIANT")]
    pub variant: Option<String>,
    /// Auto-approve permissions (dangerous).
    #[arg(long)]
    pub auto: bool,
    /// Alias for --auto (hidden).
    #[arg(long, hide = true)]
    pub yolo: bool,
}

/// Renderer configuration for `run` output.
pub struct RenderConfig {
    pub agent: String,
    pub model: Option<String>,
    pub thinking: bool,
    pub json: bool,
    pub auto: bool,
}

pub async fn handle(args: RunArgs, channel: Channel) -> anyhow::Result<()> {
    let message = resolve_message(&args).await?;
    let session_id = resolve_session(&args, channel.clone()).await?;

    let mut chat = ChatServiceClient::new(channel);
    let (sink, outbound_rx) = mpsc::channel::<ChatClientMsg>(8);
    let msg = ChatClientMsg {
        session_id: session_id.map(|v| SessionId { value: v }),
        payload: Some(chat_client_msg::Payload::SendMessage(SendMessage {
            content: message,
            agent: args.agent.clone().unwrap_or_else(|| "build".to_string()),
            model: args.model.clone(),
            images: Vec::new(),
            exec: args.exec.clone(),
        })),
    };
    sink.send(msg)
        .await
        .map_err(|_| anyhow::anyhow!("chat stream closed"))?;

    let outbound = ReceiverStream::new(outbound_rx);
    let resp = chat
        .stream_chat(with_workspace(Request::new(outbound)))
        .await?;
    let stream = resp.into_inner();

    let config = RenderConfig {
        agent: args.agent.unwrap_or_else(|| "build".to_string()),
        model: args.model.clone(),
        thinking: args.thinking,
        json: args.format == "json",
        auto: args.auto || args.yolo,
    };
    consume_stream(stream, std::io::stdout(), std::io::stderr(), config, sink).await
}

/// Consume the chat stream, rendering each event and auto-resolving any interactive request
/// (permission / ask-user) — allow with `--auto`, reject otherwise (non-interactive `run`).
async fn consume_stream<S, W, E>(
    mut stream: S,
    out: W,
    err: E,
    config: RenderConfig,
    sink: mpsc::Sender<ChatClientMsg>,
) -> anyhow::Result<()>
where
    S: futures::Stream<Item = Result<ChatEvent, tonic::Status>> + Unpin,
    W: Write,
    E: Write,
{
    use futures::StreamExt;
    let mut renderer = Renderer {
        out,
        err,
        config,
        header_printed: false,
        text_buf: String::new(),
    };
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(resolver) = auto_resolve(&event, renderer.config.auto) {
            sink.send(resolver)
                .await
                .map_err(|_| anyhow::anyhow!("chat stream closed"))?;
        }
        let is_done = done(&event);
        renderer.render(&event);
        if is_done {
            break;
        }
    }
    Ok(())
}

/// Build the resolver verb for an interactive request (permission / ask-user), auto-rejecting (or
/// auto-allowing with `auto`) in non-interactive mode.
fn auto_resolve(event: &ChatEvent, auto: bool) -> Option<ChatClientMsg> {
    match &event.payload {
        Some(chat_event::Payload::ToolPermissionRequest(r)) => Some(ChatClientMsg {
            session_id: event.session_id.clone(),
            payload: Some(chat_client_msg::Payload::ResolveToolPermission(
                ResolveToolPermission {
                    id: r.id.clone(),
                    allow: auto,
                    persist: String::new(),
                },
            )),
        }),
        Some(chat_event::Payload::AskUserRequest(r)) => Some(ChatClientMsg {
            session_id: event.session_id.clone(),
            payload: Some(chat_client_msg::Payload::ResolveAskUser(ResolveAskUser {
                id: r.id.clone(),
                answer: String::new(),
            })),
        }),
        Some(chat_event::Payload::DiffReviewRequest(r)) => Some(ChatClientMsg {
            session_id: event.session_id.clone(),
            payload: Some(chat_client_msg::Payload::ResolveDiffReview(
                ResolveDiffReview {
                    id: r.id.clone(),
                    verdict: if auto {
                        "accept".to_string()
                    } else {
                        "reject".to_string()
                    },
                    new_content: None,
                },
            )),
        }),
        _ => None,
    }
}

/// The stateful `run` renderer: header-once, text→stdout, tool/thinking→stderr.
struct Renderer<W, E> {
    out: W,
    err: E,
    config: RenderConfig,
    header_printed: bool,
    /// Accumulated streaming text deltas, flushed (trimmed) at a part boundary.
    text_buf: String,
}

impl<W: Write, E: Write> Renderer<W, E> {
    fn render(&mut self, event: &ChatEvent) {
        match &event.payload {
            Some(chat_event::Payload::AgentEvent(env)) => {
                if !crate::protocol::compatible(env.protocol_version) {
                    let _ = writeln!(
                        self.err,
                        "[notice] skipping agent event with incompatible protocol version {}",
                        env.protocol_version
                    );
                    return;
                }
                if let Some(inner) = &env.event {
                    self.render_agent(inner);
                }
            }
            Some(chat_event::Payload::SessionError(e)) => {
                self.flush_text();
                let _ = writeln!(self.err, "[session error] {}", e.message);
            }
            Some(chat_event::Payload::TitleChanged(t)) => {
                if self.config.json {
                    let _ = writeln!(
                        self.out,
                        "{}",
                        serde_json::json!({
                            "type": "title",
                            "timestamp": now_ms(),
                            "title": t.title,
                        })
                    );
                }
            }
            _ => {}
        }
    }

    fn header(&mut self) {
        if self.header_printed {
            return;
        }
        self.header_printed = true;
        let line = match &self.config.model {
            Some(m) => format!("> {} · {}", self.config.agent, m),
            None => format!("> {}", self.config.agent),
        };
        let _ = writeln!(self.err);
        let _ = writeln!(self.err, "{line}");
        let _ = writeln!(self.err);
    }

    /// Flush the accumulated text as one finalized part (trimmed), matching the reference CLI's
    /// finalized-text-part output.
    fn flush_text(&mut self) {
        let text = self.text_buf.trim().to_string();
        self.text_buf.clear();
        if text.is_empty() {
            return;
        }
        if self.config.json {
            self.emit("text", serde_json::json!({ "part": { "text": text } }));
        } else {
            self.header();
            let _ = writeln!(self.out, "{text}");
        }
    }

    fn render_agent(&mut self, event: &AgentEvent) {
        match &event.kind {
            Some(agent_event::Kind::Text(t)) => {
                self.text_buf.push_str(&t.text);
            }
            Some(agent_event::Kind::Reasoning(r)) => {
                if !self.config.thinking {
                    return;
                }
                let text = r.text.trim();
                if text.is_empty() {
                    return;
                }
                if self.config.json {
                    self.emit("reasoning", serde_json::json!({ "part": { "text": text } }));
                } else {
                    self.header();
                    let _ = writeln!(self.err, "Thinking: {text}");
                }
            }
            Some(agent_event::Kind::ToolCall(t)) => {
                self.flush_text();
                if self.config.json {
                    self.emit(
                        "tool_use",
                        serde_json::json!({ "part": { "tool": t.name, "input": t.input } }),
                    );
                } else {
                    self.header();
                    let args = summarize_input(&t.input);
                    let _ = writeln!(self.err, "{} {}{}", tool_icon(&t.name), t.name, args);
                }
            }
            Some(agent_event::Kind::ToolResult(t)) => {
                // the reference CLI's `run` is quiet on success; only failures are surfaced.
                if t.is_error {
                    let _ = writeln!(self.err, "✗ {} failed", t.name);
                    if !t.summary.is_empty() {
                        let _ = writeln!(self.err, "Error: {}", t.summary);
                    }
                }
            }
            Some(agent_event::Kind::Error(e)) => {
                self.flush_text();
                if self.config.json {
                    self.emit("error", serde_json::json!({ "error": e.message }));
                } else {
                    let _ = writeln!(self.err, "Error: {}", e.message);
                }
            }
            Some(agent_event::Kind::Notice(n)) => {
                let _ = writeln!(self.err, "{}", n.text);
            }
            Some(agent_event::Kind::Done(_)) => {
                self.flush_text();
            }
            Some(agent_event::Kind::Usage(_)) | None => {}
        }
    }

    /// Emit one JSON event line (the reference CLI's `--format json` shape).
    fn emit(&mut self, ty: &str, data: serde_json::Value) {
        let mut obj = data;
        if let Some(map) = obj.as_object_mut() {
            map.insert("type".to_string(), serde_json::json!(ty));
            map.insert("timestamp".to_string(), serde_json::json!(now_ms()));
        }
        let _ = writeln!(self.out, "{obj}");
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

async fn resolve_message(args: &RunArgs) -> anyhow::Result<String> {
    if !args.message.is_empty() {
        return Ok(args.message.join(" "));
    }

    if std::io::stdin().is_terminal() {
        anyhow::bail!("no message provided and stdin is a terminal");
    }

    let mut buf = String::new();
    use std::io::Read;
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("failed to read message from stdin")?;
    Ok(buf.trim_end().to_string())
}

async fn resolve_session(args: &RunArgs, channel: Channel) -> anyhow::Result<Option<String>> {
    if args.fork && !args.r#continue && args.session.is_none() {
        anyhow::bail!("--fork requires --continue or --session");
    }

    if let Some(id) = &args.session {
        if args.fork {
            return Ok(Some(fork_session(id, channel).await?));
        }
        return Ok(Some(id.clone()));
    }

    if args.r#continue {
        let id = most_recent_root_session(channel.clone()).await?;
        if args.fork {
            return Ok(Some(fork_session(&id, channel).await?));
        }
        return Ok(Some(id));
    }

    // Fresh session: an absent session id lets the daemon create the session and return its id.
    Ok(None)
}

async fn fork_session(id: &str, channel: Channel) -> anyhow::Result<String> {
    let mut client = SessionServiceClient::new(channel);
    let resp = client
        .fork_session(with_workspace(Request::new(ForkSessionRequest {
            session_id: Some(SessionId {
                value: id.to_string(),
            }),
            message_id: Some(MessageId {
                value: String::new(),
            }),
        })))
        .await?
        .into_inner();

    resp.session
        .and_then(|s| s.session_id.map(|sid| sid.value))
        .context("fork returned no session")
}

async fn most_recent_root_session(channel: Channel) -> anyhow::Result<String> {
    let mut client = SessionServiceClient::new(channel);
    let resp = client
        .list_sessions(with_workspace(Request::new(ListSessionsRequest {
            workspace_id: None,
            limit: None,
        })))
        .await?
        .into_inner();

    let mut sessions: Vec<_> = resp
        .sessions
        .into_iter()
        .filter(|s| s.parent_id.is_none())
        .collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity));

    sessions
        .into_iter()
        .filter_map(|s| s.session_id.map(|sid| sid.value))
        .next()
        .context("no sessions found to continue")
}

fn done(event: &ChatEvent) -> bool {
    if let Some(chat_event::Payload::AgentEvent(env)) = &event.payload {
        if !crate::protocol::compatible(env.protocol_version) {
            return false;
        }
        if let Some(inner) = &env.event {
            return matches!(inner.kind, Some(agent_event::Kind::Done(_)));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::chat_service_server::{ChatService, ChatServiceServer};
    use astra_proto::astra::engine::v1::{
        agent_event, chat_event, AgentEventEnvelope, ChatEvent, DoneEvent, TextEvent, UsageEvent,
    };
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tonic::{Response, Status, Streaming};

    fn config() -> RenderConfig {
        RenderConfig {
            agent: "build".to_string(),
            model: Some("deepseek/deepseek-chat".to_string()),
            thinking: false,
            json: false,
            auto: false,
        }
    }

    fn sink() -> mpsc::Sender<ChatClientMsg> {
        let (tx, _rx) = mpsc::channel(4);
        tx
    }

    #[derive(Clone)]
    struct MockChatService {
        received: Arc<Mutex<Vec<SendMessage>>>,
    }

    fn text_event(text: &str) -> ChatEvent {
        ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::AgentEvent(AgentEventEnvelope {
                protocol_version: 1.3,
                event: Some(AgentEvent {
                    kind: Some(agent_event::Kind::Text(TextEvent { text: text.into() })),
                }),
            })),
        }
    }

    fn done_event(result: Option<&str>) -> ChatEvent {
        ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::AgentEvent(AgentEventEnvelope {
                protocol_version: 1.3,
                event: Some(AgentEvent {
                    kind: Some(agent_event::Kind::Done(DoneEvent {
                        result: result.map(str::to_string),
                        num_turns: Some(1),
                    })),
                }),
            })),
        }
    }

    fn usage_event() -> ChatEvent {
        ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::AgentEvent(AgentEventEnvelope {
                protocol_version: 1.3,
                event: Some(AgentEvent {
                    kind: Some(agent_event::Kind::Usage(UsageEvent {
                        input_tokens: 10,
                        output_tokens: 5,
                        cost_usd: None,
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        model: None,
                        context_limit: None,
                    })),
                }),
            })),
        }
    }

    #[tonic::async_trait]
    impl ChatService for MockChatService {
        type StreamChatStream =
            Pin<Box<dyn futures::Stream<Item = Result<ChatEvent, Status>> + Send>>;

        async fn stream_chat(
            &self,
            request: tonic::Request<Streaming<ChatClientMsg>>,
        ) -> Result<Response<Self::StreamChatStream>, Status> {
            // Read the FIRST inbound message (the prompt) and reply; the client keeps the bidi
            // stream open to resolve interactive requests, so do NOT drain to EOF here (that would
            // deadlock the handle loop).
            let mut inbound = request.into_inner();
            if let Some(msg) = inbound.message().await? {
                if let Some(chat_client_msg::Payload::SendMessage(sm)) = msg.payload {
                    self.received.lock().unwrap().push(sm);
                }
            }

            let stream = futures::stream::iter(vec![
                Ok(text_event("hello ")),
                Ok(text_event("world")),
                Ok(done_event(None)),
            ]);
            Ok(Response::new(Box::pin(stream)))
        }
    }

    #[tokio::test]
    async fn run_sends_message_and_consumes_stream() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let mock = MockChatService {
            received: received.clone(),
        };
        let addr = crate::testutil::spawn(ChatServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            RunArgs {
                message: vec!["hello".into(), "world".into()],
                model: None,
                agent: Some("plan".into()),
                r#continue: false,
                session: None,
                fork: false,
                exec: None,
                format: "default".into(),
                thinking: false,
                title: None,
                variant: None,
                auto: false,
                yolo: false,
            },
            channel,
        )
        .await
        .expect("run should complete");

        let msgs = received.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello world");
        assert_eq!(msgs[0].agent, "plan");
    }

    #[tokio::test]
    async fn run_joins_message_and_defaults_agent() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let mock = MockChatService {
            received: received.clone(),
        };
        let addr = crate::testutil::spawn(ChatServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            RunArgs {
                message: vec!["just".into(), "one".into()],
                model: None,
                agent: None,
                r#continue: false,
                session: None,
                fork: false,
                exec: None,
                format: "default".into(),
                thinking: false,
                title: None,
                variant: None,
                auto: false,
                yolo: false,
            },
            channel,
        )
        .await
        .expect("run should complete");

        let msgs = received.lock().unwrap();
        assert_eq!(msgs[0].content, "just one");
        assert_eq!(msgs[0].agent, "build");
    }

    #[tokio::test]
    async fn text_is_clean_with_header_and_no_usage_noise() {
        let stream = futures::stream::iter(vec![
            Ok::<_, Status>(usage_event()),
            Ok(text_event("hi ")),
            Ok(text_event("there")),
            Ok(done_event(None)),
        ]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        consume_stream(stream, &mut out, &mut err, config(), sink())
            .await
            .expect("stream should consume");

        let out_text = String::from_utf8(out).unwrap();
        // Streaming text deltas accumulate into one trimmed part on stdout; no [usage] markers.
        assert_eq!(out_text, "hi there\n", "unexpected stdout: {out_text}");
        // Header is on stderr once.
        let err_text = String::from_utf8(err).unwrap();
        assert!(
            err_text.contains("> build · deepseek/deepseek-chat"),
            "header missing: {err_text}"
        );
        assert_eq!(
            err_text.matches("> build ·").count(),
            1,
            "header should print once: {err_text}"
        );
    }

    #[tokio::test]
    async fn done_with_result_is_not_duplicated() {
        let stream = futures::stream::iter(vec![
            Ok::<_, Status>(text_event("hi")),
            Ok(done_event(Some("final answer"))),
        ]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        consume_stream(stream, &mut out, &mut err, config(), sink())
            .await
            .expect("stream should consume");
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("hi"), "text rendered: {text}");
        // The Done result is NOT re-printed (the reference CLI prints text parts, not a `result`).
        assert!(
            !text.contains("final answer"),
            "done result not duplicated: {text}"
        );
    }

    #[tokio::test]
    async fn incompatible_protocol_version_is_skipped_with_notice() {
        let bad = ChatEvent {
            session_id: None,
            payload: Some(chat_event::Payload::AgentEvent(AgentEventEnvelope {
                protocol_version: 2.0,
                event: Some(AgentEvent {
                    kind: Some(agent_event::Kind::Text(TextEvent {
                        text: "nope".into(),
                    })),
                }),
            })),
        };
        let stream = futures::stream::iter(vec![Ok::<_, Status>(bad)]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        consume_stream(stream, &mut out, &mut err, config(), sink())
            .await
            .expect("stream should consume");
        assert!(out.is_empty(), "incompatible event must not be rendered");
        let err_text = String::from_utf8(err).unwrap();
        assert!(
            err_text.contains("incompatible protocol version"),
            "notice emitted: {err_text}"
        );
    }

    #[tokio::test]
    async fn fork_without_session_or_continue_bails() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let mock = MockChatService { received };
        let addr = crate::testutil::spawn(ChatServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        let err = handle(
            RunArgs {
                message: vec!["hi".into()],
                model: None,
                agent: None,
                r#continue: false,
                session: None,
                fork: true,
                exec: None,
                format: "default".into(),
                thinking: false,
                title: None,
                variant: None,
                auto: false,
                yolo: false,
            },
            channel,
        )
        .await
        .expect_err("fork without session/continue must fail");
        assert!(
            format!("{err:#}").contains("--fork requires"),
            "error should mention the requirement: {err:#}"
        );
    }
}
