//! `astra serve` — start the headless daemon (`astrad`).
//!
//! The astra daemon owns the server socket (`~/.astra/engine.sock` on Unix,
//! `\\.\pipe\astra-engine` on Windows) and serves the gRPC surface
//! (`SessionService`, `ChatService`, `FleetService`, …). `astra serve` therefore
//! does not start a second server — it locates the `astrad` binary, starts it in
//! the background if it isn't already running, and reports the daemon endpoint
//! and pid.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::Context;
use clap::Args;

#[derive(Args)]
pub struct ServeArgs {
    /// Listen host for the headless server.
    ///
    /// Advisory only: the daemon binds a Unix socket (or Windows named pipe);
    /// these values take effect once a TCP gateway exists.
    #[arg(long, default_value = "127.0.0.1", value_name = "HOST")]
    pub host: String,
    /// Listen port.
    ///
    /// Advisory only (see `--host`).
    #[arg(long, default_value_t = 4096, value_name = "PORT")]
    pub port: u16,
}

pub async fn handle(args: ServeArgs) -> anyhow::Result<()> {
    let endpoint = crate::endpoint::resolved_endpoint();
    let bin = resolve_astrad_bin();
    let action = decide(bin.clone(), running_astrad(), bin.is_file());

    match action {
        ServeAction::AlreadyRunning { pid } => {
            println!("astrad: already running (pid {pid})");
            println!("daemon endpoint: {endpoint}");
        }
        ServeAction::Spawn { bin } => {
            let child = spawn_astrad(&bin)?;
            println!("astrad: started (pid {})", child.id());
            println!("daemon endpoint: {endpoint}");
        }
        ServeAction::MissingBinary { bin } => {
            anyhow::bail!(
                "astrad is not built at {} — build it first (`make launch` or `cargo build --bin astrad` in the engine repo).",
                bin.display()
            );
        }
    }

    println!(
        "note: --host {host} --port {port} are advisory until a TCP gateway exists; the daemon binds the engine socket ({endpoint}).",
        host = args.host,
        port = args.port
    );
    Ok(())
}

/// The action `serve` should take, given the binary path, whether a daemon is
/// already running, and whether the binary exists.
///
/// Pure and side-effect-free so the "already running" and "binary missing"
/// branches are unit-testable without touching the process table or filesystem.
#[derive(Debug, PartialEq, Eq)]
enum ServeAction {
    AlreadyRunning { pid: u32 },
    Spawn { bin: PathBuf },
    MissingBinary { bin: PathBuf },
}

fn decide(bin: PathBuf, running: Option<u32>, bin_exists: bool) -> ServeAction {
    if let Some(pid) = running {
        ServeAction::AlreadyRunning { pid }
    } else if bin_exists {
        ServeAction::Spawn { bin }
    } else {
        ServeAction::MissingBinary { bin }
    }
}

/// Resolve the `astrad` binary: `$ASTRAD_BIN` if set, else the sibling engine
/// repo's debug build.
fn resolve_astrad_bin() -> PathBuf {
    resolve_astrad_bin_from(std::env::var_os("ASTRAD_BIN"))
}

/// Pure form of [`resolve_astrad_bin`] so both branches are testable.
fn resolve_astrad_bin_from(env_override: Option<OsString>) -> PathBuf {
    match env_override {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from("../astra-engine-rust/target/debug/astrad"),
    }
}

/// The pid of a running `astrad`, if any (`pgrep -x astrad`).
fn running_astrad() -> Option<u32> {
    let output = std::process::Command::new("pgrep")
        .args(["-x", "astrad"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let first = String::from_utf8_lossy(&output.stdout);
    first.lines().next()?.trim().parse::<u32>().ok()
}

/// Spawn `astrad` in the background, redirecting its output to `/tmp/astrad.log`
/// (matching `make launch`). The child keeps running after this process exits.
fn spawn_astrad(bin: &PathBuf) -> anyhow::Result<std::process::Child> {
    use std::process::Stdio;

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/astrad.log")
        .context("failed to open /tmp/astrad.log for daemon output")?;
    let log_err = log
        .try_clone()
        .context("failed to clone the daemon log handle")?;

    std::process::Command::new(bin)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .with_context(|| format!("failed to spawn `{}`", bin.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_astrad_bin_from_env_override() {
        assert_eq!(
            resolve_astrad_bin_from(Some(OsString::from("/custom/astrad"))),
            PathBuf::from("/custom/astrad")
        );
    }

    #[test]
    fn resolves_astrad_bin_default() {
        assert_eq!(
            resolve_astrad_bin_from(None),
            PathBuf::from("../astra-engine-rust/target/debug/astrad")
        );
    }

    #[test]
    fn decides_already_running_first() {
        let action = decide(PathBuf::from("/missing/astrad"), Some(4242), false);
        assert_eq!(action, ServeAction::AlreadyRunning { pid: 4242 });
    }

    #[test]
    fn decides_spawn_when_built() {
        let action = decide(PathBuf::from("/x/astrad"), None, true);
        assert_eq!(
            action,
            ServeAction::Spawn {
                bin: PathBuf::from("/x/astrad")
            }
        );
    }

    #[test]
    fn decides_missing_binary_when_not_built() {
        let action = decide(PathBuf::from("/x/astrad"), None, false);
        assert_eq!(
            action,
            ServeAction::MissingBinary {
                bin: PathBuf::from("/x/astrad")
            }
        );
    }
}
