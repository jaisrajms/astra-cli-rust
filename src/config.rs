//! `astra config` — local CLI configuration.
//!
//! The CLI's only persisted knob today is the daemon endpoint (`--endpoint` /
//! `ASTRA_ENDPOINT`); all other configuration lives in the daemon/workspace
//! (`.astra/*`, `~/.astra/*`). `config` therefore reports the resolved endpoint
//! and how it was set, and `config endpoint` prints just the endpoint value.

use clap::{Args, Subcommand};

use crate::endpoint;

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
    let endpoint = endpoint::resolved_endpoint();
    let source = match std::env::var("ASTRA_ENDPOINT") {
        Ok(e) if !e.trim().is_empty() => {
            format!("ASTRA_ENDPOINT (value: {e})")
        }
        _ => "default".to_string(),
    };

    match args.command {
        Some(ConfigCommand::Endpoint) => {
            println!("{endpoint}");
        }
        None => {
            println!("endpoint: {endpoint} (source: {source})");
            println!(
                "other configuration lives in the daemon/workspace (`.astra/*`, `~/.astra/*`)."
            );
        }
    }
    Ok(())
}
