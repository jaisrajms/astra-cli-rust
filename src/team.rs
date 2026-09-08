//! `astra team` — teams of fleet tasks (FleetService team verbs).

use clap::{Args, Subcommand};
use tonic::transport::Channel;

use astra_proto::astra::engine::v1::fleet_service_client::FleetServiceClient;
use astra_proto::astra::engine::v1::{TeamCreateRequest, TeamListRequest};

#[derive(Args)]
pub struct TeamArgs {
    #[command(subcommand)]
    pub command: TeamCommand,
}

#[derive(Subcommand)]
pub enum TeamCommand {
    /// Create a team (one objective per member task).
    Create(CreateArgs),
    /// List teams.
    List,
}

#[derive(Args)]
pub struct CreateArgs {
    /// Member-task objectives (repeatable).
    #[arg(value_name = "PROMPT", required = true, num_args = 1..)]
    pub prompts: Vec<String>,
}

pub async fn handle(args: TeamArgs, channel: Channel) -> anyhow::Result<()> {
    match args.command {
        TeamCommand::Create(a) => create(a, channel).await,
        TeamCommand::List => list(channel).await,
    }
}

async fn create(args: CreateArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = FleetServiceClient::new(channel);
    let resp = client
        .team_create(TeamCreateRequest {
            prompts: args.prompts,
        })
        .await?
        .into_inner();

    if let Some(team) = resp.team {
        println!(
            "Created team {} (status: {}, {} member(s))",
            team.id,
            team.status,
            team.task_ids.len()
        );
    }
    Ok(())
}

async fn list(channel: Channel) -> anyhow::Result<()> {
    let mut client = FleetServiceClient::new(channel);
    let resp = client.team_list(TeamListRequest {}).await?.into_inner();

    let rows: Vec<Vec<String>> = resp
        .teams
        .iter()
        .map(|t| {
            vec![
                t.id.clone(),
                t.status.clone(),
                t.task_ids.len().to_string(),
                t.created_at.to_string(),
            ]
        })
        .collect();
    crate::render::table(&["ID", "Status", "Members", "Created"], &rows);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::fleet_service_server::{
        FleetService, FleetServiceServer,
    };
    use astra_proto::astra::engine::v1::{
        CronCreateRequest, CronCreateResponse, CronDeleteRequest, CronDeleteResponse,
        CronListRequest, CronListResponse, ListChildrenRequest, ListChildrenResponse, Team,
        TeamCreateResponse, TeamListResponse, TaskCreateRequest, TaskCreateResponse,
        TaskListRequest, TaskListResponse, TaskOutputRequest, TaskOutputResponse,
        TaskStopRequest, TaskStopResponse,
    };
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status};

    struct MockFleetService {
        created: Arc<Mutex<Option<TeamCreateRequest>>>,
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
            request: Request<TeamCreateRequest>,
        ) -> Result<Response<TeamCreateResponse>, Status> {
            let req = request.into_inner();
            let ids = (1..=req.prompts.len())
                .map(|i| format!("t-{i}"))
                .collect();
            *self.created.lock().unwrap() = Some(req);
            Ok(Response::new(TeamCreateResponse {
                team: Some(Team {
                    id: "team-1".into(),
                    task_ids: ids,
                    status: "running".into(),
                    created_at: 0,
                    updated_at: 0,
                }),
            }))
        }

        async fn team_list(
            &self,
            _: Request<TeamListRequest>,
        ) -> Result<Response<TeamListResponse>, Status> {
            Ok(Response::new(TeamListResponse {
                teams: vec![Team {
                    id: "team-1".into(),
                    task_ids: vec!["t-1".into()],
                    status: "running".into(),
                    created_at: 0,
                    updated_at: 0,
                }],
            }))
        }

        async fn cron_create(
            &self,
            _: Request<CronCreateRequest>,
        ) -> Result<Response<CronCreateResponse>, Status> {
            Err(Status::unimplemented("cron_create"))
        }

        async fn cron_list(
            &self,
            _: Request<CronListRequest>,
        ) -> Result<Response<CronListResponse>, Status> {
            Err(Status::unimplemented("cron_list"))
        }

        async fn cron_delete(
            &self,
            _: Request<CronDeleteRequest>,
        ) -> Result<Response<CronDeleteResponse>, Status> {
            Err(Status::unimplemented("cron_delete"))
        }

        async fn list_children(
            &self,
            _: Request<ListChildrenRequest>,
        ) -> Result<Response<ListChildrenResponse>, Status> {
            Err(Status::unimplemented("list_children"))
        }
    }

    #[tokio::test]
    async fn team_create_round_trips_request_body() {
        let created = Arc::new(Mutex::new(None));
        let mock = MockFleetService {
            created: created.clone(),
        };
        let addr = crate::testutil::spawn(FleetServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            TeamArgs {
                command: TeamCommand::Create(CreateArgs {
                    prompts: vec!["build api".into(), "write tests".into()],
                }),
            },
            channel,
        )
        .await
        .expect("team create should succeed");

        let captured = created.lock().unwrap().take().expect("create captured");
        assert_eq!(captured.prompts, vec!["build api", "write tests"]);
    }
}
