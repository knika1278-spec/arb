//! Hard-limit validation gate (txbuilder-6). Computes, for the exact instruction list the
//! builder will emit, the three ceilings that silently kill a tx if breached (invariant §5):
//!
//! * **account locks** — every unique account the tx touches (static + ALT-resolved). This is
//!   `MAX_TX_ACCOUNT_LOCKS = 128`, the ceiling that **binds before** the 256 loaded cap.
//! * **serialized bytes** — `TX_SIZE_LIMIT_BYTES = 1232`; an ALT compresses 32-byte keys to
//!   1-byte indices but does NOT raise this cap.
//! * **compute units** — `MAX_COMPUTE_UNIT_LIMIT = 1_400_000`.
//!
//! We compute the serialized size from the v0 wire layout directly (ShortVec/compact-u16
//! counts), so the gate is exact for the builder's **partition policy**: signers and program
//! ids are kept in the static key set (signers legally cannot live in an ALT); every other
//! account that appears in a provided ALT is resolved through it. The actual
//! `VersionedMessage` object construction + signing is the signer/executor seam — the *limits*
//! are knowable here without it.

use crate::txbuilder::error::TxBuilderError;
use arb_config::limits::{
    MAX_COMPUTE_UNIT_LIMIT, MAX_LOADED_ACCOUNTS, MAX_TX_ACCOUNT_LOCKS, TX_SIZE_LIMIT_BYTES,
};
use solana_program::instruction::Instruction;
use solana_pubkey::Pubkey;
use std::collections::{HashMap, HashSet};

/// One ALT the builder will attach, with the addresses it currently holds.
#[derive(Clone, Copy, Debug)]
pub struct AltView<'a> {
    pub table: Pubkey,
    pub addresses: &'a [Pubkey],
}

/// Serialized length of a ShortVec / compact-u16 length prefix for `n` elements.
pub fn shortvec_len(n: usize) -> usize {
    if n < 0x80 {
        1
    } else if n < 0x4000 {
        2
    } else {
        3
    }
}

/// The measured budget for a candidate transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitReport {
    /// Required signatures (= static signer accounts, fee payer included).
    pub num_signers: usize,
    /// Accounts written into the static key array (signers + program ids + non-ALT accounts).
    pub num_static_keys: usize,
    /// Accounts resolved through ALTs (writable + readonly).
    pub num_alt_loaded: usize,
    /// Unique accounts the tx locks (static + ALT-resolved). Checked vs MAX_TX_ACCOUNT_LOCKS.
    pub account_locks: usize,
    /// Unique accounts loaded (== `account_locks` for a v0 tx). Checked vs MAX_LOADED_ACCOUNTS.
    pub loaded_accounts: usize,
    /// Serialized transaction size in bytes. Checked vs TX_SIZE_LIMIT_BYTES.
    pub serialized_len: usize,
    /// Requested compute-unit limit. Checked vs MAX_COMPUTE_UNIT_LIMIT.
    pub compute_units: u32,
}

impl LimitReport {
    /// The four hard-limit checks. Returns the FIRST breach (locks first — it binds first).
    pub fn validate(&self) -> Result<(), TxBuilderError> {
        if self.account_locks > MAX_TX_ACCOUNT_LOCKS {
            return Err(TxBuilderError::TooManyAccountLocks {
                got: self.account_locks,
                max: MAX_TX_ACCOUNT_LOCKS,
            });
        }
        if self.loaded_accounts > MAX_LOADED_ACCOUNTS {
            return Err(TxBuilderError::TooManyLoadedAccounts {
                got: self.loaded_accounts,
                max: MAX_LOADED_ACCOUNTS,
            });
        }
        if self.serialized_len > TX_SIZE_LIMIT_BYTES {
            return Err(TxBuilderError::TxTooLarge {
                got: self.serialized_len,
                max: TX_SIZE_LIMIT_BYTES,
            });
        }
        if self.compute_units > MAX_COMPUTE_UNIT_LIMIT {
            return Err(TxBuilderError::ComputeBudgetExceeded {
                got: self.compute_units,
                max: MAX_COMPUTE_UNIT_LIMIT,
            });
        }
        Ok(())
    }
}

