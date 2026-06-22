//! Reconnect / replay policy (plan.md §5 "Reconnect/replay"). On disconnect, resubscribe
//! with `from_slot = last_processed_slot` and the same filters/commitment; duplicates are
//! fine because the cache is idempotent. Each reconnect starts a NEW `session_id` so the
//! cache switches to the cross-session (slot-only) acceptance rule — `write_version` from the
//! prior session must not suppress fresh state.

/// Slot to resubscribe from after a disconnect. `None` (never processed) ⇒ subscribe from the
/// latest slot.
pub fn resubscribe_from_slot(last_processed_slot: Option<u64>) -> Option<u64> {
    last_processed_slot
}

/// Monotonic session id; bump on every (re)connect. Saturates rather than wrapping.
pub fn next_session_id(current: u64) -> u64 {
    current.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy() {
        assert_eq!(resubscribe_from_slot(Some(1234)), Some(1234));
        assert_eq!(resubscribe_from_slot(None), None);
        assert_eq!(next_session_id(0), 1);
        assert_eq!(next_session_id(u64::MAX), u64::MAX);
    }
}
