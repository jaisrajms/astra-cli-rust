//! `astra doctor` — diagnose CLI/daemon connectivity.
//!
//! The daemon serves `grpc.health.v1.Health` and the `ConnectivityService`
//! probe RPCs. `doctor` calls `ProbeAll` and renders the returned per-route
//! connectivity rows: route, provider, status (`checking`/`ok`/`error`), and —
//! on error — the failure's `kind`/`reason`/`remediation`.

use clap::Args;
use tonic::transport::Channel;

use astra_proto::astra::engine::v1::connectivity_service_client::ConnectivityServiceClient;
use astra_proto::astra::engine::v1::{
    ConnectivityStatus, Failure, FailureKind, ProbeAllRequest, Route, RouteStatus,
};

#[derive(Args)]
pub struct DoctorArgs {
    /// Also resolve and print the daemon endpoint.
    #[arg(long)]
    pub show_endpoint: bool,
}

/// Probe every route over `channel` and print the doctor table.
///
/// A successful round-trip is the "daemon reachable" signal; any transport error
/// (unavailable / connection refused) surfaces as a "daemon not reachable" error.
pub async fn handle(args: DoctorArgs, channel: Channel) -> anyhow::Result<()> {
    let endpoint = crate::endpoint::resolved_endpoint();
    if args.show_endpoint {
        println!("endpoint: {endpoint}");
    }

    let report = probe_and_render(&channel, &endpoint).await?;
    print!("{report}");
    Ok(())
}

/// Call `ProbeAll` and render the resulting rows as a single report string.
///
/// Factored out of [`handle`] so the mock-server tests can assert on the rendered
/// output without capturing stdout.
async fn probe_and_render(channel: &Channel, endpoint: &str) -> anyhow::Result<String> {
    let mut client = ConnectivityServiceClient::new(channel.clone());
    let response = client
        .probe_all(ProbeAllRequest {})
        .await
        .map_err(|status| {
            anyhow::anyhow!(
                "daemon not reachable at {endpoint}: {status}\n\
                 hint: is `astrad` running? start it with `astrad`."
            )
        })?;

    let mut out = format!("daemon: reachable ({endpoint})\n");
    for row in &response.into_inner().rows {
        out.push_str(&render_row(row));
        out.push('\n');
    }
    Ok(out)
}

/// Render one connectivity row as a single readable line.
fn render_row(row: &ConnectivityStatus) -> String {
    let route = route_name(row.route);
    let status = status_name(row.status);
    let mut line = format!("{route} ({provider}): {status}", provider = row.provider);
    if let Some(failure) = &row.failure {
        line.push_str(&render_failure(failure));
    }
    line
}

/// Render a failure's kind/reason/remediation, e.g.
/// ` — auth_expired: api key expired (remediation: run `astra auth login`)`.
fn render_failure(failure: &Failure) -> String {
    let mut out = format!(" — {}: {}", kind_name(failure.kind), failure.reason);
    if let Some(remediation) = &failure.remediation {
        out.push_str(&format!(" (remediation: {remediation})"));
    }
    out
}

fn route_name(route: i32) -> &'static str {
    match Route::try_from(route) {
        Ok(Route::Phase1) => "phase1",
        Ok(Route::Phase2) => "phase2",
        Ok(Route::Phase3) => "phase3",
        Ok(Route::Chat) => "chat",
        _ => "unknown",
    }
}

fn status_name(status: i32) -> &'static str {
    match RouteStatus::try_from(status) {
        Ok(RouteStatus::Checking) => "checking",
        Ok(RouteStatus::Ok) => "ok",
        Ok(RouteStatus::Error) => "error",
        _ => "unknown",
    }
}

