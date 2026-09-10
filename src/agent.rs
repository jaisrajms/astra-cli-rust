//! `astra agent` — list / create agents (roles).

use clap::{Args, Subcommand, ValueEnum};
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;

use astra_proto::astra::engine::v1::{
    agent_service_client::AgentServiceClient, Agent, AgentMode, CreateAgentRequest,
    ListAgentsRequest, PermissionAction, PermissionRule, PermissionRuleset,
};

#[derive(Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: AgentCommand,
}

#[derive(Subcommand)]
pub enum AgentCommand {
    /// List all available agents.
    List,
    /// Create (register) a workspace-scoped agent.
    Create(CreateArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    /// Agent name.
    #[arg(long, value_name = "NAME")]
    pub name: String,
    /// What the agent does.
    #[arg(long, value_name = "DESCRIPTION")]
    pub description: Option<String>,
    /// Agent mode.
    #[arg(long, value_enum, default_value_t = Mode::Primary)]
    pub mode: Mode,
    /// Model in `provider/model` form.
    #[arg(long, short = 'm', value_name = "PROVIDER/MODEL")]
    pub model: Option<String>,
    /// Base "who am I" prompt.
    #[arg(long, value_name = "PROMPT")]
    pub prompt: Option<String>,
    /// Comma-separated `tool=action` rules (e.g. `bash=deny,edit=allow`).
    #[arg(long, value_name = "RULES")]
    pub permission: Option<String>,
}

#[derive(ValueEnum, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Primary,
    Subagent,
    All,
}

pub async fn handle(args: AgentArgs, channel: Channel) -> anyhow::Result<()> {
    match args.command {
        AgentCommand::List => list(channel).await,
        AgentCommand::Create(a) => create(a, channel).await,
    }
}

async fn list(channel: Channel) -> anyhow::Result<()> {
    let mut client = AgentServiceClient::new(channel);
    let resp = client
        .list_agents(with_workspace(Request::new(ListAgentsRequest {})))
        .await?
        .into_inner();

    let mut agents = resp.agents;
    agents.sort_by(|a, b| a.name.cmp(&b.name));

    for agent in &agents {
        println!("{} ({})", agent.name, mode_name(agent.mode));
        if let Some(perm) = &agent.permission {
            let rules: Vec<serde_json::Value> = perm
                .rules
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "tool": r.tool,
                        "matcher": r.matcher,
                        "action": action_name(r.action),
                    })
                })
                .collect();
            println!("  {}", serde_json::to_string(&rules)?);
        }
    }
    Ok(())
}

async fn create(args: CreateArgs, channel: Channel) -> anyhow::Result<()> {
    let agent = Agent {
        name: args.name.clone(),
        description: args.description,
        mode: match args.mode {
            Mode::Primary => AgentMode::Primary as i32,
            Mode::Subagent => AgentMode::Subagent as i32,
            Mode::All => AgentMode::All as i32,
        },
        prompt: args.prompt,
        permission: Some(PermissionRuleset {
            rules: parse_rules(args.permission.as_deref())?,
        }),
        model: args.model.as_deref().map(crate::ids::parse_model),
        steps: None,
        hidden: None,
        color: None,
    };

    let mut client = AgentServiceClient::new(channel);
    let resp = client
        .create_agent(with_workspace(Request::new(CreateAgentRequest {
            agent: Some(agent),
        })))
        .await?
        .into_inner();

    if let Some(a) = resp.agent {
        println!("Created agent {} ({})", a.name, mode_name(a.mode));
    }
    Ok(())
}

fn parse_rules(spec: Option<&str>) -> anyhow::Result<Vec<PermissionRule>> {
    let Some(spec) = spec else {
        return Ok(Vec::new());
    };
    spec.split(',')
        .map(|part| {
            let part = part.trim();
            let (tool, action) = part.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("invalid permission rule `{part}` (expected tool=action)")
            })?;
            Ok(PermissionRule {
                tool: tool.trim().to_string(),
                matcher: "*".to_string(),
                action: match action.trim() {
                    "allow" => PermissionAction::Allow as i32,
                    "deny" => PermissionAction::Deny as i32,
                    "ask" => PermissionAction::Ask as i32,
                    other => anyhow::bail!("invalid permission action `{other}`"),
                },
            })
        })
        .collect()
}

fn mode_name(mode: i32) -> &'static str {
    match AgentMode::try_from(mode) {
        Ok(AgentMode::Primary) => "primary",
        Ok(AgentMode::Subagent) => "subagent",
        Ok(AgentMode::All) => "all",
        _ => "unspecified",
    }
}

