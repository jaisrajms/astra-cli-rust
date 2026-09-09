//! `astra doctor` — diagnose CLI/daemon connectivity.
//!
//! The daemon serves `grpc.health.v1.Health` and the `ConnectivityService`
//! probe RPCs. `doctor` verifies the daemon is reachable over the resolved
//! endpoint and reports the transport + a basic reachability verdict. The
//! per-route credential diagnostics live behind `ConnectivityService.probe`
//! whose wire messages are still placeholder-shaped (`connectivity.proto`),
//! so `doctor` reports reachability today and points at that gap rather than
//! fabricating per-route rows.

use clap::Args;
use tonic::transport::Channel;

use astra_proto::astra::engine::v1::connectivity_service_client::ConnectivityServiceClient;
use astra_proto::astra::engine::v1::ProbeAllRequest;

#[derive(Args)]
pub struct DoctorArgs {
    /// Also resolve and print the daemon endpoint.
    #[arg(long)]
    pub show_endpoint: bool,
}

/// Reachability check: the daemon answers the probe RPC (empty response), so a
/// successful round-trip is the "daemon reachable" signal. Any transport error
/// (unavailable / connection refused) surfaces as an error.
pub async fn handle(args: DoctorArgs, channel: Channel) -> anyhow::Result<()> {
    let endpoint = crate::endpoint::resolved_endpoint();
    if args.show_endpoint {
        println!("endpoint: {endpoint}");
    }

    let mut client = ConnectivityServiceClient::new(channel);
    client
        .probe_all(ProbeAllRequest {})
        .await
        .map_err(|status| {
            anyhow::anyhow!(
                "daemon not reachable at {endpoint}: {status}\n\
                 hint: is `astrad` running? start it with `astrad`."
            )
        })?;

    println!("daemon: reachable ({endpoint})");
    println!("note: per-route credential diagnostics are not yet exposed over the wire (connectivity.proto is placeholder-shaped).");
    Ok(())
}
