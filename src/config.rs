//! `astra config` — daemon configuration (thin client).
//!
//! All configuration is owned by the daemon; the CLI never reads or writes
//! `~/.astra/config.json` itself. It relays `GetConfig`/`SetConfig` over the
//! daemon's [`ConfigService`].

use anyhow::Context;
use clap::{Args, Subcommand};
use tonic::transport::Channel;

use astra_proto::astra::engine::v1::config_service_client::ConfigServiceClient;
use astra_proto::astra::engine::v1::{GetConfigRequest, SetConfigRequest};

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: Option<ConfigCommand>,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Print the full resolved config as JSON.
    List,
    /// Print a single config value by dot-path (e.g. `provider.openai.baseUrl`).
    Get(GetArgs),
    /// Set a config value by dot-path.
    Set(SetArgs),
}

#[derive(Args)]
pub struct GetArgs {
    /// Dot-path key, e.g. `model` or `provider.openai.baseUrl`.
    #[arg(value_name = "KEY")]
    pub key: String,
}

#[derive(Args)]
pub struct SetArgs {
    /// Dot-path key, e.g. `model` or `provider.openai.baseUrl`.
    #[arg(value_name = "KEY")]
    pub key: String,

    /// Value to set, JSON-encoded. Plain words are stored as strings; pass
    /// numbers/bools/objects as JSON (e.g. `42`, `true`, `{"a":1}`).
    #[arg(value_name = "VALUE")]
    pub value: String,
}

pub async fn handle(args: ConfigArgs, channel: Channel) -> anyhow::Result<()> {
    match args.command.unwrap_or(ConfigCommand::List) {
        ConfigCommand::List => {
            let mut client = ConfigServiceClient::new(channel);
            println!("{}", list_render(&mut client).await?);
            Ok(())
        }
        ConfigCommand::Get(g) => {
            let mut client = ConfigServiceClient::new(channel);
            println!("{}", get_render(&mut client, &g.key).await?);
            Ok(())
        }
        ConfigCommand::Set(s) => {
            let value = encode_value(&s.value);
            let mut client = ConfigServiceClient::new(channel);
            client
                .set_config(SetConfigRequest { key: s.key, value })
                .await?;
            Ok(())
        }
    }
}

/// Fetch the full config and pretty-print it.
async fn list_render(client: &mut ConfigServiceClient<Channel>) -> anyhow::Result<String> {
    let json = client
        .get_config(GetConfigRequest {})
        .await?
        .into_inner()
        .config_json;
    pretty_json(&json)
}

/// Fetch the config and print the single value at `key`.
async fn get_render(
    client: &mut ConfigServiceClient<Channel>,
    key: &str,
) -> anyhow::Result<String> {
    let json = client
        .get_config(GetConfigRequest {})
        .await?
        .into_inner()
        .config_json;
    lookup(&json, key)
}

/// Pretty-print a JSON string (a no-op rewrite that also validates it).
fn pretty_json(json: &str) -> anyhow::Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(json).context("daemon returned invalid JSON config")?;
    serde_json::to_string_pretty(&value).context("failed to pretty-print config")
}

/// Resolve a dot-path in a JSON config document to its rendered value.
fn lookup(json: &str, key: &str) -> anyhow::Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(json).context("daemon returned invalid JSON config")?;
    let mut current = &value;
    for part in key.split('.').filter(|p| !p.is_empty()) {
        current = current
            .get(part)
            .ok_or_else(|| anyhow::anyhow!("no such config key `{key}`"))?;
    }
    Ok(render_value(current))
}

