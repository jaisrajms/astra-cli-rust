//! `astra auth` — daemon authentication (thin client).
//!
//! The CLI never reads or writes the daemon's credential files; it prompts the
//! user for the secret material, relays it to the daemon's [`AuthService`]
//! (write-only: raw keys are never returned), and confirms the result through
//! the masked [`AuthService::status`] view.

use std::io::IsTerminal;
use std::time::Duration;

use clap::{Args, Subcommand};
use tonic::transport::Channel;

use astra_proto::astra::engine::v1::auth_service_client::AuthServiceClient;
use astra_proto::astra::engine::v1::{
    AuthStatusRequest, AwsCredentials, ConfiguredProvider, ListProvidersRequest, LoginStartRequest,
    LogoutRequest, PollLoginRequest, ProviderInfo,
};

#[derive(Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: Option<AuthCommand>,
}

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Log in to a provider.
    Login(LoginArgs),
    /// Log out of a provider.
    Logout(LogoutArgs),
    /// Show configured providers (keys masked).
    Status,
}

#[derive(Args)]
pub struct LoginArgs {
    /// Provider ID to log in to (defaults to an interactive choice).
    #[arg(value_name = "PROVIDER")]
    pub provider: Option<String>,

    /// API key (for `api` providers; prompts if omitted).
    #[arg(long, value_name = "KEY")]
    pub key: Option<String>,

    /// Remote server URL (for `remote` providers; prompts if omitted).
    #[arg(long, value_name = "URL")]
    pub server_url: Option<String>,

    /// AWS access key ID (for `aws` providers; prompts if omitted).
    #[arg(long, value_name = "ID")]
    pub aws_access_key_id: Option<String>,

    /// AWS secret access key (for `aws` providers; prompts if omitted).
    #[arg(long, value_name = "SECRET")]
    pub aws_secret_access_key: Option<String>,

    /// AWS session token (optional, for `aws` providers).
    #[arg(long, value_name = "TOKEN")]
    pub aws_session_token: Option<String>,

    /// AWS region (optional, for `aws` providers).
    #[arg(long, value_name = "REGION")]
    pub aws_region: Option<String>,
}

#[derive(Args)]
pub struct LogoutArgs {
    /// Provider ID to log out (defaults to an interactive choice).
    #[arg(value_name = "PROVIDER")]
    pub provider: Option<String>,
}

/// User interaction seam: the real implementation reads the terminal (with echo
/// disabled for secrets) and opens the system browser; tests substitute a fake.
pub trait Prompt: Send {
    fn prompt(&mut self, label: &str) -> std::io::Result<String>;
    fn prompt_secret(&mut self, label: &str) -> std::io::Result<String>;
    fn open_browser(&mut self, url: &str);
}

/// The terminal-backed [`Prompt`].
pub struct StdinPrompt;

