//! Shared small helpers reused across command modules.

use astra_proto::astra::engine::v1::ModelRef;

/// Mint a fresh idempotency key for one logical `SendMessage` (C-04, `SendMessage.request_id`):
/// call this ONCE per new user-initiated send, and reuse the SAME returned string only if that
/// exact send is retried after a disconnect — never mint a new one for a retry, or the daemon
/// cannot recognize it as the same logical request. Unique enough per process without a UUID
/// dependency: a monotonic counter plus the current time.
pub fn new_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("req-{nanos:x}-{n}")
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
    fn new_request_id_is_unique_across_calls() {
        let a = new_request_id();
        let b = new_request_id();
        assert_ne!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn parse_model_splits_provider_and_model() {
        let m = parse_model("anthropic/claude");
        assert_eq!(m.provider_id, "anthropic");
        assert_eq!(m.model_id, "claude");

        let m = parse_model("claude");
        assert_eq!(m.provider_id, "");
        assert_eq!(m.model_id, "claude");
    }
}
