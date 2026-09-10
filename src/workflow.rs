//! `astra workflow` — drive a workflow over the engine's workflow stream.

use clap::{Args, ValueEnum};
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;

use astra_proto::astra::engine::v1::workflow_service_client::WorkflowServiceClient;
use astra_proto::astra::engine::v1::{
    client_msg, engine_event, ClientMsg, EngineEvent, StartFull, StartQuick, WorkflowStateName,
};

#[derive(Args)]
pub struct WorkflowArgs {
    /// Workflow input prompt.
    #[arg(value_name = "PROMPT")]
    pub prompt: String,
    /// Start kind: `quick` | `full`.
    #[arg(long, value_enum, default_value_t = Kind::Quick)]
    pub kind: Kind,
}

#[derive(ValueEnum, Clone, Copy)]
pub enum Kind {
    Quick,
    Full,
}

pub async fn handle(args: WorkflowArgs, channel: Channel) -> anyhow::Result<()> {
    let msg = ClientMsg {
        msg: Some(match args.kind {
            Kind::Quick => client_msg::Msg::StartQuick(StartQuick {
                input: args.prompt,
                input_obj: None,
                exec: String::new(),
            }),
            Kind::Full => client_msg::Msg::StartFull(StartFull {
                input: args.prompt,
                input_obj: None,
                exec: String::new(),
            }),
        }),
    };

    let mut client = WorkflowServiceClient::new(channel);
    let outbound = futures::stream::iter(std::iter::once(msg));
    let resp = client
        .stream_workflow(with_workspace(Request::new(outbound)))
        .await?;
    let mut stream = resp.into_inner();

    while let Some(event) = stream.message().await? {
        if terminal(&event) {
            render(&event);
            break;
        }
        render(&event);
    }
    Ok(())
}

fn terminal(event: &EngineEvent) -> bool {
    if let Some(engine_event::Event::StateChange(state)) = &event.event {
        return matches!(
            WorkflowStateName::try_from(state.name),
            Ok(WorkflowStateName::Succeeded)
                | Ok(WorkflowStateName::Failed)
                | Ok(WorkflowStateName::Abandoned)
        );
    }
    false
}

fn render(event: &EngineEvent) {
    match &event.event {
        Some(engine_event::Event::StateChange(state)) => {
            let name = WorkflowStateName::try_from(state.name)
                .map(|n| format!("{n:?}"))
                .unwrap_or_else(|_| state.name.to_string());
            println!("[state] {name}");
        }
        Some(engine_event::Event::TextDelta(delta)) => print!("{}", delta.text),
        Some(engine_event::Event::PromptRequired(_)) => println!("[prompt required]"),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::workflow_service_server::{
        WorkflowService, WorkflowServiceServer,
    };
    use astra_proto::astra::engine::v1::{
        client_msg, engine_event, ClientMsg, EngineEvent, TextDelta, WorkflowState,
    };
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status, Streaming};

    struct MockWorkflowService {
        received: Arc<Mutex<Option<ClientMsg>>>,
    }

    fn succeeded_state() -> WorkflowState {
        WorkflowState {
            name: WorkflowStateName::Succeeded as i32,
            workflow_id: String::new(),
            mode: String::new(),
            phase_label: String::new(),
            current_task_index: 0,
            total_tasks: 0,
            title: String::new(),
            iteration: 0,
            max_iterations: 0,
            activity: String::new(),
            activity_log: String::new(),
            cost_tokens: 0,
            cost_usd: 0.0,
            error_message: String::new(),
            error_detail: String::new(),
            error_detail_path: String::new(),
            pending_tool_approval: None,
            pending_clarification: None,
            pending_dirty_decision: None,
            pending_hook_confirm: None,
            pending_budget_decision: None,
            pending_verify_decision: None,
            pending_recovery: None,
            pending_privacy_decision: None,
            affected_repos: Vec::new(),
        }
    }

    #[tonic::async_trait]
    impl WorkflowService for MockWorkflowService {
        type StreamWorkflowStream =
            Pin<Box<dyn futures::Stream<Item = Result<EngineEvent, Status>> + Send>>;

        async fn stream_workflow(
            &self,
            request: Request<Streaming<ClientMsg>>,
        ) -> Result<Response<Self::StreamWorkflowStream>, Status> {
            let mut inbound = request.into_inner();
            if let Some(msg) = inbound.message().await? {
                *self.received.lock().unwrap() = Some(msg);
            }

            let stream = futures::stream::iter(vec![
                Ok(EngineEvent {
                    event: Some(engine_event::Event::TextDelta(TextDelta {
                        text: "hi".into(),
                    })),
                }),
                Ok(EngineEvent {
                    event: Some(engine_event::Event::StateChange(succeeded_state())),
                }),
            ]);
            Ok(Response::new(Box::pin(stream)))
        }
    }

    #[tokio::test]
    async fn workflow_quick_round_trips_request_body() {
        let received = Arc::new(Mutex::new(None));
        let mock = MockWorkflowService {
            received: received.clone(),
        };
        let addr = crate::testutil::spawn(WorkflowServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            WorkflowArgs {
                prompt: "build a thing".into(),
                kind: Kind::Quick,
            },
            channel,
        )
        .await
        .expect("workflow should succeed");

        let captured = received.lock().unwrap().take().expect("workflow captured");
        match captured.msg {
            Some(client_msg::Msg::StartQuick(sq)) => assert_eq!(sq.input, "build a thing"),
            other => panic!("expected StartQuick, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn workflow_full_round_trips_request_body() {
        let received = Arc::new(Mutex::new(None));
        let mock = MockWorkflowService {
            received: received.clone(),
        };
        let addr = crate::testutil::spawn(WorkflowServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            WorkflowArgs {
                prompt: "plan it fully".into(),
                kind: Kind::Full,
            },
            channel,
        )
        .await
        .expect("workflow should succeed");

        let captured = received.lock().unwrap().take().expect("workflow captured");
        match captured.msg {
            Some(client_msg::Msg::StartFull(sf)) => assert_eq!(sf.input, "plan it fully"),
            other => panic!("expected StartFull, got {other:?}"),
        }
    }
}