impl Prompt for StdinPrompt {
    fn prompt(&mut self, label: &str) -> std::io::Result<String> {
        print!("{label}: ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok(line.trim().to_string())
    }

    fn prompt_secret(&mut self, label: &str) -> std::io::Result<String> {
        print!("{label}: ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let line = if std::io::stdin().is_terminal() {
            read_hidden_line()?
        } else {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line
        };
        println!();
        Ok(line.trim().to_string())
    }

    fn open_browser(&mut self, url: &str) {
        open_browser(url);
    }
}

pub async fn handle(args: AuthArgs, channel: Channel) -> anyhow::Result<()> {
    handle_with(args, channel, StdinPrompt).await
}

/// Testable entry point: identical to [`handle`] but with an injected [`Prompt`].
pub async fn handle_with(
    args: AuthArgs,
    channel: Channel,
    mut prompt: impl Prompt,
) -> anyhow::Result<()> {
    match args.command.unwrap_or(AuthCommand::Status) {
        AuthCommand::Login(a) => login(a, channel, &mut prompt).await,
        AuthCommand::Logout(a) => logout(a, channel, &mut prompt).await,
        AuthCommand::Status => {
            let mut client = AuthServiceClient::new(channel);
            print!("{}", status_render(&mut client).await?);
            Ok(())
        }
    }
}

async fn login(args: LoginArgs, channel: Channel, prompt: &mut impl Prompt) -> anyhow::Result<()> {
    let mut client = AuthServiceClient::new(channel);

    let (provider, method) = resolve_login_target(&mut client, &args, prompt).await?;

    let mut req = LoginStartRequest {
        provider,
        api_key: None,
        aws: None,
        server_url: None,
    };

    match method.as_str() {
        "api" => {
            let key = match args.key {
                Some(k) => k,
                None => prompt.prompt_secret("API key")?,
            };
            req.api_key = Some(key);
        }
        "oauth" => {}
        "aws" => {
            let access_key_id = match args.aws_access_key_id {
                Some(v) => v,
                None => prompt.prompt("AWS access key ID")?,
            };
            let secret_access_key = match args.aws_secret_access_key {
                Some(v) => v,
                None => prompt.prompt_secret("AWS secret access key")?,
            };
            let session_token = optional_flag_or_prompt(
                args.aws_session_token,
                "AWS session token (optional)",
                prompt,
            )?;
            let region = optional_flag_or_prompt(args.aws_region, "AWS region (optional)", prompt)?;
            req.aws = Some(AwsCredentials {
                access_key_id,
                secret_access_key,
                session_token,
                region,
            });
        }
        "remote" => {
            let url = match args.server_url {
                Some(u) => u,
                None => prompt.prompt("Server URL")?,
            };
            req.server_url = Some(url);
        }
        other => anyhow::bail!("unsupported auth method `{other}`"),
    }

    let resp = client.login_start(req).await?.into_inner();

    if resp.status == "error" {
        let msg = resp.error.unwrap_or_else(|| "unknown error".into());
        anyhow::bail!("login failed: {msg}");
    }

    if method == "oauth" {
        if let Some(code) = &resp.user_code {
            println!("user code: {code}");
        }
        if let Some(url) = &resp.verification_url {
            println!("verification URL: {url}");
            prompt.open_browser(url);
        }
        poll_until_complete(&mut client, &resp.login_id).await?;
    }

    let rendered = status_render(&mut client).await?;
    print!("{rendered}");
    Ok(())
}

async fn logout(
    args: LogoutArgs,
    channel: Channel,
    prompt: &mut impl Prompt,
) -> anyhow::Result<()> {
    let mut client = AuthServiceClient::new(channel);

    let provider = match args.provider {
        Some(p) => p,
        None => {
            let configured = client
                .status(AuthStatusRequest {})
                .await?
                .into_inner()
                .providers;
            if configured.is_empty() {
                anyhow::bail!("no providers are currently configured");
            }
            if configured.len() == 1 {
                configured[0].id.clone()
            } else {
                pick_configured_provider(&configured, prompt)?
            }
        }
    };

    client
        .logout(LogoutRequest {
            provider: provider.clone(),
        })
        .await?;
    println!("logged out of {provider}");
    Ok(())
}

/// Resolve the provider + auth method for a login, prompting as needed.
async fn resolve_login_target(
    client: &mut AuthServiceClient<Channel>,
    args: &LoginArgs,
    prompt: &mut impl Prompt,
) -> anyhow::Result<(String, String)> {
    let method_from_flags = method_from_flags(args);

    // Only need the catalog when the provider or its method isn't already known.
    let need_catalog = args.provider.is_none() || method_from_flags.is_none();
    let providers = if need_catalog {
        client
            .list_providers(ListProvidersRequest {})
            .await?
            .into_inner()
            .providers
    } else {
        Vec::new()
    };

    let provider = match &args.provider {
        Some(p) => {
            if !providers.is_empty() && !providers.iter().any(|i| &i.id == p) {
                anyhow::bail!("unknown provider `{p}`");
            }
            p.clone()
        }
        None => {
            let candidates: Vec<&ProviderInfo> = match method_from_flags {
                Some(m) => providers.iter().filter(|i| i.auth_method == m).collect(),
                None => providers.iter().collect(),
            };
            if candidates.is_empty() {
                anyhow::bail!("no providers available to log in to");
            }
            if candidates.len() == 1 {
                candidates[0].id.clone()
            } else {
                pick_provider(&candidates, prompt)?
            }
        }
    };

    let method = match method_from_flags {
        Some(m) => m.to_string(),
        None => providers
            .iter()
            .find(|i| i.id == provider)
            .map(|i| i.auth_method.clone())
            .ok_or_else(|| anyhow::anyhow!("unknown provider `{provider}`"))?,
    };

    Ok((provider, method))
}

fn method_from_flags(args: &LoginArgs) -> Option<&'static str> {
    if args.key.is_some() {
        Some("api")
    } else if args.aws_access_key_id.is_some()
        || args.aws_secret_access_key.is_some()
        || args.aws_session_token.is_some()
        || args.aws_region.is_some()
    {
        Some("aws")
    } else if args.server_url.is_some() {
        Some("remote")
    } else {
        None
    }
}

