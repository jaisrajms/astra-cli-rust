//! Pure (clap-derived) argument parsing for the `astra` CLI surface.
//!
//! This module owns no I/O: it only declares the subcommand tree and the
//! per-command argument structs. The RPC-calling logic lives in the sibling
//! command modules, whose handlers take the parsed args plus a tonic channel.

use clap::{Parser, Subcommand};

use crate::agent::AgentArgs;
use crate::auth::AuthArgs;
use crate::config::ConfigArgs;
use crate::cron::CronArgs;
use crate::doctor::DoctorArgs;
use crate::generate::GenerateArgs;
use crate::mcp::McpArgs;
use crate::models::ModelsArgs;
use crate::run::RunArgs;
use crate::serve::ServeArgs;
use crate::session::SessionArgs;
use crate::task::TaskArgs;
use crate::team::TeamArgs;
use crate::workflow::WorkflowArgs;

#[derive(Parser)]
#[command(name = "astra", version, about = "Astra CLI", long_about = None)]
pub struct Cli {
    /// Daemon endpoint: a Unix socket path (default `~/.astra/engine.sock`) or
    /// an `http(s)://` URI. Overridable via the `ASTRA_ENDPOINT` env var.
    #[arg(long, global = true, env = "ASTRA_ENDPOINT", value_name = "ENDPOINT")]
    pub endpoint: Option<String>,

    /// The subcommand to run. Omitted for the default full-screen TUI.
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// The resolved daemon endpoint, with the socket default applied.
    pub fn endpoint(&self) -> String {
        match self.endpoint.as_deref() {
            Some(e) if !e.trim().is_empty() => e.to_string(),
            _ => "~/.astra/engine.sock".to_string(),
        }
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Run a one-shot message against the daemon.
    Run(RunArgs),
    /// Manage sessions.
    Session(SessionArgs),
    /// Manage agents.
    Agent(AgentArgs),
    /// Generate a new agent from a description.
    Generate(GenerateArgs),
    /// Manage MCP servers.
    Mcp(McpArgs),
    /// List the resolved model catalog.
    Models(ModelsArgs),
    /// Start a headless daemon-facing server.
    Serve(ServeArgs),
    /// Manage background fleet tasks.
    Task(TaskArgs),
    /// Manage teams of fleet tasks.
    Team(TeamArgs),
    /// Manage cron schedules.
    Cron(CronArgs),
    /// Drive a workflow over the engine's workflow stream.
    Workflow(WorkflowArgs),
    /// Read/write local CLI configuration.
    Config(ConfigArgs),
    /// Manage daemon authentication.
    Auth(AuthArgs),
    /// Diagnose CLI/daemon connectivity.
    Doctor(DoctorArgs),
}
