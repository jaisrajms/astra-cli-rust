//! `astra models [provider]` — list the resolved model catalog (F-02).

use clap::Args;
use tonic::transport::Channel;

use astra_proto::astra::engine::v1::{
    gateway_service_client::GatewayServiceClient, ListModelsRequest,
};

#[derive(Args)]
pub struct ModelsArgs {
    /// Provider ID to filter models by.
    #[arg(value_name = "PROVIDER")]
    pub provider: Option<String>,
}

pub async fn handle(args: ModelsArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = GatewayServiceClient::new(channel);
    let resp = client
        .list_models(ListModelsRequest {
            provider: args.provider,
        })
        .await?
        .into_inner();

    for model in resp.models {
        println!("{}/{}", model.provider, model.id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::gateway_service_server::{
        GatewayService, GatewayServiceServer,
    };
    use astra_proto::astra::engine::v1::{
        AbortRequest, AbortResponse, EstimateCostRequest, EstimateCostResponse, ListModelsResponse,
        Model, QueryRequest, QueryResponse, SessionCostRequest, SessionCostResponse,
    };
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status};

    struct MockGatewayService {
        list_models: Arc<Mutex<Option<ListModelsRequest>>>,
    }

    #[tonic::async_trait]
    impl GatewayService for MockGatewayService {
        type QueryStream =
            Pin<Box<dyn futures::Stream<Item = Result<QueryResponse, Status>> + Send>>;

        async fn query(
            &self,
            _: Request<QueryRequest>,
        ) -> Result<Response<Self::QueryStream>, Status> {
            Err(Status::unimplemented("query"))
        }

        async fn estimate_cost(
            &self,
            _: Request<EstimateCostRequest>,
        ) -> Result<Response<EstimateCostResponse>, Status> {
            Err(Status::unimplemented("estimate_cost"))
        }

        async fn session_cost(
            &self,
            _: Request<SessionCostRequest>,
        ) -> Result<Response<SessionCostResponse>, Status> {
            Err(Status::unimplemented("session_cost"))
        }

        async fn abort(&self, _: Request<AbortRequest>) -> Result<Response<AbortResponse>, Status> {
            Err(Status::unimplemented("abort"))
        }

        async fn list_models(
            &self,
            request: Request<ListModelsRequest>,
        ) -> Result<Response<ListModelsResponse>, Status> {
            *self.list_models.lock().unwrap() = Some(request.into_inner());
            Ok(Response::new(ListModelsResponse {
                models: vec![
                    Model {
                        id: "claude-sonnet".into(),
                        provider: "anthropic".into(),
                        name: Some("Claude Sonnet".into()),
                        family: None,
                    },
                    Model {
                        id: "gpt-4o".into(),
                        provider: "openai".into(),
                        name: None,
                        family: None,
                    },
                ],
            }))
        }
    }

    #[tokio::test]
    async fn models_list_forwards_provider_filter() {
        let list_models = Arc::new(Mutex::new(None));
        let mock = MockGatewayService {
            list_models: list_models.clone(),
        };
        let addr = crate::testutil::spawn(GatewayServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            ModelsArgs {
                provider: Some("anthropic".into()),
            },
            channel,
        )
        .await
        .expect("models list should succeed");

        let captured = list_models.lock().unwrap().take().expect("list_models captured");
        assert_eq!(captured.provider.as_deref(), Some("anthropic"));
    }
}