/// Resolve an optional AWS flag: use the flag if set (non-empty), else prompt.
fn optional_flag_or_prompt(
    flag: Option<String>,
    label: &str,
    prompt: &mut impl Prompt,
) -> anyhow::Result<Option<String>> {
    match flag {
        Some(v) if !v.trim().is_empty() => Ok(Some(v)),
        Some(_) => Ok(None),
        None => {
            let v = prompt.prompt(label)?;
            if v.trim().is_empty() {
                Ok(None)
            } else {
                Ok(Some(v))
            }
        }
    }
}

/// Print a numbered provider menu and return the chosen id.
fn pick_provider(candidates: &[&ProviderInfo], prompt: &mut impl Prompt) -> anyhow::Result<String> {
    let ids: Vec<String> = candidates.iter().map(|p| p.id.clone()).collect();
    let methods: Vec<String> = candidates.iter().map(|p| p.auth_method.clone()).collect();
    let index = pick_index(&ids, &methods, prompt)?;
    Ok(ids[index].clone())
}

fn pick_configured_provider(
    candidates: &[ConfiguredProvider],
    prompt: &mut impl Prompt,
) -> anyhow::Result<String> {
    let ids: Vec<String> = candidates.iter().map(|p| p.id.clone()).collect();
    let methods: Vec<String> = candidates.iter().map(|p| p.auth_method.clone()).collect();
    let index = pick_index(&ids, &methods, prompt)?;
    Ok(ids[index].clone())
}

fn pick_index(
    ids: &[String],
    methods: &[String],
    prompt: &mut impl Prompt,
) -> anyhow::Result<usize> {
    for (i, id) in ids.iter().enumerate() {
        let method = methods.get(i).map(String::as_str).unwrap_or("");
        println!("  {}: {id} ({method})", i + 1);
    }
    loop {
        let answer = prompt.prompt("provider")?;
        if let Some(i) = ids.iter().position(|id| id == &answer) {
            return Ok(i);
        }
        if let Ok(n) = answer.parse::<usize>() {
            if (1..=ids.len()).contains(&n) {
                return Ok(n - 1);
            }
        }
        println!("invalid choice; enter a number or provider id");
    }
}

async fn poll_until_complete(
    client: &mut AuthServiceClient<Channel>,
    login_id: &str,
) -> anyhow::Result<()> {
    loop {
        let resp = client
            .poll_login(PollLoginRequest {
                login_id: login_id.to_string(),
            })
            .await?
            .into_inner();
        match resp.status.as_str() {
            "complete" => return Ok(()),
            "pending" => tokio::time::sleep(Duration::from_secs(2)).await,
            "error" | "expired" => {
                let msg = resp.error.unwrap_or_default();
                anyhow::bail!("login {}: {msg}", resp.status);
            }
            other => anyhow::bail!("unexpected login poll status `{other}`"),
        }
    }
}

/// Call [`AuthService::status`] and render the masked provider table.
async fn status_render(client: &mut AuthServiceClient<Channel>) -> anyhow::Result<String> {
    let resp = client.status(AuthStatusRequest {}).await?.into_inner();
    if resp.providers.is_empty() {
        return Ok("no providers configured\n".to_string());
    }
    Ok(format_status_table(&resp.providers))
}

