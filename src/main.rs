//! Astra CLI entry point.
//!
//! Parses the subcommand surface and dispatches to the
//! per-subcommand handler, which is a thin async fn taking a tonic
//! [`Channel`] so it can be exercised against a mock server in tests.

mod agent;
mod auth;
mod cli;
mod config;
mod cron;
mod doctor;
mod endpoint;
mod export;
mod generate;
mod ids;
mod import;
mod mcp;
mod models;
mod protocol;
mod render;
mod run;
mod serve;
mod session;
mod task;
mod team;
mod tui;
mod workflow;

#[cfg(test)]
mod testutil;

use clap::Parser;
use cli::{Cli, Command};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let endpoint = cli.endpoint();
    let channel = endpoint::connect(&endpoint)?;

    match target_for(cli.command) {
        Target::Tui => tui::run(channel).await,
        Target::Subcommand(command) => dispatch(command, channel).await,
    }
}

/// Where the CLI routes after parsing: the default full-screen TUI (no
/// subcommand) or a specific subcommand.
enum Target {
    Tui,
    Subcommand(Command),
}

fn target_for(command: Option<Command>) -> Target {
    match command {
        Some(command) => Target::Subcommand(command),
        None => Target::Tui,
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("astra=info,warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn dispatch(command: Command, channel: tonic::transport::Channel) -> anyhow::Result<()> {
    match command {
        Command::Run(args) => run::handle(args, channel).await,
        Command::Session(args) => session::handle(args, channel).await,
        Command::Agent(args) => agent::handle(args, channel).await,
        Command::Generate(args) => generate::handle(args, channel).await,
        Command::Mcp(args) => mcp::handle(args, channel).await,
        Command::Models(args) => models::handle(args, channel).await,
        Command::Serve(args) => serve::handle(args).await,
        Command::Task(args) => task::handle(args, channel).await,
        Command::Team(args) => team::handle(args, channel).await,
        Command::Cron(args) => cron::handle(args, channel).await,
        Command::Workflow(args) => workflow::handle(args, channel).await,
        Command::Config(args) => config::handle(args).await,
        Command::Auth(args) => auth::handle(args).await,
        Command::Doctor(args) => doctor::handle(args, channel).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_dispatches_to_tui() {
        let cli = Cli::try_parse_from(["astra"]).expect("parse no-arg invocation");
        assert!(matches!(target_for(cli.command), Target::Tui));
    }

    #[test]
    fn subcommand_dispatches_to_handler() {
        let cli = Cli::try_parse_from(["astra", "run", "hello"]).expect("parse run invocation");
        assert!(matches!(
            target_for(cli.command),
            Target::Subcommand(Command::Run(_))
        ));
    }

    #[test]
    fn every_subcommand_still_dispatches() {
        let cases: &[&[&str]] = &[
            &["run", "hi"],
            &["session", "list"],
            &["agent", "list"],
            &["generate", "a helper"],
            &["mcp"],
            &["models"],
            &["serve"],
            &["task", "list"],
            &["team", "list"],
            &["cron", "list"],
            &["workflow", "hi"],
            &["config"],
            &["auth"],
            &["doctor"],
        ];
        for case in cases {
            let mut argv = vec!["astra"];
            argv.extend_from_slice(case);
            let cli = Cli::try_parse_from(argv).expect("parse subcommand");
            assert!(
                matches!(target_for(cli.command), Target::Subcommand(_)),
                "subcommand `{case:?}` should dispatch, not launch the TUI"
            );
        }
    }
}
