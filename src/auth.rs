//! `astra auth` — daemon authentication.
//!
//! NOTE (E-02): there is no engine service for auth; this is an arg-parse
//! scaffold.

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
    // TODO(E-02): wire auth once the daemon exposes an auth service.
    let what = match args.command {
        Some(AuthCommand::Login) => "login",
        Some(AuthCommand::Logout) => "logout",
        Some(AuthCommand::Status) => "status",
        None => "auth",
    };
    println!("{what}: not yet implemented (E-02 stub)");
    Ok(())
}