fn action_name(action: i32) -> &'static str {
    match PermissionAction::try_from(action) {
        Ok(PermissionAction::Allow) => "allow",
        Ok(PermissionAction::Deny) => "deny",
        Ok(PermissionAction::Ask) => "ask",
        _ => "unspecified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::agent_service_server::{AgentService, AgentServiceServer};
    use astra_proto::astra::engine::v1::{
        CreateAgentResponse, GenerateAgentRequest, GenerateAgentResponse, GetAgentRequest,
        GetAgentResponse, ListAgentsResponse,
    };
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status};

    struct MockAgentService {
        created: Arc<Mutex<Option<CreateAgentRequest>>>,
        generated: Arc<Mutex<Option<GenerateAgentRequest>>>,
    }

    #[tonic::async_trait]
    impl AgentService for MockAgentService {
        async fn list_agents(
            &self,
            _: Request<ListAgentsRequest>,
        ) -> Result<Response<ListAgentsResponse>, Status> {
            Ok(Response::new(ListAgentsResponse {
                agents: vec![
                    Agent {
                        name: "build".into(),
                        description: Some("build agent".into()),
                        mode: AgentMode::Primary as i32,
                        prompt: None,
                        permission: Some(PermissionRuleset { rules: Vec::new() }),
                        model: None,
                        steps: None,
                        hidden: None,
                        color: None,
                    },
                    Agent {
                        name: "explore".into(),
                        description: None,
                        mode: AgentMode::Subagent as i32,
                        prompt: None,
                        permission: None,
                        model: None,
                        steps: None,
                        hidden: None,
                        color: None,
                    },
                ],
            }))
        }

        async fn get_agent(
            &self,
            _: Request<GetAgentRequest>,
        ) -> Result<Response<GetAgentResponse>, Status> {
            Err(Status::unimplemented("get_agent"))
        }

        async fn create_agent(
            &self,
            request: Request<CreateAgentRequest>,
        ) -> Result<Response<CreateAgentResponse>, Status> {
            let req = request.into_inner();
            let created = req.agent.clone();
            *self.created.lock().unwrap() = Some(req);
            Ok(Response::new(CreateAgentResponse { agent: created }))
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
    async fn agent_list_round_trips_against_mock() {
        let mock = MockAgentService {
            created: Arc::new(Mutex::new(None)),
            generated: Arc::new(Mutex::new(None)),
        };
        let addr = crate::testutil::spawn(AgentServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            AgentArgs {
                command: AgentCommand::List,
            },
            channel,
        )
        .await
        .expect("agent list should succeed");
    }

    #[tokio::test]
    async fn agent_create_round_trips_request_body() {
        let created = Arc::new(Mutex::new(None));
        let mock = MockAgentService {
            created: created.clone(),
            generated: Arc::new(Mutex::new(None)),
        };
        let addr = crate::testutil::spawn(AgentServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            AgentArgs {
                command: AgentCommand::Create(CreateArgs {
                    name: "reviewer".into(),
                    description: Some("reviews diffs".into()),
                    mode: Mode::Subagent,
                    model: Some("anthropic/claude".into()),
                    prompt: Some("you are a reviewer".into()),
                    permission: Some("bash=deny,edit=allow".into()),
                }),
            },
            channel,
        )
        .await
        .expect("agent create should succeed");

        let captured = created.lock().unwrap().take().expect("create captured");
        let agent = captured.agent.expect("request carries an agent");
        assert_eq!(agent.name, "reviewer");
        assert_eq!(agent.description.as_deref(), Some("reviews diffs"));
        assert_eq!(agent.mode, AgentMode::Subagent as i32);
        assert_eq!(agent.prompt.as_deref(), Some("you are a reviewer"));

        let model = agent.model.expect("model parsed");
        assert_eq!(model.provider_id, "anthropic");
        assert_eq!(model.model_id, "claude");

        let rules = agent.permission.expect("permission present").rules;
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].tool, "bash");
        assert_eq!(rules[0].action, PermissionAction::Deny as i32);
        assert_eq!(rules[1].tool, "edit");
        assert_eq!(rules[1].action, PermissionAction::Allow as i32);
    }

    #[test]
    fn parses_permission_rules() {
        let rules = parse_rules(Some("bash=deny,edit=allow,read=ask")).unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].tool, "bash");
        assert_eq!(rules[0].action, PermissionAction::Deny as i32);
        assert_eq!(rules[1].action, PermissionAction::Allow as i32);
        assert_eq!(rules[2].action, PermissionAction::Ask as i32);
    }
}
