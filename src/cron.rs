//! `astra cron` — cron schedules (FleetService cron verbs).

use clap::{Args, Subcommand};
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;

use astra_proto::astra::engine::v1::fleet_service_client::FleetServiceClient;
use astra_proto::astra::engine::v1::{CronCreateRequest, CronDeleteRequest, CronListRequest};

#[derive(Args)]
pub struct CronArgs {
    #[command(subcommand)]
    pub command: CronCommand,
}

#[derive(Subcommand)]
pub enum CronCommand {
    /// Create a cron schedule.
    Create(CreateArgs),
    /// List cron schedules.
    List,
    /// Delete a cron schedule.
    Delete(DeleteArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    /// Schedule: `@every 30m`, `@daily`, or a 5-field cron expr.
    #[arg(long, value_name = "SCHEDULE")]
    pub schedule: String,
    /// Prompt the schedule fires as a fleet task.
    #[arg(long, value_name = "PROMPT")]
    pub prompt: String,
}

#[derive(Args)]
pub struct DeleteArgs {
    /// Cron entry id.
    #[arg(value_name = "ID")]
    pub id: String,
}

pub async fn handle(args: CronArgs, channel: Channel) -> anyhow::Result<()> {
    match args.command {
        CronCommand::Create(a) => create(a, channel).await,
        CronCommand::List => list(channel).await,
        CronCommand::Delete(a) => delete(a, channel).await,
    }
}

async fn create(args: CreateArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = FleetServiceClient::new(channel);
    let resp = client
        .cron_create(with_workspace(Request::new(CronCreateRequest {
            schedule: args.schedule,
            prompt: args.prompt,
        })))
        .await?
        .into_inner();

    if let Some(entry) = resp.entry {
        println!(
            "Created cron {} (schedule: {}, enabled: {})",
            entry.id, entry.schedule, entry.enabled
        );
    }
    Ok(())
}

async fn list(channel: Channel) -> anyhow::Result<()> {
    let mut client = FleetServiceClient::new(channel);
    let resp = client
        .cron_list(with_workspace(Request::new(CronListRequest {})))
        .await?
        .into_inner();

    let rows: Vec<Vec<String>> = resp
        .entries
        .iter()
        .map(|e| {
            vec![
                e.id.clone(),
                e.schedule.clone(),
                e.prompt.clone(),
                e.enabled.to_string(),
                e.run_count.to_string(),
            ]
        })
        .collect();
    crate::render::table(&["ID", "Schedule", "Prompt", "Enabled", "Runs"], &rows);
    Ok(())
}

async fn delete(args: DeleteArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = FleetServiceClient::new(channel);
    client
        .cron_delete(with_workspace(Request::new(CronDeleteRequest {
            id: args.id.clone(),
        })))
        .await?;
    println!("Deleted cron {}", args.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::fleet_service_server::{FleetService, FleetServiceServer};
    use astra_proto::astra::engine::v1::{
        CronCreateResponse, CronDeleteResponse, CronEntry, CronListResponse, ListChildrenRequest,
        ListChildrenResponse, TaskCreateRequest, TaskCreateResponse, TaskListRequest,
        TaskListResponse, TaskOutputRequest, TaskOutputResponse, TaskStopRequest, TaskStopResponse,
        TeamCreateRequest, TeamCreateResponse, TeamListRequest, TeamListResponse,
    };
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status};

    struct MockFleetService {
        created: Arc<Mutex<Option<CronCreateRequest>>>,
        deleted: Arc<Mutex<Option<CronDeleteRequest>>>,
    }

    #[tonic::async_trait]
    impl FleetService for MockFleetService {
        async fn task_create(
            &self,
            _: Request<TaskCreateRequest>,
        ) -> Result<Response<TaskCreateResponse>, Status> {
            Err(Status::unimplemented("task_create"))
        }

        async fn task_list(
            &self,
            _: Request<TaskListRequest>,
        ) -> Result<Response<TaskListResponse>, Status> {
            Err(Status::unimplemented("task_list"))
        }

        async fn task_output(
            &self,
            _: Request<TaskOutputRequest>,
        ) -> Result<Response<TaskOutputResponse>, Status> {
            Err(Status::unimplemented("task_output"))
        }

        async fn task_stop(
            &self,
            _: Request<TaskStopRequest>,
        ) -> Result<Response<TaskStopResponse>, Status> {
            Err(Status::unimplemented("task_stop"))
        }

        async fn team_create(
            &self,
            _: Request<TeamCreateRequest>,
        ) -> Result<Response<TeamCreateResponse>, Status> {
            Err(Status::unimplemented("team_create"))
        }

        async fn team_list(
            &self,
            _: Request<TeamListRequest>,
        ) -> Result<Response<TeamListResponse>, Status> {
            Err(Status::unimplemented("team_list"))
        }

        async fn cron_create(
            &self,
            request: Request<CronCreateRequest>,
        ) -> Result<Response<CronCreateResponse>, Status> {
            let req = request.into_inner();
            *self.created.lock().unwrap() = Some(req.clone());
            Ok(Response::new(CronCreateResponse {
                entry: Some(CronEntry {
                    id: "cron-1".into(),
                    schedule: req.schedule,
                    prompt: req.prompt,
                    enabled: true,
                    created_at: 0,
                    last_run_at: None,
                    run_count: 0,
                }),
            }))
        }

        async fn cron_list(
            &self,
            _: Request<CronListRequest>,
        ) -> Result<Response<CronListResponse>, Status> {
            Ok(Response::new(CronListResponse {
                entries: vec![CronEntry {
                    id: "cron-1".into(),
                    schedule: "@daily".into(),
                    prompt: "triage".into(),
                    enabled: true,
                    created_at: 0,
                    last_run_at: None,
                    run_count: 0,
                }],
            }))
        }

        async fn cron_delete(
            &self,
            request: Request<CronDeleteRequest>,
        ) -> Result<Response<CronDeleteResponse>, Status> {
            *self.deleted.lock().unwrap() = Some(request.into_inner());
            Ok(Response::new(CronDeleteResponse {}))
        }

        async fn list_children(
            &self,
            _: Request<ListChildrenRequest>,
        ) -> Result<Response<ListChildrenResponse>, Status> {
            Err(Status::unimplemented("list_children"))
        }
    }

    #[tokio::test]
    async fn cron_create_round_trips_request_body() {
        let created = Arc::new(Mutex::new(None));
        let mock = MockFleetService {
            created: created.clone(),
            deleted: Arc::new(Mutex::new(None)),
        };
        let addr = crate::testutil::spawn(FleetServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            CronArgs {
                command: CronCommand::Create(CreateArgs {
                    schedule: "@every 30m".into(),
                    prompt: "run triage".into(),
                }),
            },
            channel,
        )
        .await
        .expect("cron create should succeed");

        let captured = created.lock().unwrap().take().expect("create captured");
        assert_eq!(captured.schedule, "@every 30m");
        assert_eq!(captured.prompt, "run triage");
    }

    #[tokio::test]
    async fn cron_delete_forwards_id() {
        let deleted = Arc::new(Mutex::new(None));
        let mock = MockFleetService {
            created: Arc::new(Mutex::new(None)),
            deleted: deleted.clone(),
        };
        let addr = crate::testutil::spawn(FleetServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            CronArgs {
                command: CronCommand::Delete(DeleteArgs {
                    id: "cron-9".into(),
                }),
            },
            channel,
        )
        .await
        .expect("cron delete should succeed");

        let captured = deleted.lock().unwrap().take().expect("delete captured");
        assert_eq!(captured.id, "cron-9");
    }
}