fn format_status_table(providers: &[ConfiguredProvider]) -> String {
    let rows: Vec<Vec<String>> = providers
        .iter()
        .map(|p| {
            vec![
                p.id.clone(),
                p.auth_method.clone(),
                p.masked_key.clone().unwrap_or_else(|| "-".into()),
                p.source.clone().unwrap_or_else(|| "-".into()),
            ]
        })
        .collect();
    crate::render::table_string(&["Provider", "Method", "Key", "Source"], &rows)
}

/// Best-effort browser open (`open` on macOS, `xdg-open` on Linux, `cmd` on Windows).
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(windows)]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    let _ = result;
}

/// Read a line with terminal echo disabled (Unix); plain read elsewhere.
#[cfg(unix)]
fn read_hidden_line() -> std::io::Result<String> {
    use std::os::fd::AsRawFd;
    use termios::{tcsetattr, Termios, ECHO, TCSANOW};

    let fd = std::io::stdin().as_raw_fd();
    let saved = Termios::from_fd(fd)?;
    let mut hidden = saved;
    hidden.c_lflag &= !ECHO;
    tcsetattr(fd, TCSANOW, &hidden)?;

    let mut line = String::new();
    let result = std::io::stdin().read_line(&mut line);
    let _ = tcsetattr(fd, TCSANOW, &saved);
    result?;
    Ok(line)
}