fn touch(
    flags: &mut HashMap<Pubkey, (bool, bool)>,
    order: &mut Vec<Pubkey>,
    k: Pubkey,
    signer: bool,
    writable: bool,
) {
    match flags.get_mut(&k) {
        Some(e) => {
            e.0 |= signer;
            e.1 |= writable;
        }
        None => {
            flags.insert(k, (signer, writable));
            order.push(k);
        }
    }
}

/// Measure the v0 budget for `instructions` under the builder's partition policy.
pub fn measure(
    fee_payer: &Pubkey,
    extra_signers: &[Pubkey],
    instructions: &[Instruction],
    alts: &[AltView],
    compute_units: u32,
) -> LimitReport {
    // 1. Merge per-account (signer, writable) flags across the whole tx, recording first-seen
    //    order so each unique account is counted once.
    let mut flags: HashMap<Pubkey, (bool, bool)> = HashMap::new();
    let mut order: Vec<Pubkey> = Vec::new();
    let mut forced_static: HashSet<Pubkey> = HashSet::new();

    touch(&mut flags, &mut order, *fee_payer, true, true);
    forced_static.insert(*fee_payer);
    for s in extra_signers {
        touch(&mut flags, &mut order, *s, true, false);
        forced_static.insert(*s);
    }
    for ix in instructions {
        touch(&mut flags, &mut order, ix.program_id, false, false);
        forced_static.insert(ix.program_id);
        for m in &ix.accounts {
            touch(&mut flags, &mut order, m.pubkey, m.is_signer, m.is_writable);
        }
    }

    // 2. First ALT (in attach order) that holds each address.
    let mut alt_first: HashMap<Pubkey, usize> = HashMap::new();
    for (ti, alt) in alts.iter().enumerate() {
        for a in alt.addresses {
            alt_first.entry(*a).or_insert(ti);
        }
    }

    // 3. Classify every referenced account: forced-static (signer/program) or, if it appears
    //    in an ALT, resolved through it; otherwise static.
    let mut num_static = 0usize;
    let mut num_signers = 0usize;
    let mut alt_loaded = 0usize;
    let mut tbl_w = vec![0usize; alts.len()];
    let mut tbl_r = vec![0usize; alts.len()];

    for k in &order {
        let (sg, wr) = *flags.get(k).expect("k came from order");
        if sg {
            num_signers += 1;
        }
        if !sg && !forced_static.contains(k) {
            if let Some(&ti) = alt_first.get(k) {
                if wr {
                    tbl_w[ti] += 1;
                } else {
                    tbl_r[ti] += 1;
                }
                alt_loaded += 1;
                continue;
            }
        }
        num_static += 1;
    }

    // 4. Serialized size from the v0 wire layout.
    let mut ix_bytes = 0usize;
    for ix in instructions {
        ix_bytes += 1 // program-id index
            + shortvec_len(ix.accounts.len())
            + ix.accounts.len() // 1 index byte each
            + shortvec_len(ix.data.len())
            + ix.data.len();
    }
    let mut alt_bytes = 0usize;
    let mut contributing_tables = 0usize;
    for ti in 0..alts.len() {
        if tbl_w[ti] + tbl_r[ti] == 0 {
            continue;
        }
        contributing_tables += 1;
        alt_bytes += 32 + shortvec_len(tbl_w[ti]) + tbl_w[ti] + shortvec_len(tbl_r[ti]) + tbl_r[ti];
    }

    let message_len = 1 // v0 version prefix (0x80)
        + 3 // message header
        + shortvec_len(num_static)
        + 32 * num_static
        + 32 // recent blockhash
        + shortvec_len(instructions.len())
        + ix_bytes
        + shortvec_len(contributing_tables)
        + alt_bytes;

    let serialized_len = shortvec_len(num_signers) + 64 * num_signers + message_len;

    LimitReport {
        num_signers,
        num_static_keys: num_static,
        num_alt_loaded: alt_loaded,
        account_locks: num_static + alt_loaded,
        loaded_accounts: num_static + alt_loaded,
        serialized_len,
        compute_units,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_program::instruction::{AccountMeta, Instruction};

    fn key(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    #[test]
    fn shortvec_boundaries() {
        assert_eq!(shortvec_len(0), 1);
        assert_eq!(shortvec_len(127), 1);
        assert_eq!(shortvec_len(128), 2);
        assert_eq!(shortvec_len(16_383), 2);
        assert_eq!(shortvec_len(16_384), 3);
    }

    fn one_ix(program: Pubkey, metas: Vec<AccountMeta>, data: Vec<u8>) -> Instruction {
        Instruction {
            program_id: program,
            accounts: metas,
            data,
        }
    }

    #[test]
    fn no_alt_size_matches_hand_computation() {
        // fee_payer + 1 instruction (program + 2 readonly metas) + 3 data bytes, no ALT.
        let payer = key(1);
        let ix = one_ix(
            key(50),
            vec![
                AccountMeta::new(key(60), false),
                AccountMeta::new(key(61), false),
            ],
            vec![1, 2, 3],
        );
        let r = measure(&payer, &[], &[ix], &[], 200_000);
        // static = payer + program + 2 metas = 4 ; signers = 1.
        assert_eq!(r.num_static_keys, 4);
        assert_eq!(r.num_signers, 1);
        assert_eq!(r.num_alt_loaded, 0);
        assert_eq!(r.account_locks, 4);
        // ix_bytes = 1 + 1 + 2 + 1 + 3 = 8.
        // message = 1+3 + 1 + 128 + 32 + 1 + 8 + 1 + 0 = 175.
        // tx = 1 + 64 + 175 = 240.
        assert_eq!(r.serialized_len, 240);
    }

    #[test]
    fn alt_shrinks_bytes_but_locks_unchanged() {
        let payer = key(1);
        let metas = vec![
            AccountMeta::new(key(60), false),
            AccountMeta::new(key(61), false),
        ];
        let ix = one_ix(key(50), metas, vec![1, 2, 3]);
        let alt_addrs = [key(60), key(61)];
        let alts = [AltView {
            table: key(99),
            addresses: &alt_addrs,
        }];
        let with_alt = measure(&payer, &[], std::slice::from_ref(&ix), &alts, 200_000);
        let without = measure(&payer, &[], std::slice::from_ref(&ix), &[], 200_000);

        // The two metas move out of the static key array into the lookup → fewer bytes.
        assert_eq!(with_alt.num_static_keys, 2); // payer + program
        assert_eq!(with_alt.num_alt_loaded, 2);
        assert_eq!(with_alt.account_locks, without.account_locks); // same unique set
        assert!(with_alt.serialized_len < without.serialized_len);
    }

    #[test]
    fn validate_flags_each_ceiling() {
        let ok = LimitReport {
            num_signers: 1,
            num_static_keys: 10,
            num_alt_loaded: 10,
            account_locks: 20,
            loaded_accounts: 20,
            serialized_len: 500,
            compute_units: 200_000,
        };
        assert!(ok.validate().is_ok());

        let locks = LimitReport {
            account_locks: 200,
            loaded_accounts: 200,
            ..ok
        };
        assert!(matches!(
            locks.validate(),
            Err(TxBuilderError::TooManyAccountLocks { got: 200, .. })
        ));

        let big = LimitReport {
            serialized_len: 2000,
            ..ok
        };
        assert!(matches!(
            big.validate(),
            Err(TxBuilderError::TxTooLarge { got: 2000, .. })
        ));

        let cu = LimitReport {
            compute_units: 2_000_000,
            ..ok
        };
        assert!(matches!(
            cu.validate(),
            Err(TxBuilderError::ComputeBudgetExceeded { got: 2_000_000, .. })
        ));
    }
}
