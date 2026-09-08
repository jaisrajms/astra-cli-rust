//! Daemon endpoint resolution and channel construction.
//!
//! The daemon listens on a Unix socket by default (`~/.astra/engine.sock`); a
//! `--endpoint`/`ASTRA_ENDPOINT` value may also be an `http(s)://` URI (used by
//! the mock-server tests). A leading `~` in a socket path is expanded.
//!
//! NOTE: the Windows named-pipe endpoint (`\\.\pipe\astra-engine`) is NOT
//! implemented here yet — anything that is not an `http(s)://` URI is dialed as
//! a Unix socket, which is not how a Windows pipe is reached. Windows support
//! lands with the daemon's pipe transport (Phase I); this gap is intentional.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

/// Build a [`Channel`] to the given endpoint.
///
/// `http(s)://` URIs are dialed over TCP; anything else is treated as a Unix
/// socket path (with `~` expanded to `$HOME`).
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
}
