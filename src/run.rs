//! `astra run [message..]` — one-shot message over the chat stream.

use std::io::IsTerminal;

use anyhow::Context;
use clap::Args;
use tonic::transport::Channel;
use tonic::Request;

use astra_proto::astra::engine::v1::chat_service_client::ChatServiceClient;
use astra_proto::astra::engine::v1::session_service_client::SessionServiceClient;
use astra_proto::astra::engine::v1::{
    agent_event, chat_client_msg, chat_event, ChatClientMsg, ChatEvent, ForkSessionRequest,
    ListSessionsRequest, SendMessage,
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
}

pub async fn handle(args: RunArgs, channel: Channel) -> anyhow::Result<()> {
    let message = resolve_message(&args).await?;
    let session_id = resolve_session(&args, channel.clone()).await?;

    let mut chat = ChatServiceClient::new(channel);
    let msg = ChatClientMsg {
        session_id: Some(SessionId { value: session_id }),
        payload: Some(chat_client_msg::Payload::SendMessage(SendMessage {
            content: message,
            agent: args.agent.unwrap_or_else(|| "build".to_string()),
            model: args.model,
            images: Vec::new(),
            exec: args.exec,
        })),
    };

    let outbound = futures::stream::iter(std::iter::once(msg));
    let resp = chat.stream_chat(Request::new(outbound)).await?;
    let stream = resp.into_inner();

    consume_stream(stream, std::io::stdout(), std::io::stderr()).await
}

/// Consume the chat stream, rendering each event. The terminal `Done` event is
/// rendered (so its `result` is emitted) before the loop breaks.
async fn consume_stream<S, W, E>(mut stream: S, mut out: W, mut err: E) -> anyhow::Result<()>
where
    S: futures::Stream<Item = Result<ChatEvent, tonic::Status>> + Unpin,
    W: std::io::Write,
    E: std::io::Write,
{
    use futures::StreamExt;
    while let Some(event) = stream.next().await {
        let event = event?;
        let is_done = done(&event);
        render_to(&event, &mut out, &mut err);
        if is_done {
            break;
        }
    }
    Ok(())
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

async fn resolve_session(args: &RunArgs, channel: Channel) -> anyhow::Result<String> {
    if args.fork && !args.r#continue && args.session.is_none() {
        anyhow::bail!("--fork requires --continue or --session");
    }

    if let Some(id) = &args.session {
        if args.fork {
            return fork_session(id, channel).await;
        }
        return Ok(id.clone());
    }

    if args.r#continue {
        let id = most_recent_root_session(channel.clone()).await?;
        if args.fork {
            return fork_session(&id, channel).await;
        }
        return Ok(id);
    }

    Ok(crate::ids::fresh_session_id())
}

async fn fork_session(id: &str, channel: Channel) -> anyhow::Result<String> {
    let mut client = SessionServiceClient::new(channel);
    let resp = client
        .fork_session(ForkSessionRequest {
            session_id: Some(SessionId {
                value: id.to_string(),
            }),
            message_id: Some(MessageId {
                value: String::new(),
            }),
        })
        .await?
        .into_inner();

    resp.session
        .and_then(|s| s.session_id.map(|sid| sid.value))
        .context("fork returned no session")
}

async fn most_recent_root_session(channel: Channel) -> anyhow::Result<String> {
    let mut client = SessionServiceClient::new(channel);
    let resp = client
        .list_sessions(ListSessionsRequest {
            workspace_id: None,
            limit: None,
        })
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

fn render_to(event: &ChatEvent, out: &mut impl std::io::Write, err: &mut impl std::io::Write) {
    match &event.payload {
        Some(chat_event::Payload::AgentEvent(env)) => {
            if !crate::protocol::compatible(env.protocol_version) {
                let _ = writeln!(
                    err,
                    "[notice] skipping agent event with incompatible protocol version {}",
                    env.protocol_version
                );
                return;
            }
            if let Some(inner) = &env.event {
                match &inner.kind {
                    Some(agent_event::Kind::Text(t)) => {
                        let _ = write!(out, "{}", t.text);
                    }
                    Some(agent_event::Kind::ToolCall(t)) => {
                        let _ = writeln!(out, "\n[tool] {}", t.name);
                    }
                    Some(agent_event::Kind::ToolResult(t)) => {
                        let _ = writeln!(out, "[tool result] {}: {}", t.name, t.summary);
                    }
                    Some(agent_event::Kind::Usage(u)) => {
                        let _ = writeln!(
                            out,
                            "\n[usage] in={} out={} cost={:?}",
                            u.input_tokens, u.output_tokens, u.cost_usd
                        );
                    }
                    Some(agent_event::Kind::Notice(n)) => {
                        let _ = writeln!(out, "{}", n.text);
                    }
                    Some(agent_event::Kind::Reasoning(r)) => {
                        let _ = writeln!(out, "[reasoning] {}", r.text);
                    }
                    Some(agent_event::Kind::Done(d)) => {
                        if let Some(result) = &d.result {
                            let _ = writeln!(out, "\n{result}");
                        }
                    }
                    Some(agent_event::Kind::Error(e)) => {
                        let _ = writeln!(err, "[error] {}", e.message);
                    }
                    None => {}
                }
            }
        }
        Some(chat_event::Payload::SessionError(e)) => {
            let _ = writeln!(err, "[session error] {}", e.message);
        }
        Some(chat_event::Payload::TitleChanged(t)) => {
            let _ = writeln!(out, "[title] {}", t.title);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::chat_service_server::{ChatService, ChatServiceServer};
    use astra_proto::astra::engine::v1::{
        agent_event, chat_event, AgentEvent, AgentEventEnvelope, ChatEvent, DoneEvent, TextEvent,
    };
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tonic::{Response, Status, Streaming};

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

    #[tonic::async_trait]
    impl ChatService for MockChatService {
        type StreamChatStream =
            Pin<Box<dyn futures::Stream<Item = Result<ChatEvent, Status>> + Send>>;

        async fn stream_chat(
            &self,
            request: tonic::Request<Streaming<ChatClientMsg>>,
        ) -> Result<Response<Self::StreamChatStream>, Status> {
            let mut inbound = request.into_inner();
            while let Some(msg) = inbound.message().await? {
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
            },
            channel,
        )
        .await
        .expect("run should complete");

        let msgs = received.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "just one");
        assert_eq!(msgs[0].agent, "build");
    }

    #[tokio::test]
    async fn done_with_result_is_rendered() {
        let stream = futures::stream::iter(vec![
            Ok::<_, Status>(text_event("hi")),
            Ok(done_event(Some("final answer"))),
        ]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        consume_stream(stream, &mut out, &mut err)
            .await
            .expect("stream should consume");
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("hi"), "text event rendered: {text}");
        assert!(
            text.contains("final answer"),
            "done result rendered: {text}"
        );
        assert!(err.is_empty(), "no error output expected: {err:?}");
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
        consume_stream(stream, &mut out, &mut err)
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
