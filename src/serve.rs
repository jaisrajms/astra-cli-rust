//! `astra serve` — start a headless server (opencode parity).
//!
//! NOTE (E-02): the astra daemon owns the server socket (`~/.astra/engine.sock`);
//! a CLI-side headless HTTP gateway has no engine service yet, so this is an
//! arg-parse scaffold.

use clap::Args;

#[derive(Args)]
pub struct ServeArgs {
    /// Listen host for the headless server.
    #[arg(long, default_value = "127.0.0.1", value_name = "HOST")]
    pub host: String,
    /// Listen port.
    #[arg(long, default_value_t = 4096, value_name = "PORT")]
    pub port: u16,
}

pub async fn handle(args: ServeArgs) -> anyhow::Result<()> {
    // TODO(E-02): start a headless gateway once the daemon exposes an HTTP surface.
    println!(
        "astra serve ({host}:{port}) is not yet implemented — the daemon owns the server socket (E-02 stub)",
        host = args.host,
        port = args.port
    );
    Ok(())
}
