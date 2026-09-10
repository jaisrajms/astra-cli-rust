//! `astra session export` — export a session to a portable payload (bytes).

use std::io::Write;

use anyhow::Context;
use clap::Args;
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;

use astra_proto::astra::engine::v1::{
    session_service_client::SessionServiceClient, ExportSessionRequest, ListSessionsRequest,
};
use astra_proto::SessionId;

#[derive(Args)]
pub struct ExportArgs {
    /// Session id to export (default: most recent).
    #[arg(value_name = "SESSION_ID")]
    pub session_id: Option<String>,
    /// Write the payload to a file instead of stdout.
    #[arg(long, short = 'o', value_name = "FILE")]
    pub output: Option<String>,
}

pub async fn handle(args: ExportArgs, channel: Channel) -> anyhow::Result<()> {
    let session_id = match args.session_id {
        Some(id) => id,
        None => most_recent_session_id(channel.clone()).await?,
    };

    let mut client = SessionServiceClient::new(channel);
    let resp = client
        .export_session(with_workspace(Request::new(ExportSessionRequest {
            session_id: Some(SessionId { value: session_id }),
        })))
        .await?
        .into_inner();

    match args.output {
        Some(path) => {
            tokio::fs::write(&path, &resp.payload)
                .await
                .with_context(|| format!("failed to write {path}"))?;
            println!("Exported session to {path}");
        }
        None => {
            std::io::stdout()
                .write_all(&resp.payload)
                .context("failed to write payload to stdout")?;
        }
    }
    Ok(())
}

async fn most_recent_session_id(channel: Channel) -> anyhow::Result<String> {
    let mut client = SessionServiceClient::new(channel);
    let resp = client
        .list_sessions(with_workspace(Request::new(ListSessionsRequest {
            workspace_id: None,
            limit: None,
        })))
        .await?
        .into_inner();

    let mut sessions = resp.sessions;
    sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity));

    sessions
        .into_iter()
        .filter_map(|s| s.session_id.map(|id| id.value))
        .next()
        .context("no sessions found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::session_service_server::{
        SessionService, SessionServiceServer,
    };
    use astra_proto::astra::engine::v1::{
        CreateSessionRequest, CreateSessionResponse, DeleteSessionRequest, DeleteSessionResponse,
        ExportSessionResponse, ForkSessionRequest, ForkSessionResponse, GetSessionRequest,
        GetSessionResponse, ImportSessionRequest, ImportSessionResponse, ListSessionsResponse,
        ResumeSessionRequest, ResumeSessionResponse, SessionSummary,
    };
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status};

    fn summary(id: &str, last_activity: i64) -> SessionSummary {
        SessionSummary {
            session_id: Some(SessionId { value: id.into() }),
            parent_id: None,
            title: format!("session {id}"),
            agent: "build".into(),
            archived: false,
            last_activity,
            message_count: 1,
            cost_usd: 0.0,
            last_resume_cursor: None,
            workspace_identity_digest: None,
        }
    }

    struct MockSessionService {
        exported: Arc<Mutex<Option<ExportSessionRequest>>>,
        sessions: Vec<SessionSummary>,
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
            _: Request<ListSessionsRequest>,
        ) -> Result<Response<ListSessionsResponse>, Status> {
            Ok(Response::new(ListSessionsResponse {
                sessions: self.sessions.clone(),
            }))
        }

        async fn export_session(
            &self,
            request: Request<ExportSessionRequest>,
        ) -> Result<Response<ExportSessionResponse>, Status> {
            *self.exported.lock().unwrap() = Some(request.into_inner());
            Ok(Response::new(ExportSessionResponse {
                payload: b"payload".to_vec(),
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

        async fn import_session(
            &self,
            _: Request<ImportSessionRequest>,
        ) -> Result<Response<ImportSessionResponse>, Status> {
            Err(Status::unimplemented("import_session"))
        }
    }

    #[tokio::test]
    async fn export_forwards_explicit_session_id() {
        let exported = Arc::new(Mutex::new(None));
        let mock = MockSessionService {
            exported: exported.clone(),
            sessions: vec![],
        };
        let addr = crate::testutil::spawn(SessionServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            ExportArgs {
                session_id: Some("s-1".into()),
                output: None,
            },
            channel,
        )
        .await
        .expect("export should succeed");

        let captured = exported.lock().unwrap().take().expect("export captured");
        assert_eq!(
            captured.session_id.map(|id| id.value).as_deref(),
            Some("s-1")
        );
    }

    #[tokio::test]
    async fn export_without_session_id_picks_most_recent_by_last_activity() {
        let exported = Arc::new(Mutex::new(None));
        let mock = MockSessionService {
            exported: exported.clone(),
            // Deliberately unsorted: `s-new` has the higher `last_activity`.
            sessions: vec![summary("s-old", 100), summary("s-new", 200)],
        };
        let addr = crate::testutil::spawn(SessionServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            ExportArgs {
                session_id: None,
                output: None,
            },
            channel,
        )
        .await
        .expect("export should succeed");

        let captured = exported.lock().unwrap().take().expect("export captured");
        assert_eq!(
            captured.session_id.map(|id| id.value).as_deref(),
            Some("s-new"),
            "most recent session (by last_activity) should be exported"
        );
    }
}
