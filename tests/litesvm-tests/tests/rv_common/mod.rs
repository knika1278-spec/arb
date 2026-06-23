//! Shared harness for the **real-venue** M1-GATE differentials (`real_venue_*.rs`). Loads a
//! mainnet snapshot (program `.so` + account `.bin` + `manifest.txt`, produced by
//! `tests/scripts/dump_<venue>.py`) into LiteSVM so the REAL venue program executes over real
//! account bytes, and each test compares the realized swap output to our off-chain `arb_math`
//! quote. Fixtures live under `$REAL_VENUE_FIXTURES/<venue>`; tests self-skip when absent so a
//! plain host/CI `cargo test` stays green. See [[arbit-realvenue-litesvm]] for the method.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use litesvm::LiteSVM;
use solana_sdk::{account::Account, clock::Clock, pubkey::Pubkey};

pub const AMOUNT_OFFSET: usize = 64;

/// One snapshotted account: pubkey + on-chain owner + lamports (from `manifest.txt`).
pub struct Entry {
    pub pubkey: Pubkey,
    pub owner: Pubkey,
    pub lamports: u64,
}

/// A loaded mainnet snapshot for one venue: the fixtures dir + the role->Entry manifest map.
pub struct Snapshot {
    pub dir: PathBuf,
    pub man: HashMap<String, Entry>,
}

impl Snapshot {
    /// `$REAL_VENUE_FIXTURES/<venue>` if it has a `manifest.txt`, else `None` (test self-skips).
    pub fn open(venue: &str) -> Option<Self> {
        let dir = PathBuf::from(std::env::var("REAL_VENUE_FIXTURES").ok()?).join(venue);
        if !dir.join("manifest.txt").exists() {
            return None;
        }
        let text = std::fs::read_to_string(dir.join("manifest.txt")).ok()?;
        let mut man = HashMap::new();
        for line in text.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() != 4 {
                continue;
            }
            man.insert(
                f[0].to_string(),
                Entry {
                    pubkey: f[1].parse().ok()?,
                    owner: f[2].parse().ok()?,
                    lamports: f[3].parse().ok()?,
                },
            );
        }
        Some(Self { dir, man })
    }

    pub fn pk(&self, role: &str) -> Pubkey {
        self.man
            .get(role)
            .unwrap_or_else(|| panic!("manifest missing role '{role}'"))
            .pubkey
    }

    /// Raw bytes of an account `.bin` (by pubkey).
    pub fn bin(&self, pubkey: &Pubkey) -> Vec<u8> {
        std::fs::read(self.dir.join(format!("{pubkey}.bin")))
            .unwrap_or_else(|_| panic!("fixture {pubkey}.bin"))
    }

    pub fn role_bin(&self, role: &str) -> Vec<u8> {
        self.bin(&self.pk(role))
    }

    /// Load a program `.so` (by file name) into the SVM at `id`.
    pub fn add_program(&self, svm: &mut LiteSVM, id: Pubkey, so_name: &str) {
        let so =
            std::fs::read(self.dir.join(so_name)).unwrap_or_else(|_| panic!("program {so_name}"));
        svm.add_program(id, &so).unwrap();
    }

    /// Materialize every listed role's snapshotted account verbatim at its real pubkey.
    pub fn load_accounts(&self, svm: &mut LiteSVM, roles: &[&str]) {
        for role in roles {
            let e = &self.man[*role];
            svm.set_account(
                e.pubkey,
                Account {
                    lamports: e.lamports.max(1_000_000),
                    data: self.bin(&e.pubkey),
                    owner: e.owner,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        }
    }
}

pub fn read_u64(data: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(data[off..off + 8].try_into().unwrap())
}

pub fn read_u128(data: &[u8], off: usize) -> u128 {
    u128::from_le_bytes(data[off..off + 16].try_into().unwrap())
}

pub fn read_pubkey(data: &[u8], off: usize) -> Pubkey {
    Pubkey::new_from_array(data[off..off + 32].try_into().unwrap())
}

/// SPL token-account bytes: mint@0, owner@32, amount@64, AccountState::Initialized@108.
pub fn token_account_bytes(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut d = vec![0u8; 165];
    d[0..32].copy_from_slice(mint.as_ref());
    d[32..64].copy_from_slice(owner.as_ref());
    d[64..72].copy_from_slice(&amount.to_le_bytes());
    d[108] = 1;
    d
}

pub fn token_amount(svm: &LiteSVM, key: &Pubkey) -> u64 {
    svm.get_account(key)
        .map(|a| read_u64(&a.data, AMOUNT_OFFSET))
        .unwrap_or(0)
}

/// Warp the Clock sysvar to `unix_ts` (past the pool's open_time gate) at a plausible slot/epoch.
pub fn warp_clock(svm: &mut LiteSVM, unix_ts: i64) {
    svm.set_sysvar(&Clock {
        slot: 350_000_000,
        epoch_start_timestamp: unix_ts - 3600,
        epoch: 810,
        leader_schedule_epoch: 810,
        unix_timestamp: unix_ts,
    });
}
