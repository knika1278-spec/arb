//! Address Lookup Table lifecycle logic (txbuilder-8/9/10).
//!
//! The three ALT hazards from the spec, as pure host-testable logic (the on-chain
//! create/extend/close *submission* is the executor seam):
//!
//! * **append-only extend + chunking** (txbuilder-8) — an ALT is append-only; an `Extend`
//!   instruction can only carry so many 32-byte addresses before the tx itself blows the
//!   1232-byte cap (~30 is the safe working chunk). New addresses are deduped against what the
//!   table already holds, then split into chunks.
//! * **warm-up gate** (txbuilder-9) — addresses extended into a table in slot `S` cannot be
//!   *used* in a v0 tx until a later slot, or key resolution silently fails (invariant §4).
//! * **janitor** (txbuilder-10) — a deactivated table can only be closed (rent reclaimed)
//!   after the deactivation cool-down (~512 slots) elapses.

use crate::txbuilder::error::TxBuilderError;
use solana_pubkey::Pubkey;
use std::collections::HashMap;

/// Safe number of addresses per `Extend` instruction (32 B each; keeps the extend tx itself
/// under the 1232-byte cap with headroom for the rest of the message).
pub const MAX_EXTEND_CHUNK: usize = 30;

/// Slots that must pass after `DeactivateLookupTable` before `CloseLookupTable` is allowed.
/// The runtime cool-down is 512 slots; we wait one extra to be unambiguous.
pub const ALT_CLOSE_COOLDOWN_SLOTS: u64 = 513;

/// An ALT is warm (usable) once at least one slot has passed since its last extend.
pub fn is_warm(last_extended_slot: u64, current_slot: u64) -> bool {
    current_slot > last_extended_slot
}

/// Guard: error unless the table is warm at `current_slot` (never extend-then-use same slot).
pub fn require_warm(
    table: Pubkey,
    last_extended_slot: u64,
    current_slot: u64,
) -> Result<(), TxBuilderError> {
    if is_warm(last_extended_slot, current_slot) {
        Ok(())
    } else {
        Err(TxBuilderError::AltNotWarm {
            table,
            last_extended_slot,
            current_slot,
        })
    }
}

/// A deactivated table is closeable once the cool-down has fully elapsed.
pub fn is_closeable(deactivated_slot: u64, current_slot: u64) -> bool {
    current_slot >= deactivated_slot.saturating_add(ALT_CLOSE_COOLDOWN_SLOTS)
}

/// Dedup `needed` against `existing` (append-only) and split the genuinely-new addresses into
/// `MAX_EXTEND_CHUNK`-sized extend batches, preserving first-seen order and dropping
/// duplicates within `needed` itself.
pub fn plan_extends(existing: &[Pubkey], needed: &[Pubkey]) -> Vec<Vec<Pubkey>> {
    let mut seen: std::collections::HashSet<Pubkey> = existing.iter().copied().collect();
    let mut fresh: Vec<Pubkey> = Vec::new();
    for k in needed {
        if seen.insert(*k) {
            fresh.push(*k);
        }
    }
    fresh.chunks(MAX_EXTEND_CHUNK).map(|c| c.to_vec()).collect()
}

/// Minimal in-memory model of the ALTs the bot maintains: which addresses each table holds and
/// the slot it was last extended (for the warm-up gate). The real on-chain table state is
/// refreshed from RPC by the executor; this tracks intent + warmth between rebuilds.
#[derive(Default)]
pub struct AltManager {
    tables: HashMap<Pubkey, TableState>,
}

#[derive(Clone, Debug, Default)]
struct TableState {
    addresses: Vec<Pubkey>,
    last_extended_slot: u64,
}

impl AltManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a table the manager should track (e.g. the long-lived static table).
    pub fn register(&mut self, table: Pubkey, addresses: Vec<Pubkey>, last_extended_slot: u64) {
        self.tables.insert(
            table,
            TableState {
                addresses,
                last_extended_slot,
            },
        );
    }

    /// Addresses currently known to be in `table`.
    pub fn addresses(&self, table: &Pubkey) -> &[Pubkey] {
        self.tables
            .get(table)
            .map(|t| t.addresses.as_slice())
            .unwrap_or(&[])
    }

    /// Plan the extend batches needed to make `table` contain `needed`, recording the new
    /// addresses + the slot of the (last) extend so the warm-up gate accounts for them. Returns
    /// the chunks to submit (empty if nothing new).
    pub fn ensure_keys_present(
        &mut self,
        table: Pubkey,
        needed: &[Pubkey],
        current_slot: u64,
    ) -> Vec<Vec<Pubkey>> {
        let state = self.tables.entry(table).or_default();
        let chunks = plan_extends(&state.addresses, needed);
        if !chunks.is_empty() {
            for chunk in &chunks {
                state.addresses.extend_from_slice(chunk);
            }
            state.last_extended_slot = current_slot;
        }
        chunks
    }

    /// Whether `table` is warm (safe to use) at `current_slot`.
    pub fn is_warm(&self, table: &Pubkey, current_slot: u64) -> bool {
        self.tables
            .get(table)
            .map(|t| is_warm(t.last_extended_slot, current_slot))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    #[test]
    fn warm_up_gate_blocks_same_slot_use() {
        assert!(!is_warm(100, 100)); // extend-then-use same slot
        assert!(is_warm(100, 101));
        let t = key(9);
        assert!(require_warm(t, 100, 100).is_err());
        assert!(require_warm(t, 100, 101).is_ok());
    }

    #[test]
    fn janitor_waits_full_cooldown() {
        assert!(!is_closeable(1_000, 1_000 + 512));
        assert!(is_closeable(1_000, 1_000 + 513));
        assert!(is_closeable(1_000, 5_000));
    }

    #[test]
    fn plan_extends_dedups_and_chunks() {
        let existing = vec![key(1), key(2)];
        let mut needed = vec![key(2)]; // already present -> dropped
        for i in 10..(10 + 35u8) {
            needed.push(key(i));
        }
        needed.push(key(10)); // duplicate within needed -> dropped
        let chunks = plan_extends(&existing, &needed);
        // 35 genuinely new -> 30 + 5.
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), MAX_EXTEND_CHUNK);
        assert_eq!(chunks[1].len(), 5);
        assert!(!chunks[0].contains(&key(2)));
    }

    #[test]
    fn manager_tracks_addresses_and_warmth() {
        let mut m = AltManager::new();
        let t = key(50);
        let chunks = m.ensure_keys_present(t, &[key(1), key(2)], 200);
        assert_eq!(chunks.len(), 1);
        assert_eq!(m.addresses(&t).len(), 2);
        // Just extended at slot 200 -> not warm at 200, warm at 201.
        assert!(!m.is_warm(&t, 200));
        assert!(m.is_warm(&t, 201));
        // Re-ensuring the same keys is a no-op (append-only dedup).
        assert!(m.ensure_keys_present(t, &[key(1), key(2)], 250).is_empty());
        assert_eq!(m.addresses(&t).len(), 2);
    }
}
