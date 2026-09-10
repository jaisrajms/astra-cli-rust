//! Daemon endpoint resolution and channel construction.
//!
//! The daemon listens on a Unix socket by default (`~/.astra/engine.sock`) or —
//! on Windows — a named pipe (`\\.\pipe\astra-engine`); a
//! `--endpoint`/`ASTRA_ENDPOINT` value may also be an `http(s)://` URI (used by
//! the mock-server tests). A leading `~` in a socket path is expanded.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

/// Production pipe name for the daemon on Windows (mirrors the engine's
/// `astra-daemon::transport::windows_pipe::PIPE_NAME`, Design §3.2).
#[cfg(windows)]
pub const PIPE_NAME: &str = r"\\.\pipe\astra-engine";

/// The `\\.\pipe\` prefix that marks a Windows named-pipe endpoint.
#[cfg(windows)]
const PIPE_PREFIX: &str = r"\\.\pipe\";

/// True when `endpoint` names a Windows named pipe (`\\.\pipe\...`).
///
/// Only the pipe transport is wired up on Windows (`#[cfg(windows)]`), so this
/// helper is gated too; on macOS/Linux a pipe name falls through to the
/// Unix-socket path below.
#[cfg(windows)]
pub fn is_windows_pipe(endpoint: &str) -> bool {
    endpoint.starts_with(PIPE_PREFIX)
}

/// Build a [`Channel`] to the given endpoint.
///
/// `http(s)://` URIs are dialed over TCP; a Windows named pipe
/// (`\\.\pipe\...`) is dialed as a named pipe on Windows; anything else is
/// treated as a Unix socket path (with `~` expanded to `$HOME`).
///
/// Both transports enable HTTP/2 keep-alive while idle so a half-open
/// connection (daemon died without closing) is detected as a stream error
/// rather than a silent hang.
pub fn connect(endpoint: &str) -> anyhow::Result<Channel> {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        let ep = Endpoint::try_from(endpoint.to_string())
            .with_context(|| format!("invalid endpoint URI: {endpoint}"))?
            .http2_keep_alive_interval(Duration::from_secs(30))
            .keep_alive_while_idle(true);
        return Ok(ep.connect_lazy());
    }

    #[cfg(windows)]
    if is_windows_pipe(endpoint) {
        return connect_windows_pipe(endpoint);
    }

    let path = expand_home(endpoint);
    let channel = Endpoint::try_from("http://[::]:50051")
        .context("failed to build the Unix-socket endpoint")?
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_while_idle(true)
        .connect_with_connector_lazy(service_fn(move |_: Uri| {
            let path = path.clone();
            async move {
                let stream = UnixStream::connect(&path).await?;
                Ok::<_, std::io::Error>(TokioIo::new(stream))
            }
        }));
    Ok(channel)
}

/// The workspace root the CLI targets: the `ASTRA_WORKSPACE` env var, else the current directory.
/// The daemon is per-workspace, so every workspace-scoped request must carry this.
pub fn workspace_root() -> String {
    std::env::var("ASTRA_WORKSPACE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}

/// Attach the `workspace-root` gRPC metadata to a request so the daemon resolves the per-workspace
/// engine. Best-effort: a non-ASCII path that can't be encoded as a metadata value is left unset.
pub fn with_workspace<T>(mut req: tonic::Request<T>) -> tonic::Request<T> {
    if let Ok(v) = tonic::metadata::MetadataValue::from_str(&workspace_root()) {
        req.metadata_mut().insert("workspace-root", v);
    }
    req
}

/// Dial a Windows named pipe (`\\.\pipe\...`) via `tokio`'s `NamedPipeClient`.
///
/// Compiled only on Windows; on macOS/Linux named-pipe endpoints fall through to
/// the Unix-socket path above (a pipe name is not a valid socket path there).
#[cfg(windows)]
fn connect_windows_pipe(endpoint: &str) -> anyhow::Result<Channel> {
    let pipe_name = endpoint.to_string();
    let channel = Endpoint::try_from("http://[::]:50051")
        .context("failed to build the named-pipe endpoint")?
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_while_idle(true)
        .connect_with_connector_lazy(service_fn(move |_: Uri| {
            let pipe_name = pipe_name.clone();
            async move {
                let client =
                    tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe_name)?;
                Ok::<_, std::io::Error>(TokioIo::new(client))
            }
        }));
    Ok(channel)
}

/// The resolved daemon endpoint string, with the socket default applied. Mirrors
/// `Cli::endpoint()` (reads `ASTRA_ENDPOINT`, defaults to `~/.astra/engine.sock`)
/// so a handler that doesn't receive the parsed `Cli` can still display it.
pub fn resolved_endpoint() -> String {
    match std::env::var("ASTRA_ENDPOINT") {
        Ok(e) if !e.trim().is_empty() => e,
        _ => "~/.astra/engine.sock".to_string(),
    }
}

/// Expand a leading `~` in a socket path to the user's home directory.
pub fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_prefix() {
        let home = std::env::var("HOME").expect("HOME set in test env");
        let expanded = expand_home("~/.astra/engine.sock");
        assert_eq!(expanded, PathBuf::from(home).join(".astra/engine.sock"));
    }

    #[test]
    fn leaves_relative_paths_untouched() {
        assert_eq!(expand_home("./engine.sock"), PathBuf::from("./engine.sock"));
        assert_eq!(expand_home("engine.sock"), PathBuf::from("engine.sock"));
    }

    #[tokio::test]
    async fn recognizes_http_endpoints() {
        assert!(connect("http://127.0.0.1:50051").is_ok());
    }

    // Windows named-pipe transport: only compiled on Windows, where the pipe
    // connector is available. On macOS/Linux these tests are compiled out (the
    // `#[cfg(windows)]` items above are not present), which keeps the Unix build
    // warning-free. A live round-trip is verified on a real Windows host only.
    #[cfg(windows)]
    #[test]
    fn detects_windows_pipe_prefix() {
        assert!(is_windows_pipe(r"\\.\pipe\astra-engine"));
        assert!(!is_windows_pipe("~/.astra/engine.sock"));
        assert!(!is_windows_pipe("http://127.0.0.1:50051"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn dials_windows_named_pipe() {
        assert!(connect(PIPE_NAME).is_ok());
    }
}
