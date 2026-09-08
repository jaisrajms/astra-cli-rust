//! `astra doctor` — diagnose CLI/daemon connectivity.
//!
//! NOTE (E-02): there is no engine health service yet; this is an arg-parse
//! scaffold.

use clap::Args;

#[derive(Args)]
pub struct DoctorArgs {
    /// Also resolve and print the daemon endpoint.
    #[arg(long)]
    pub show_endpoint: bool,
}

pub async fn handle(args: DoctorArgs) -> anyhow::Result<()> {
    // TODO(E-02): probe the daemon once a health service exists.
    if args.show_endpoint {
        println!("endpoint: ~/.astra/engine.sock (default)");
    }
    println!("doctor: no engine health service yet (E-02 stub)");
    Ok(())
}
