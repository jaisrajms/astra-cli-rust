//! Wire protocol compatibility for the versioned agent-event envelope
//! (`chat.proto`: `AgentEventEnvelope.protocol_version`).

/// The protocol MAJOR this client speaks. `PROTOCOL_VERSION = 1.3`.
pub const PROTOCOL_MAJOR: u32 = 1;

/// True when `version` shares this client's MAJOR version. Consumers refuse a
/// higher MAJOR and skip (never fail on) a mismatched envelope.
pub fn compatible(version: f64) -> bool {
    (version as u32) == PROTOCOL_MAJOR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_major_is_compatible() {
        assert!(compatible(1.0));
        assert!(compatible(1.3));
        assert!(compatible(1.99));
    }

    #[test]
    fn different_major_is_rejected() {
        assert!(!compatible(0.9));
        assert!(!compatible(2.0));
        assert!(!compatible(2.1));
    }
}
