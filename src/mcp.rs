//! `astra mcp` — list / connect / configure MCP servers.

use clap::{Args, Subcommand};
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;

use astra_proto::astra::engine::v1::{
    mcp_service_client::McpServiceClient, ConfigureServerRequest, ConnectServerRequest,
    ListServersRequest, McpServer,
};

#[derive(Args)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: Option<McpCommand>,
}

#[derive(Subcommand)]
pub enum McpCommand {
    /// List configured MCP servers and their status.
    List,
    /// Connect to an MCP server by name.
    Connect(ConnectArgs),
    /// Configure (register or update) an MCP server.
    Configure(ConfigureArgs),
}

#[derive(Args)]
pub struct ConnectArgs {
    /// Server name.
    #[arg(value_name = "NAME")]
    pub name: String,
}

#[derive(Args)]
pub struct ConfigureArgs {
    /// Server name.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Server kind: `local` or `remote`.
    #[arg(long, value_name = "KIND")]
    pub kind: String,
    /// Remote endpoint URL (`kind=remote`).
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,
    /// Local command argv (`kind=local`).
    #[arg(long, num_args = 1.., value_name = "ARG")]
    pub command: Vec<String>,
    /// OAuth-enabled remote.
    #[arg(long)]
    pub oauth: Option<bool>,
}

pub async fn handle(args: McpArgs, channel: Channel) -> anyhow::Result<()> {
    match args.command.unwrap_or(McpCommand::List) {
        McpCommand::List => list(channel).await,
        McpCommand::Connect(a) => connect(a, channel).await,
        McpCommand::Configure(a) => configure(a, channel).await,
    }
}

async fn list(channel: Channel) -> anyhow::Result<()> {
    let mut client = McpServiceClient::new(channel);
    let resp = client
        .list_servers(with_workspace(Request::new(ListServersRequest {})))
        .await?
        .into_inner();

    let rows: Vec<Vec<String>> = resp
        .servers
        .iter()
        .map(|s| {
            vec![
                s.name.clone(),
                s.kind.clone(),
                s.status.clone().unwrap_or_default(),
                endpoint(s),
            ]
        })
        .collect();
    crate::render::table(&["Name", "Kind", "Status", "Endpoint"], &rows);
    Ok(())
}

async fn connect(args: ConnectArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = McpServiceClient::new(channel);
    let resp = client
        .connect_server(with_workspace(Request::new(ConnectServerRequest {
            name: args.name.clone(),
        })))
        .await?
        .into_inner();

    println!("{}: {}", args.name, resp.status);
    if let Some(err) = resp.error {
        println!("  error: {err}");
    }
    Ok(())
}

async fn configure(args: ConfigureArgs, channel: Channel) -> anyhow::Result<()> {
    let mut client = McpServiceClient::new(channel);
    let resp = client
        .configure_server(with_workspace(Request::new(ConfigureServerRequest {
            name: args.name.clone(),
            kind: args.kind,
            url: args.url,
            command: args.command,
            oauth: args.oauth,
        })))
        .await?
        .into_inner();

    if let Some(server) = resp.server {
        println!("Configured MCP server: {}", server.name);
        println!("  kind: {}", server.kind);
        println!("  endpoint: {}", endpoint(&server));
    }
    Ok(())
}

fn endpoint(s: &McpServer) -> String {
    if s.kind == "remote" {
        s.url.clone().unwrap_or_default()
    } else {
        s.command.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::mcp_service_server::{McpService, McpServiceServer};
    use astra_proto::astra::engine::v1::{
        ConfigureServerResponse, ConnectServerResponse, ListServersResponse,
    };
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status};

    struct MockMcpService {
        connected: Arc<Mutex<Option<ConnectServerRequest>>>,
        configured: Arc<Mutex<Option<ConfigureServerRequest>>>,
    }

    #[tonic::async_trait]
    impl McpService for MockMcpService {
        async fn list_servers(
            &self,
            _: Request<ListServersRequest>,
        ) -> Result<Response<ListServersResponse>, Status> {
            Ok(Response::new(ListServersResponse {
                servers: vec![McpServer {
                    name: "memory".into(),
                    kind: "local".into(),
                    url: None,
                    command: vec!["npx".into(), "-y".into()],
                    status: Some("connected".into()),
                    oauth: None,
                }],
            }))
        }

        async fn connect_server(
            &self,
            request: Request<ConnectServerRequest>,
        ) -> Result<Response<ConnectServerResponse>, Status> {
            *self.connected.lock().unwrap() = Some(request.into_inner());
            Ok(Response::new(ConnectServerResponse {
                status: "connected".into(),
                error: None,
            }))
        }

        async fn configure_server(
            &self,
            request: Request<ConfigureServerRequest>,
        ) -> Result<Response<ConfigureServerResponse>, Status> {
            let req = request.into_inner();
            *self.configured.lock().unwrap() = Some(req.clone());
            Ok(Response::new(ConfigureServerResponse {
                server: Some(McpServer {
                    name: req.name,
                    kind: req.kind,
                    url: req.url,
                    command: req.command,
                    status: Some("not_initialized".into()),
                    oauth: req.oauth,
                }),
            }))
        }
    }

    #[tokio::test]
    async fn mcp_connect_round_trips_request_body() {
        let connected = Arc::new(Mutex::new(None));
        let mock = MockMcpService {
            connected: connected.clone(),
            configured: Arc::new(Mutex::new(None)),
        };
        let addr = crate::testutil::spawn(McpServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            McpArgs {
                command: Some(McpCommand::Connect(ConnectArgs {
                    name: "memory".into(),
                })),
            },
            channel,
        )
        .await
        .expect("mcp connect should succeed");

        let captured = connected.lock().unwrap().take().expect("connect captured");
        assert_eq!(captured.name, "memory");
    }

    #[tokio::test]
    async fn mcp_configure_round_trips_request_body() {
        let configured = Arc::new(Mutex::new(None));
        let mock = MockMcpService {
            connected: Arc::new(Mutex::new(None)),
            configured: configured.clone(),
        };
        let addr = crate::testutil::spawn(McpServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            McpArgs {
                command: Some(McpCommand::Configure(ConfigureArgs {
                    name: "remote".into(),
                    kind: "remote".into(),
                    url: Some("https://example.com/mcp".into()),
                    command: vec![],
                    oauth: Some(true),
                })),
            },
            channel,
        )
        .await
        .expect("mcp configure should succeed");

        let captured = configured
            .lock()
            .unwrap()
            .take()
            .expect("configure captured");
        assert_eq!(captured.name, "remote");
        assert_eq!(captured.kind, "remote");
        assert_eq!(captured.url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(captured.oauth, Some(true));
    }
}