fn kind_name(kind: i32) -> &'static str {
    match FailureKind::try_from(kind) {
        Ok(FailureKind::Auth) => "auth",
        Ok(FailureKind::AuthExpired) => "auth_expired",
        Ok(FailureKind::AuthNotConfigured) => "auth_not_configured",
        Ok(FailureKind::Tls) => "tls",
        Ok(FailureKind::Network) => "network",
        Ok(FailureKind::Config) => "config",
        Ok(FailureKind::ModelNotFound) => "model_not_found",
        Ok(FailureKind::Gateway404CheckBaseUrl) => "gateway_404_check_base_url",
        Ok(FailureKind::QuotaExhausted) => "quota_exhausted",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astra_proto::astra::engine::v1::connectivity_service_server::{
        ConnectivityService, ConnectivityServiceServer,
    };
    use astra_proto::astra::engine::v1::{
        InvalidateRequest, InvalidateResponse, ProbeAllResponse, ProbeRequest, ProbeResponse,
        ReloadEnvRequest, ReloadEnvResponse, StatusRequest, StatusResponse,
    };
    use tonic::{Request, Response, Status};

    struct MockConnectivity {
        rows: Vec<ConnectivityStatus>,
    }

    #[tonic::async_trait]
    impl ConnectivityService for MockConnectivity {
        async fn status(
            &self,
            _: Request<StatusRequest>,
        ) -> Result<Response<StatusResponse>, Status> {
            Ok(Response::new(StatusResponse {
                statuses: self.rows.clone(),
            }))
        }

        async fn probe(&self, _: Request<ProbeRequest>) -> Result<Response<ProbeResponse>, Status> {
            Err(Status::unimplemented("probe"))
        }

        async fn probe_all(
            &self,
            _: Request<ProbeAllRequest>,
        ) -> Result<Response<ProbeAllResponse>, Status> {
            Ok(Response::new(ProbeAllResponse {
                rows: self.rows.clone(),
            }))
        }

        async fn invalidate(
            &self,
            _: Request<InvalidateRequest>,
        ) -> Result<Response<InvalidateResponse>, Status> {
            Ok(Response::new(InvalidateResponse {}))
        }

        async fn reload_env(
            &self,
            _: Request<ReloadEnvRequest>,
        ) -> Result<Response<ReloadEnvResponse>, Status> {
            Ok(Response::new(ReloadEnvResponse {}))
        }
    }

    #[tokio::test]
    async fn doctor_renders_connectivity_rows_against_mock() {
        let mock = MockConnectivity {
            rows: vec![
                ConnectivityStatus {
                    route: Route::Phase1 as i32,
                    provider: "anthropic".into(),
                    status: RouteStatus::Ok as i32,
                    failure: None,
                },
                ConnectivityStatus {
                    route: Route::Chat as i32,
                    provider: "openai".into(),
                    status: RouteStatus::Error as i32,
                    failure: Some(Failure {
                        kind: FailureKind::AuthExpired as i32,
                        reason: "api key expired".into(),
                        remediation: Some("run `astra auth login`".into()),
                    }),
                },
            ],
        };
        let addr = crate::testutil::spawn(ConnectivityServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        let output = probe_and_render(&channel, "http://test")
            .await
            .expect("probe succeeds");

        assert!(output.contains("anthropic"), "output: {output}");
        assert!(output.contains("openai"), "output: {output}");
        assert!(output.contains("ok"), "output: {output}");
        assert!(output.contains("error"), "output: {output}");
        assert!(output.contains("auth_expired"), "output: {output}");
        assert!(output.contains("api key expired"), "output: {output}");
        assert!(
            output.contains("run `astra auth login`"),
            "output: {output}"
        );
    }

    #[test]
    fn renders_error_row_with_failure_detail() {
        let row = ConnectivityStatus {
            route: Route::Phase2 as i32,
            provider: "openai".into(),
            status: RouteStatus::Error as i32,
            failure: Some(Failure {
                kind: FailureKind::Network as i32,
                reason: "connection refused".into(),
                remediation: None,
            }),
        };
        assert_eq!(
            render_row(&row),
            "phase2 (openai): error — network: connection refused"
        );
    }

    #[test]
    fn renders_ok_row_without_failure() {
        let row = ConnectivityStatus {
            route: Route::Phase3 as i32,
            provider: "anthropic".into(),
            status: RouteStatus::Ok as i32,
            failure: None,
        };
        assert_eq!(render_row(&row), "phase3 (anthropic): ok");
    }
}
