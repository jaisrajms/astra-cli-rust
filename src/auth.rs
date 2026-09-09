//! `astra auth` — daemon authentication.
//!
//! The daemon is a single-user local daemon reached over a `0700` Unix socket
//! (`~/.astra/engine.sock`) or a Windows named pipe; there is no network
//! bearer token to manage. `auth` therefore reports that status rather than
//! driving a login flow.

use clap::{Args, Subcommand};

#[derive(Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: Option<AuthCommand>,
}

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Log in to the daemon.
    Login,
    /// Log out of the daemon.
    Logout,
    /// Show the current auth status.
    Status,
}

pub async fn handle(args: AuthArgs) -> anyhow::Result<()> {
    let endpoint = crate::endpoint::resolved_endpoint();
    match args.command {
        Some(AuthCommand::Login) | Some(AuthCommand::Logout) => {
            println!("no credential to store: the daemon is single-user over a local socket ({endpoint}).");
        }
        Some(AuthCommand::Status) | None => {
            println!("auth: local (single-user daemon over {endpoint}); no bearer token.");
        }
    }
    Ok(())
}