#[cfg(not(unix))]
fn read_hidden_line() -> std::io::Result<String> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use astra_proto::astra::engine::v1::auth_service_server::{AuthService, AuthServiceServer};
    use astra_proto::astra::engine::v1::{
        AuthStatusResponse, ListProvidersResponse, LoginCancelRequest, LoginCancelResponse,
        LoginStartResponse, LogoutResponse, PollLoginResponse,
    };
    use tonic::{Request, Response, Status};

    struct FakePrompt {
        answers: VecDeque<String>,
        browsers: Arc<Mutex<Vec<String>>>,
    }

    impl FakePrompt {
        fn new(answers: Vec<&str>) -> Self {
            Self {
                answers: answers.into_iter().map(String::from).collect(),
                browsers: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl Prompt for FakePrompt {
        fn prompt(&mut self, _label: &str) -> std::io::Result<String> {
            Ok(self.answers.pop_front().unwrap_or_default())
        }
        fn prompt_secret(&mut self, _label: &str) -> std::io::Result<String> {
            Ok(self.answers.pop_front().unwrap_or_default())
        }
        fn open_browser(&mut self, url: &str) {
            self.browsers.lock().unwrap().push(url.to_string());
        }
    }

    #[derive(Default)]
    struct Captured {
        login_start: Option<LoginStartRequest>,
        poll_login_ids: Vec<String>,
        logout: Option<LogoutRequest>,
    }

    struct MockAuth {
        providers: Vec<ProviderInfo>,
        configured: Vec<ConfiguredProvider>,
        login_start_resp: LoginStartResponse,
        poll_resp: PollLoginResponse,
        captured: Arc<Mutex<Captured>>,
    }

    #[tonic::async_trait]
    impl AuthService for MockAuth {
        async fn list_providers(
            &self,
            _: Request<ListProvidersRequest>,
        ) -> Result<Response<ListProvidersResponse>, Status> {
            Ok(Response::new(ListProvidersResponse {
                providers: self.providers.clone(),
            }))
        }

        async fn login_start(
            &self,
            request: Request<LoginStartRequest>,
        ) -> Result<Response<LoginStartResponse>, Status> {
            self.captured.lock().unwrap().login_start = Some(request.into_inner());
            Ok(Response::new(self.login_start_resp.clone()))
        }

        async fn poll_login(
            &self,
            request: Request<PollLoginRequest>,
        ) -> Result<Response<PollLoginResponse>, Status> {
            let req = request.into_inner();
            self.captured
                .lock()
                .unwrap()
                .poll_login_ids
                .push(req.login_id);
            Ok(Response::new(self.poll_resp.clone()))
        }

        async fn login_cancel(
            &self,
            _: Request<LoginCancelRequest>,
        ) -> Result<Response<LoginCancelResponse>, Status> {
            Err(Status::unimplemented("login_cancel"))
        }

        async fn logout(
            &self,
            request: Request<LogoutRequest>,
        ) -> Result<Response<LogoutResponse>, Status> {
            self.captured.lock().unwrap().logout = Some(request.into_inner());
            Ok(Response::new(LogoutResponse {}))
        }

        async fn status(
            &self,
            _: Request<AuthStatusRequest>,
        ) -> Result<Response<AuthStatusResponse>, Status> {
            Ok(Response::new(AuthStatusResponse {
                providers: self.configured.clone(),
            }))
        }
    }

    fn default_login_args() -> LoginArgs {
        LoginArgs {
            provider: None,
            key: None,
            server_url: None,
            aws_access_key_id: None,
            aws_secret_access_key: None,
            aws_session_token: None,
            aws_region: None,
        }
    }

    #[tokio::test]
    async fn status_renders_masked_providers() {
        let mock = MockAuth {
            providers: vec![],
            configured: vec![
                ConfiguredProvider {
                    id: "anthropic".into(),
                    auth_method: "api".into(),
                    masked_key: Some("sk-ant-…abc123".into()),
                    source: Some("config".into()),
                    account_id: None,
                },
                ConfiguredProvider {
                    id: "bedrock".into(),
                    auth_method: "aws".into(),
                    masked_key: Some("AKIA…XYZ9".into()),
                    source: Some("env".into()),
                    account_id: Some("123456789012".into()),
                },
            ],
            login_start_resp: LoginStartResponse {
                login_id: String::new(),
                verification_url: None,
                user_code: None,
                status: "complete".into(),
                error: None,
            },
            poll_resp: PollLoginResponse {
                status: "complete".into(),
                error: None,
            },
            captured: Arc::new(Mutex::new(Captured::default())),
        };
        let addr = crate::testutil::spawn(AuthServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        let mut client = AuthServiceClient::new(channel);
        let out = status_render(&mut client).await.expect("status renders");

        assert!(out.contains("anthropic"), "output: {out}");
        assert!(out.contains("sk-ant-…abc123"), "output: {out}");
        assert!(out.contains("config"), "output: {out}");
        assert!(out.contains("bedrock"), "output: {out}");
        assert!(out.contains("AKIA…XYZ9"), "output: {out}");
        assert!(out.contains("env"), "output: {out}");
        assert!(
            !out.contains("123456789012"),
            "full account id leaks: {out}"
        );
    }

    #[tokio::test]
    async fn login_api_forwards_api_key() {
        let captured = Arc::new(Mutex::new(Captured::default()));
        let mock = MockAuth {
            providers: vec![],
            configured: vec![ConfiguredProvider {
                id: "anthropic".into(),
                auth_method: "api".into(),
                masked_key: Some("sk-ant-…abcd".into()),
                source: Some("store".into()),
                account_id: None,
            }],
            login_start_resp: LoginStartResponse {
                login_id: "l-1".into(),
                verification_url: None,
                user_code: None,
                status: "complete".into(),
                error: None,
            },
            poll_resp: PollLoginResponse {
                status: "complete".into(),
                error: None,
            },
            captured: captured.clone(),
        };
        let addr = crate::testutil::spawn(AuthServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        let mut args = default_login_args();
        args.provider = Some("anthropic".into());
        args.key = Some("sk-ant-secret".into());
        handle_with(
            AuthArgs {
                command: Some(AuthCommand::Login(args)),
            },
            channel,
            FakePrompt::new(vec![]),
        )
        .await
        .expect("login succeeds");

        let req = captured
            .lock()
            .unwrap()
            .login_start
            .clone()
            .expect("login_start captured");
        assert_eq!(req.provider, "anthropic");
        assert_eq!(req.api_key.as_deref(), Some("sk-ant-secret"));
        assert!(req.aws.is_none());
        assert!(req.server_url.is_none());
    }

    #[tokio::test]
    async fn login_aws_forwards_credentials() {
        let captured = Arc::new(Mutex::new(Captured::default()));
        let mock = MockAuth {
            providers: vec![],
            configured: vec![ConfiguredProvider {
                id: "bedrock".into(),
                auth_method: "aws".into(),
                masked_key: Some("AKIA…XYZ9".into()),
                source: Some("env".into()),
                account_id: None,
            }],
            login_start_resp: LoginStartResponse {
                login_id: "l-aws".into(),
                verification_url: None,
                user_code: None,
                status: "complete".into(),
                error: None,
            },
            poll_resp: PollLoginResponse {
                status: "complete".into(),
                error: None,
            },
            captured: captured.clone(),
        };
        let addr = crate::testutil::spawn(AuthServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        let mut args = default_login_args();
        args.provider = Some("bedrock".into());
        args.aws_access_key_id = Some("AKIAID".into());
        args.aws_secret_access_key = Some("secret".into());
        args.aws_region = Some("us-east-1".into());
        handle_with(
            AuthArgs {
                command: Some(AuthCommand::Login(args)),
            },
            channel,
            FakePrompt::new(vec![]),
        )
        .await
        .expect("login succeeds");

        let req = captured
            .lock()
            .unwrap()
            .login_start
            .clone()
            .expect("login_start captured");
        let aws = req.aws.expect("aws credentials set");
        assert_eq!(aws.access_key_id, "AKIAID");
        assert_eq!(aws.secret_access_key, "secret");
        assert_eq!(aws.region.as_deref(), Some("us-east-1"));
        assert_eq!(aws.session_token, None);
        assert!(req.api_key.is_none());
        assert!(req.server_url.is_none());
    }

    #[tokio::test]
    async fn login_remote_forwards_server_url() {
        let captured = Arc::new(Mutex::new(Captured::default()));
        let mock = MockAuth {
            providers: vec![],
            configured: vec![ConfiguredProvider {
                id: "remote".into(),
                auth_method: "remote".into(),
                masked_key: None,
                source: Some("config".into()),
                account_id: None,
            }],
            login_start_resp: LoginStartResponse {
                login_id: "l-remote".into(),
                verification_url: None,
                user_code: None,
                status: "complete".into(),
                error: None,
            },
            poll_resp: PollLoginResponse {
                status: "complete".into(),
                error: None,
            },
            captured: captured.clone(),
        };
        let addr = crate::testutil::spawn(AuthServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        let mut args = default_login_args();
        args.provider = Some("remote".into());
        args.server_url = Some("https://example.com/v1".into());
        handle_with(
            AuthArgs {
                command: Some(AuthCommand::Login(args)),
            },
            channel,
            FakePrompt::new(vec![]),
        )
        .await
        .expect("login succeeds");

        let req = captured
            .lock()
            .unwrap()
            .login_start
            .clone()
            .expect("login_start captured");
        assert_eq!(req.server_url.as_deref(), Some("https://example.com/v1"));
        assert!(req.api_key.is_none());
        assert!(req.aws.is_none());
    }

    #[tokio::test]
    async fn login_oauth_polls_and_opens_browser() {
        let captured = Arc::new(Mutex::new(Captured::default()));
        let mock = MockAuth {
            providers: vec![ProviderInfo {
                id: "github".into(),
                auth_method: "oauth".into(),
                models: vec![],
            }],
            configured: vec![ConfiguredProvider {
                id: "github".into(),
                auth_method: "oauth".into(),
                masked_key: None,
                source: Some("store".into()),
                account_id: Some("user1".into()),
            }],
            login_start_resp: LoginStartResponse {
                login_id: "l-oauth".into(),
                verification_url: Some("https://example.com/device".into()),
                user_code: Some("ABCD-1234".into()),
                status: "pending".into(),
                error: None,
            },
            poll_resp: PollLoginResponse {
                status: "complete".into(),
                error: None,
            },
            captured: captured.clone(),
        };
        let addr = crate::testutil::spawn(AuthServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        let mut args = default_login_args();
        args.provider = Some("github".into());
        let prompt = FakePrompt::new(vec![]);
        let browsers = prompt.browsers.clone();
        handle_with(
            AuthArgs {
                command: Some(AuthCommand::Login(args)),
            },
            channel,
            prompt,
        )
        .await
        .expect("oauth login succeeds");

        let captured = captured.lock().unwrap();
        assert_eq!(captured.poll_login_ids, vec!["l-oauth".to_string()]);
        assert_eq!(
            *browsers.lock().unwrap(),
            vec!["https://example.com/device".to_string()]
        );
        let req = captured.login_start.as_ref().expect("login_start captured");
        assert!(req.api_key.is_none());
        assert!(req.aws.is_none());
        assert!(req.server_url.is_none());
    }

    #[tokio::test]
    async fn login_without_provider_prompts_choice() {
        let captured = Arc::new(Mutex::new(Captured::default()));
        let mock = MockAuth {
            providers: vec![
                ProviderInfo {
                    id: "anthropic".into(),
                    auth_method: "api".into(),
                    models: vec!["claude-sonnet".into()],
                },
                ProviderInfo {
                    id: "openai".into(),
                    auth_method: "api".into(),
                    models: vec!["gpt-4o".into()],
                },
            ],
            configured: vec![ConfiguredProvider {
                id: "openai".into(),
                auth_method: "api".into(),
                masked_key: Some("sk-…1234".into()),
                source: Some("store".into()),
                account_id: None,
            }],
            login_start_resp: LoginStartResponse {
                login_id: "l-2".into(),
                verification_url: None,
                user_code: None,
                status: "complete".into(),
                error: None,
            },
            poll_resp: PollLoginResponse {
                status: "complete".into(),
                error: None,
            },
            captured: captured.clone(),
        };
        let addr = crate::testutil::spawn(AuthServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle_with(
            AuthArgs {
                command: Some(AuthCommand::Login(default_login_args())),
            },
            channel,
            FakePrompt::new(vec!["2", "sk-openai-key"]),
        )
        .await
        .expect("login succeeds");

        let req = captured
            .lock()
            .unwrap()
            .login_start
            .clone()
            .expect("login_start captured");
        assert_eq!(req.provider, "openai");
        assert_eq!(req.api_key.as_deref(), Some("sk-openai-key"));
    }

    #[tokio::test]
    async fn logout_forwards_provider() {
        let captured = Arc::new(Mutex::new(Captured::default()));
        let mock = MockAuth {
            providers: vec![],
            configured: vec![],
            login_start_resp: LoginStartResponse {
                login_id: String::new(),
                verification_url: None,
                user_code: None,
                status: "complete".into(),
                error: None,
            },
            poll_resp: PollLoginResponse {
                status: "complete".into(),
                error: None,
            },
            captured: captured.clone(),
        };
        let addr = crate::testutil::spawn(AuthServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle_with(
            AuthArgs {
                command: Some(AuthCommand::Logout(LogoutArgs {
                    provider: Some("anthropic".into()),
                })),
            },
            channel,
            FakePrompt::new(vec![]),
        )
        .await
        .expect("logout succeeds");

        let req = captured
            .lock()
            .unwrap()
            .logout
            .clone()
            .expect("logout captured");
        assert_eq!(req.provider, "anthropic");
    }

    #[test]
    fn format_status_table_uses_placeholder_for_missing_secrets() {
        let out = format_status_table(&[ConfiguredProvider {
            id: "openai".into(),
            auth_method: "api".into(),
            masked_key: None,
            source: None,
            account_id: None,
        }]);
        assert!(out.contains("-"), "output: {out}");
        assert!(out.contains("openai"), "output: {out}");
    }

    #[test]
    fn method_flags_win_over_provider_catalog() {
        let mut args = default_login_args();
        args.provider = Some("x".into());
        args.key = Some("k".into());
        assert_eq!(method_from_flags(&args), Some("api"));
    }
}
