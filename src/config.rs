//! `astra config` — local CLI configuration.
//!
//! NOTE (E-02): there is no engine service for CLI config; this is an
//! arg-parse scaffold.

use clap::{Args, Subcommand};

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: Option<ConfigCommand>,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Show the resolved daemon endpoint.
    Endpoint,
}

pub async fn handle(args: ConfigArgs) -> anyhow::Result<()> {
    // TODO(E-02): persist CLI config once the daemon exposes a config surface.
    match args.command {
        Some(ConfigCommand::Endpoint) => {
            println!("endpoint: ~/.astra/engine.sock (default; override with --endpoint / ASTRA_ENDPOINT)");
        }
        None => {
            println!("config: not yet persisted (E-02 stub)");
        }
    }
    Ok(())
}
