//! `astra serve` — start a headless server (opencode parity).
//!
//! The astra daemon (`astrad`) owns the server socket (`~/.astra/engine.sock`)
//! and serves the gRPC surface (`SessionService`, `ChatService`, `FleetService`,
//! …). `astra serve` therefore does not start a second server; it reports the
//! daemon ownership model and how to reach it.

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
    let endpoint = crate::endpoint::resolved_endpoint();
    println!(
        "astra serve ({host}:{port}): the daemon (`astrad`) owns the engine socket ({endpoint}) and serves the gRPC surface;\n\
         a separate CLI-side HTTP gateway is not provided. Start the daemon with `astrad` and talk to it via the CLI or the TUI.",
        host = args.host,
        port = args.port
    );
    Ok(())
}
