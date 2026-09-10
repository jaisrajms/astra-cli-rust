//! `astra session import` — import a session from a portable payload (bytes).

use anyhow::Context;
use clap::Args;
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;

use astra_proto::astra::engine::v1::{
    session_service_client::SessionServiceClient, ImportSessionRequest,
};

#[derive(Args)]
pub struct ImportArgs {
    /// Path to the exported payload file (`-` reads from stdin).
    #[arg(value_name = "FILE")]
    pub file: String,
}

pub async fn handle(args: ImportArgs, channel: Channel) -> anyhow::Result<()> {
    let payload = read_payload(&args.file).await?;

    let mut client = SessionServiceClient::new(channel);
    let resp = client
        .import_session(with_workspace(Request::new(ImportSessionRequest {
            payload,
            workspace_id: None,
        })))
        .await?
        .into_inner();

    if let Some(s) = resp.session {
        let id = s
            .session_id
            .as_ref()
            .map(|i| i.value.as_str())
            .unwrap_or("");
        println!("Imported session {id} — \"{}\"", s.title);
    }
    Ok(())
}

async fn read_payload(file: &str) -> anyhow::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;

    if file == "-" {
        let mut buf = Vec::new();
        tokio::io::stdin()
            .read_to_end(&mut buf)
            .await
            .context("failed to read payload from stdin")?;
        return Ok(buf);
    }

    tokio::fs::read(file)
        .await
        .with_context(|| format!("failed to read {file}"))
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
        GetSessionRequest, GetSessionResponse, ImportSessionResponse, ListSessionsRequest,
        ListSessionsResponse, ResumeSessionRequest, ResumeSessionResponse, SessionSummary,
    };
    use astra_proto::SessionId;
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status};

    struct MockSessionService {
        imported: Arc<Mutex<Option<ImportSessionRequest>>>,
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
            Err(Status::unimplemented("list_sessions"))
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
            request: Request<ImportSessionRequest>,
        ) -> Result<Response<ImportSessionResponse>, Status> {
            *self.imported.lock().unwrap() = Some(request.into_inner());
            Ok(Response::new(ImportSessionResponse {
                session: Some(SessionSummary {
                    session_id: Some(SessionId {
                        value: "s-9".into(),
                    }),
                    parent_id: None,
                    title: "imported".into(),
                    agent: "build".into(),
                    archived: false,
                    last_activity: 0,
                    message_count: 0,
                    cost_usd: 0.0,
                    last_resume_cursor: None,
                    workspace_identity_digest: None,
                }),
            }))
        }
    }

    #[tokio::test]
    async fn import_round_trips_payload_from_file() {
        let imported = Arc::new(Mutex::new(None));
        let mock = MockSessionService {
            imported: imported.clone(),
        };
        let addr = crate::testutil::spawn(SessionServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        let path =
            std::env::temp_dir().join(format!("astra-import-test-{}.bin", std::process::id()));
        std::fs::write(&path, b"exported-payload").expect("write payload file");

        handle(
            ImportArgs {
                file: path.to_string_lossy().into_owned(),
            },
            channel,
        )
        .await
        .expect("import should succeed");

        std::fs::remove_file(&path).ok();

        let captured = imported.lock().unwrap().take().expect("import captured");
        assert_eq!(captured.payload, b"exported-payload");
    }
}
