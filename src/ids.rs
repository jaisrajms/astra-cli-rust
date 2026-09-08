//! Shared small helpers reused across command modules.

use astra_proto::astra::engine::v1::ModelRef;

/// A fresh, timestamp-derived session id for a brand-new session.
pub fn fresh_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("s-{nanos}")
}

/// Parse a `provider/model` spec into a [`ModelRef`]. A bare name becomes a
/// model id with an empty provider.
pub fn parse_model(spec: &str) -> ModelRef {
    match spec.split_once('/') {
        Some((provider, model)) => ModelRef {
            model_id: model.to_string(),
            provider_id: provider.to_string(),
        },
        None => ModelRef {
            model_id: spec.to_string(),
            provider_id: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_splits_provider_and_model() {
        let m = parse_model("anthropic/claude");
        assert_eq!(m.provider_id, "anthropic");
        assert_eq!(m.model_id, "claude");

        let m = parse_model("claude");
        assert_eq!(m.provider_id, "");
        assert_eq!(m.model_id, "claude");
    }

    #[test]
    fn fresh_session_id_is_prefixed() {
        assert!(fresh_session_id().starts_with("s-"));
    }
}
