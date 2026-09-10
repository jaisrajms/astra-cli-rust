//! `astra session` — list / fork / resume / delete / export / import.

use clap::{Args, Subcommand, ValueEnum};
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;

use astra_proto::astra::engine::v1::{
    session_service_client::SessionServiceClient, DeleteSessionRequest, ForkSessionRequest,
    ListSessionsRequest, ResumeSessionRequest, SessionSummary,
};
use astra_proto::{MessageId, SessionId};

#[derive(Args)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

#[derive(Subcommand)]
pub enum SessionCommand {
    /// List sessions.
    List(ListArgs),
    /// Fork a session up to a message boundary.
    Fork(ForkArgs),
    /// Resume a session.
    Resume(ResumeArgs),
    /// Delete a session.
    Delete(DeleteArgs),
    /// Export a session to a portable payload.
    Export(crate::export::ExportArgs),
    /// Import a session from a portable payload.
    Import(crate::import::ImportArgs),
}

#[derive(Args)]
pub struct ListArgs {
    /// Limit to the N most recent sessions.
    #[arg(long, short = 'n', value_name = "N")]
    pub max_count: Option<i32>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Table)]
    pub format: Format,
}

#[derive(ValueEnum, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Table,
    Json,
}

#[derive(Args)]
pub struct ForkArgs {
    /// Source session id.
    #[arg(value_name = "SESSION_ID")]
    pub session_id: String,
    /// Fork-up-to message boundary (empty = reproduce all).
    #[arg(long, value_name = "MESSAGE_ID")]
    pub message: Option<String>,
}

#[derive(Args)]
pub struct ResumeArgs {
    /// Session id to resume.
    #[arg(value_name = "SESSION_ID")]
    pub session_id: String,
}

#[derive(Args)]
pub struct DeleteArgs {
    /// Session id to delete.
    #[arg(value_name = "SESSION_ID")]
    pub session_id: String,
}

pub async fn handle(args: SessionArgs, channel: Channel) -> anyhow::Result<()> {
    match args.command {
        SessionCommand::List(a) => list(a, channel).await,
        SessionCommand::Fork(a) => fork(a, channel).await,
        SessionCommand::Resume(a) => resume(a, channel).await,
        SessionCommand::Delete(a) => delete(a, channel).await,
        SessionCommand::Export(a) => crate::export::handle(a, channel).await,
        SessionCommand::Import(a) => crate::import::handle(a, channel).await,
    }
}

