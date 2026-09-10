//! `astra generate <description>` — propose a new agent (A-05 GenerateAgent).

use clap::Args;
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;

use astra_proto::astra::engine::v1::{
    agent_service_client::AgentServiceClient, GenerateAgentRequest,
};

#[derive(Args)]
pub struct GenerateArgs {
    /// Description of the agent to generate.
    #[arg(value_name = "DESCRIPTION")]
    pub description: String,
    /// Model in `provider/model` form.
    #[arg(long, short = 'm', value_name = "PROVIDER/MODEL")]
    pub model: Option<String>,
}

pub async fn handle(args: GenerateArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = AgentServiceClient::new(channel);
    let resp = client
        .generate_agent(with_workspace(Request::new(GenerateAgentRequest {
            description: args.description,
            model: args.model.as_deref().map(crate::ids::parse_model),
        })))
        .await?
        .into_inner();

    println!("identifier: {}", resp.identifier);
    println!("when_to_use: {}", resp.when_to_use);
    println!("system_prompt: {}", resp.system_prompt);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::agent_service_server::{AgentService, AgentServiceServer};
    use astra_proto::astra::engine::v1::{
        CreateAgentRequest, CreateAgentResponse, GenerateAgentResponse, GetAgentRequest,
        GetAgentResponse, ListAgentsRequest, ListAgentsResponse,
    };
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status};

    struct MockAgentService {
        generated: Arc<Mutex<Option<GenerateAgentRequest>>>,
    }

    #[tonic::async_trait]
    impl AgentService for MockAgentService {
        async fn list_agents(
            &self,
            _: Request<ListAgentsRequest>,
        ) -> Result<Response<ListAgentsResponse>, Status> {
            Err(Status::unimplemented("list_agents"))
        }

        async fn get_agent(
            &self,
            _: Request<GetAgentRequest>,
        ) -> Result<Response<GetAgentResponse>, Status> {
            Err(Status::unimplemented("get_agent"))
        }

        async fn create_agent(
            &self,
            _: Request<CreateAgentRequest>,
        ) -> Result<Response<CreateAgentResponse>, Status> {
            Err(Status::unimplemented("create_agent"))
        }

        async fn generate_agent(
            &self,
            request: Request<GenerateAgentRequest>,
        ) -> Result<Response<GenerateAgentResponse>, Status> {
            *self.generated.lock().unwrap() = Some(request.into_inner());
            Ok(Response::new(GenerateAgentResponse {
                identifier: "gen-agent".into(),
                when_to_use: "when to use".into(),
                system_prompt: "system prompt".into(),
            }))
        }
    }

    #[tokio::test]
    async fn generate_round_trips_request_body() {
        let generated = Arc::new(Mutex::new(None));
        let mock = MockAgentService {
            generated: generated.clone(),
        };
        let addr = crate::testutil::spawn(AgentServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            GenerateArgs {
                description: "a diff reviewer".into(),
                model: Some("anthropic/claude".into()),
            },
            channel,
        )
        .await
        .expect("generate should succeed");

        let captured = generated.lock().unwrap().take().expect("generate captured");
        assert_eq!(captured.description, "a diff reviewer");
        let model = captured.model.expect("model parsed");
        assert_eq!(model.provider_id, "anthropic");
        assert_eq!(model.model_id, "claude");
    }
}