fn render_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_string(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// JSON-encode a raw CLI value for [`SetConfigRequest::value`]. If the value is
/// already valid JSON it is forwarded verbatim (so `42`, `true`, `{"a":1}` keep
/// their types); otherwise it is wrapped as a JSON string.
fn encode_value(raw: &str) -> String {
    if raw.parse::<serde_json::Value>().is_ok() {
        raw.to_string()
    } else {
        serde_json::Value::String(raw.to_string()).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use astra_proto::astra::engine::v1::config_service_server::{
        ConfigService, ConfigServiceServer,
    };
    use astra_proto::astra::engine::v1::{GetConfigResponse, SetConfigResponse};
    use tonic::{Request, Response, Status};

    struct MockConfig {
        config_json: String,
        captured: Arc<Mutex<Option<SetConfigRequest>>>,
    }

    #[tonic::async_trait]
    impl ConfigService for MockConfig {
        async fn get_config(
            &self,
            _: Request<GetConfigRequest>,
        ) -> Result<Response<GetConfigResponse>, Status> {
            Ok(Response::new(GetConfigResponse {
                config_json: self.config_json.clone(),
            }))
        }

        async fn set_config(
            &self,
            request: Request<SetConfigRequest>,
        ) -> Result<Response<SetConfigResponse>, Status> {
            *self.captured.lock().unwrap() = Some(request.into_inner());
            Ok(Response::new(SetConfigResponse {}))
        }
    }

    const SAMPLE_CONFIG: &str = r#"{"model":"claude-sonnet","provider":{"openai":{"baseUrl":"https://api.openai.com"}},"retries":3}"#;

    #[tokio::test]
    async fn list_renders_pretty_json() {
        let mock = MockConfig {
            config_json: SAMPLE_CONFIG.into(),
            captured: Arc::new(Mutex::new(None)),
        };
        let addr = crate::testutil::spawn(ConfigServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        let mut client = ConfigServiceClient::new(channel);
        let out = list_render(&mut client).await.expect("list renders");
        assert!(out.contains("claude-sonnet"), "output: {out}");
        assert!(out.contains("baseUrl"), "output: {out}");
    }

    #[tokio::test]
    async fn get_renders_single_value() {
        let mock = MockConfig {
            config_json: SAMPLE_CONFIG.into(),
            captured: Arc::new(Mutex::new(None)),
        };
        let addr = crate::testutil::spawn(ConfigServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        let mut client = ConfigServiceClient::new(channel);
        let out = get_render(&mut client, "model").await.expect("get renders");
        assert_eq!(out, "claude-sonnet");
    }

    #[tokio::test]
    async fn set_forwards_json_encoded_value() {
        let captured = Arc::new(Mutex::new(None));
        let mock = MockConfig {
            config_json: SAMPLE_CONFIG.into(),
            captured: captured.clone(),
        };
        let addr = crate::testutil::spawn(ConfigServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            ConfigArgs {
                command: Some(ConfigCommand::Set(SetArgs {
                    key: "model".into(),
                    value: "gpt-4o".into(),
                })),
            },
            channel,
        )
        .await
        .expect("set succeeds");

        let req = captured.lock().unwrap().clone().expect("set captured");
        assert_eq!(req.key, "model");
        assert_eq!(req.value, "\"gpt-4o\"");
    }

    #[tokio::test]
    async fn set_forwards_typed_json_verbatim() {
        let captured = Arc::new(Mutex::new(None));
        let mock = MockConfig {
            config_json: SAMPLE_CONFIG.into(),
            captured: captured.clone(),
        };
        let addr = crate::testutil::spawn(ConfigServiceServer::new(mock)).await;
        let channel = crate::testutil::channel(addr);

        handle(
            ConfigArgs {
                command: Some(ConfigCommand::Set(SetArgs {
                    key: "retries".into(),
                    value: "7".into(),
                })),
            },
            channel,
        )
        .await
        .expect("set succeeds");

        let req = captured.lock().unwrap().clone().expect("set captured");
        assert_eq!(req.value, "7");
    }

    #[test]
    fn lookup_resolves_nested_dot_paths() {
        let value = lookup(SAMPLE_CONFIG, "provider.openai.baseUrl").expect("nested key resolves");
        assert_eq!(value, "https://api.openai.com");
    }

    #[test]
    fn lookup_missing_key_errors() {
        assert!(lookup(SAMPLE_CONFIG, "provider.anthropic").is_err());
        assert!(lookup(SAMPLE_CONFIG, "nope").is_err());
    }

    #[test]
    fn encode_value_wraps_plain_words_as_strings() {
        assert_eq!(encode_value("claude-sonnet"), "\"claude-sonnet\"");
        assert_eq!(encode_value("42"), "42");
        assert_eq!(encode_value("true"), "true");
        assert_eq!(encode_value("{\"a\":1}"), "{\"a\":1}");
    }
}