async fn list(args: ListArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = SessionServiceClient::new(channel);
    let resp = client
        .list_sessions(with_workspace(Request::new(ListSessionsRequest {
            workspace_id: None,
            limit: args.max_count,
        })))
        .await?
        .into_inner();

    match args.format {
        Format::Json => {
            let out: Vec<serde_json::Value> = resp.sessions.iter().map(summary_json).collect();
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Format::Table => {
            let rows: Vec<Vec<String>> = resp
                .sessions
                .iter()
                .map(|s| {
                    vec![
                        s.session_id
                            .as_ref()
                            .map(|id| id.value.clone())
                            .unwrap_or_default(),
                        s.title.clone(),
                        s.agent.clone(),
                        s.message_count.to_string(),
                        format!("{:.4}", s.cost_usd),
                        s.last_activity.to_string(),
                    ]
                })
                .collect();
            crate::render::table(
                &[
                    "Session ID",
                    "Title",
                    "Agent",
                    "Msgs",
                    "Cost",
                    "Updated (ms)",
                ],
                &rows,
            );
        }
    }
    Ok(())
}

async fn fork(args: ForkArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = SessionServiceClient::new(channel);
    let resp = client
        .fork_session(with_workspace(Request::new(ForkSessionRequest {
            session_id: Some(SessionId {
                value: args.session_id,
            }),
            message_id: Some(MessageId {
                value: args.message.unwrap_or_default(),
            }),
        })))
        .await?
        .into_inner();

    if let Some(s) = resp.session {
        print_summary(&s);
    }
    Ok(())
}

async fn resume(args: ResumeArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = SessionServiceClient::new(channel);
    let resp = client
        .resume_session(with_workspace(Request::new(ResumeSessionRequest {
            session_id: Some(SessionId {
                value: args.session_id,
            }),
        })))
        .await?
        .into_inner();

    if let Some(s) = resp.session {
        print_summary(&s);
    }
    Ok(())
}

async fn delete(args: DeleteArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = SessionServiceClient::new(channel);
    client
        .delete_session(with_workspace(Request::new(DeleteSessionRequest {
            session_id: Some(SessionId {
                value: args.session_id.clone(),
            }),
        })))
        .await?;
    println!("Session {} deleted", args.session_id);
    Ok(())
}

fn print_summary(s: &SessionSummary) {
    let id = s
        .session_id
        .as_ref()
        .map(|i| i.value.as_str())
        .unwrap_or("");
    println!(
        "Session {} — \"{}\" (agent: {}, {} msgs)",
        id, s.title, s.agent, s.message_count
    );
}

fn summary_json(s: &SessionSummary) -> serde_json::Value {
    serde_json::json!({
        "id": s.session_id.as_ref().map(|i| i.value.as_str()).unwrap_or(""),
        "parent_id": s.parent_id.as_ref().map(|i| i.value.as_str()),
        "title": s.title,
        "agent": s.agent,
        "archived": s.archived,
        "last_activity": s.last_activity,
        "message_count": s.message_count,
        "cost_usd": s.cost_usd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::session_service_server::{
        SessionService, SessionServiceServer,
    };
    use astra_proto::astra::engine::v1::{
        CreateSessionRequest, CreateSessionResponse, DeleteSessionRequest, DeleteSessionResponse,
        ExportSessionRequest, ExportSessionResponse, ForkSessionRequest, ForkSessionResponse,
        GetSessionRequest, GetSessionResponse, ImportSessionRequest, ImportSessionResponse,
        ListSessionsResponse, ResumeSessionRequest, ResumeSessionResponse,
    };
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status};

    fn summary(id: &str, parent: Option<&str>) -> SessionSummary {
        SessionSummary {
            session_id: Some(SessionId { value: id.into() }),
            parent_id: parent.map(|p| SessionId { value: p.into() }),
            title: format!("session {id}"),
            agent: "build".into(),
            archived: false,
            last_activity: 1_700_000_000_000,
            message_count: 7,
            cost_usd: 0.25,
            last_resume_cursor: None,
            workspace_identity_digest: None,
        }
    }

    #[derive(Default)]
    struct MockSessionService {
        last_limit: Arc<Mutex<Option<i32>>>,
    }

    #[tonic::async_trait]
    impl SessionService for MockSessionService {
        async fn create_session(
            &self,
            _: Request<CreateSessionRequest>,
        ) -> Result<Response<CreateSessionResponse>, Status> {
            Err(Status::unimplemented("create_session"))
        }

        async fn list_sessions(
            &self,
            request: Request<ListSessionsRequest>,
        ) -> Result<Response<ListSessionsResponse>, Status> {
            *self.last_limit.lock().unwrap() = request.into_inner().limit;
            Ok(Response::new(ListSessionsResponse {
                sessions: vec![summary("s-1", None), summary("s-2", Some("s-1"))],
            }))
        }

        async fn get_session(
            &self,
            _: Request<GetSessionRequest>,
        ) -> Result<Response<GetSessionResponse>, Status> {
            Err(Status::unimplemented("get_session"))
        }

        async fn fork_session(
            &self,
            _: Request<ForkSessionRequest>,
        ) -> Result<Response<ForkSessionResponse>, Status> {
            Err(Status::unimplemented("fork_session"))
        }

        async fn resume_session(
            &self,
            _: Request<ResumeSessionRequest>,
        ) -> Result<Response<ResumeSessionResponse>, Status> {
            Err(Status::unimplemented("resume_session"))
        }

        async fn delete_session(
            &self,
            _: Request<DeleteSessionRequest>,
        ) -> Result<Response<DeleteSessionResponse>, Status> {
            Err(Status::unimplemented("delete_session"))
        }

        async fn export_session(
            &self,
            _: Request<ExportSessionRequest>,
        ) -> Result<Response<ExportSessionResponse>, Status> {
            Err(Status::unimplemented("export_session"))
        }

        async fn import_session(
            &self,
            _: Request<ImportSessionRequest>,
        ) -> Result<Response<ImportSessionResponse>, Status> {
            Err(Status::unimplemented("import_session"))
        }
    }

    #[tokio::test]
    async fn session_list_round_trips_against_mock() {
        let mock = MockSessionService::default();
        let addr = crate::testutil::spawn(SessionServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            SessionArgs {
                command: SessionCommand::List(ListArgs {
                    max_count: Some(5),
                    format: Format::Table,
                }),
            },
            channel,
        )
        .await
        .expect("session list should succeed");
    }

    #[tokio::test]
    async fn session_list_forwards_limit_to_daemon() {
        let mock = MockSessionService::default();
        let last_limit = mock.last_limit.clone();
        let addr = crate::testutil::spawn(SessionServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            SessionArgs {
                command: SessionCommand::List(ListArgs {
                    max_count: Some(10),
                    format: Format::Json,
                }),
            },
            channel,
        )
        .await
        .expect("json listing should succeed");

        assert_eq!(
            *last_limit.lock().unwrap(),
            Some(10),
            "the `limit` should be forwarded on the wire"
        );
    }
}
