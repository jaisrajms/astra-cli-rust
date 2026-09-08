//! `astra task` — background fleet tasks (FleetService task verbs).

use clap::{Args, Subcommand};
use tonic::transport::Channel;

use astra_proto::astra::engine::v1::fleet_service_client::FleetServiceClient;
use astra_proto::astra::engine::v1::{
    TaskCreateRequest, TaskListRequest, TaskOutputRequest, TaskStopRequest,
};

#[derive(Args)]
pub struct TaskArgs {
    #[command(subcommand)]
    pub command: TaskCommand,
}

#[derive(Subcommand)]
pub enum TaskCommand {
    /// Create a background task.
    Create(CreateArgs),
    /// List tasks.
    List,
    /// Fetch a task's output.
    Output(OutputArgs),
    /// Stop a task.
    Stop(StopArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    /// Task objective (a bare prompt).
    #[arg(value_name = "PROMPT")]
    pub prompt: String,
    /// Optional description.
    #[arg(long, value_name = "DESCRIPTION")]
    pub description: Option<String>,
    /// Team id to attach the task to.
    #[arg(long, value_name = "TEAM_ID")]
    pub team: Option<String>,
}

#[derive(Args)]
pub struct OutputArgs {
    /// Task id.
    #[arg(value_name = "TASK_ID")]
    pub task_id: String,
}

#[derive(Args)]
pub struct StopArgs {
    /// Task id.
    #[arg(value_name = "TASK_ID")]
    pub task_id: String,
}

pub async fn handle(args: TaskArgs, channel: Channel) -> anyhow::Result<()> {
    match args.command {
        TaskCommand::Create(a) => create(a, channel).await,
        TaskCommand::List => list(channel).await,
        TaskCommand::Output(a) => output(a, channel).await,
        TaskCommand::Stop(a) => stop(a, channel).await,
    }
}

async fn create(args: CreateArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = FleetServiceClient::new(channel);
    let resp = client
        .task_create(TaskCreateRequest {
            prompt: args.prompt,
            description: args.description,
            team_id: args.team,
        })
        .await?
        .into_inner();

    if let Some(task) = resp.task {
        println!("Created task {} (status: {})", task.id, task.status);
    }
    Ok(())
}

async fn list(channel: Channel) -> anyhow::Result<()> {
    let mut client = FleetServiceClient::new(channel);
    let resp = client.task_list(TaskListRequest {}).await?.into_inner();

    let rows: Vec<Vec<String>> = resp
        .tasks
        .iter()
        .map(|t| {
            vec![
                t.id.clone(),
                t.prompt.clone(),
                t.status.clone(),
                t.created_at.to_string(),
                t.cost_usd.map(|c| format!("{c:.4}")).unwrap_or_default(),
            ]
        })
        .collect();
    crate::render::table(&["ID", "Prompt", "Status", "Created", "Cost"], &rows);
    Ok(())
}

async fn output(args: OutputArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = FleetServiceClient::new(channel);
    let resp = client
        .task_output(TaskOutputRequest {
            task_id: args.task_id,
        })
        .await?
        .into_inner();

    if let Some(task) = resp.task {
        if let Some(output) = task.output {
            println!("{output}");
        } else if let Some(error) = task.error {
            eprintln!("[error] {error}");
        } else {
            println!("(no output yet — status: {})", task.status);
        }
    }
    Ok(())
}

async fn stop(args: StopArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = FleetServiceClient::new(channel);
    let resp = client
        .task_stop(TaskStopRequest {
            task_id: args.task_id.clone(),
        })
        .await?
        .into_inner();

    println!("Stopped task {}: {}", args.task_id, resp.stopped);
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
        CronListRequest, CronListResponse, FleetTask, ListChildrenRequest, ListChildrenResponse,
        TaskCreateResponse, TaskListResponse, TaskOutputResponse, TaskStopResponse,
        TeamCreateRequest, TeamCreateResponse, TeamListRequest, TeamListResponse,
    };
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status};

    struct MockFleetService {
        created: Arc<Mutex<Option<TaskCreateRequest>>>,
        output: Arc<Mutex<Option<TaskOutputRequest>>>,
        stopped: Arc<Mutex<Option<TaskStopRequest>>>,
    }

    #[tonic::async_trait]
    impl FleetService for MockFleetService {
        async fn task_create(
            &self,
            request: Request<TaskCreateRequest>,
        ) -> Result<Response<TaskCreateResponse>, Status> {
            let req = request.into_inner();
            *self.created.lock().unwrap() = Some(req.clone());
            Ok(Response::new(TaskCreateResponse {
                task: Some(FleetTask {
                    id: "t-1".into(),
                    prompt: req.prompt,
                    status: "created".into(),
                    created_at: 0,
                    updated_at: 0,
                    output: None,
                    error: None,
                    cost_usd: None,
                    team_id: req.team_id,
                    cron_id: None,
                }),
            }))
        }

        async fn task_list(
            &self,
            _: Request<TaskListRequest>,
        ) -> Result<Response<TaskListResponse>, Status> {
            Ok(Response::new(TaskListResponse {
                tasks: vec![FleetTask {
                    id: "t-1".into(),
                    prompt: "do work".into(),
                    status: "running".into(),
                    created_at: 0,
                    updated_at: 0,
                    output: None,
                    error: None,
                    cost_usd: None,
                    team_id: None,
                    cron_id: None,
                }],
            }))
        }

        async fn task_output(
            &self,
            request: Request<TaskOutputRequest>,
        ) -> Result<Response<TaskOutputResponse>, Status> {
            let req = request.into_inner();
            *self.output.lock().unwrap() = Some(req.clone());
            Ok(Response::new(TaskOutputResponse {
                task: Some(FleetTask {
                    id: req.task_id,
                    prompt: String::new(),
                    status: "completed".into(),
                    created_at: 0,
                    updated_at: 0,
                    output: Some("result".into()),
                    error: None,
                    cost_usd: None,
                    team_id: None,
                    cron_id: None,
                }),
            }))
        }

        async fn task_stop(
            &self,
            request: Request<TaskStopRequest>,
        ) -> Result<Response<TaskStopResponse>, Status> {
            let req = request.into_inner();
            *self.stopped.lock().unwrap() = Some(req);
            Ok(Response::new(TaskStopResponse { stopped: true }))
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

    fn mock() -> MockFleetService {
        MockFleetService {
            created: Arc::new(Mutex::new(None)),
            output: Arc::new(Mutex::new(None)),
            stopped: Arc::new(Mutex::new(None)),
        }
    }

    #[tokio::test]
    async fn task_create_round_trips_request_body() {
        let svc = mock();
        let created = svc.created.clone();
        let addr = crate::testutil::spawn(FleetServiceServer::new(svc)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            TaskArgs {
                command: TaskCommand::Create(CreateArgs {
                    prompt: "ship it".into(),
                    description: Some("release".into()),
                    team: Some("team-1".into()),
                }),
            },
            channel,
        )
        .await
        .expect("task create should succeed");

        let captured = created.lock().unwrap().take().expect("create captured");
        assert_eq!(captured.prompt, "ship it");
        assert_eq!(captured.description.as_deref(), Some("release"));
        assert_eq!(captured.team_id.as_deref(), Some("team-1"));
    }

    #[tokio::test]
    async fn task_output_and_stop_forward_task_id() {
        let svc = mock();
        let output = svc.output.clone();
        let stopped = svc.stopped.clone();
        let addr = crate::testutil::spawn(FleetServiceServer::new(svc)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            TaskArgs {
                command: TaskCommand::Output(OutputArgs {
                    task_id: "t-7".into(),
                }),
            },
            channel.clone(),
        )
        .await
        .expect("task output should succeed");
        assert_eq!(
            output.lock().unwrap().take().expect("output captured").task_id,
            "t-7"
        );

        handle(
            TaskArgs {
                command: TaskCommand::Stop(StopArgs {
                    task_id: "t-7".into(),
                }),
            },
            channel,
        )
        .await
        .expect("task stop should succeed");
        assert_eq!(
            stopped.lock().unwrap().take().expect("stop captured").task_id,
            "t-7"
        );
    }
}
