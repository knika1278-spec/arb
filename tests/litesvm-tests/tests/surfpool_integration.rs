//! onchain-11 / testing-8 — M1-GATE on the REAL surfpool runtime (agave solana-core 2.2.4,
//! real compute budget + CPI depth), not LiteSVM's embedded VM. A dependency-free JSON-RPC
//! client (std::net) drives the `surfnet_setAccount` cheatcode to install the `arb_program` +
//! `swap-harness` .so and the synthetic token accounts, then sends a real `TryArbitrage` tx and
//! asserts the on-chain realized round-trip == the off-chain `arb_math` prediction (bit-exact)
//! and that an unprofitable round-trip reverts `Unprofitable` on the real runtime.
//!
//! This is the runtime-PARITY proof (the swap-harness stands in for the venue, overriding the
//! allowlisted id via cheatcode). The full real-VENUE differential (clone a real Raydium pool +
//! the real swap CPI account list) is the remaining onchain-6/testing-8 step.
//!
//! Self-skips unless a surfpool fork is reachable (start it with `tests/scripts/run_surfpool.sh`)
//! AND the build-sbf artifacts are present (`ARB_PROGRAM_SO` / `SWAP_HARNESS_SO`).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::str::FromStr;
use std::time::Duration;

use arb_math::{CpmmReserves, RoundTrip};
use arb_program::instruction::{LegDescriptor, TryArbitrageData};
use arb_types::{DexKind, SwapDir};
use serde_json::{json, Value};
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
const BPF_LOADER2: &str = "BPFLoader2111111111111111111111111111111111";
const FEE_NUM: u64 = 25;
const FEE_DEN: u64 = 10_000;

// ----------------------------------------------------------------------------------------------
// Minimal JSON-RPC over localhost HTTP/1.0 (no deps; HTTP/1.0 => server closes after the body,
// no chunked framing, so read-to-EOF yields exactly the response).
// ----------------------------------------------------------------------------------------------
fn rpc_addr() -> String {
    std::env::var("SURFPOOL_RPC_ADDR").unwrap_or_else(|_| "127.0.0.1:8899".to_string())
}

fn rpc(method: &str, params: Value) -> Option<Value> {
    let req = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
    let addr = rpc_addr();
    let mut stream = TcpStream::connect(&addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(25))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();
    let http = format!(
        "POST / HTTP/1.0\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        req.len(),
        req
    );
    stream.write_all(http.as_bytes()).ok()?;
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).ok()?;
    let text = String::from_utf8_lossy(&resp);
    let body = text.split("\r\n\r\n").nth(1)?;
    serde_json::from_str(body.trim()).ok()
}

fn reachable() -> bool {
    rpc("getHealth", json!([]))
        .and_then(|v| v.get("result").and_then(|r| r.as_str().map(|s| s == "ok")))
        .unwrap_or(false)
}

// ----------------------------------------------------------------------------------------------
// hex / base64 (dependency-free)
// ----------------------------------------------------------------------------------------------
fn to_hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = Vec::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(H[(b >> 4) as usize]);
        s.push(H[(b & 0xf) as usize]);
    }
    String::from_utf8(s).unwrap()
}

fn b64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn b64_decode(s: &str) -> Vec<u8> {
    let t = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut rev = [255u8; 256];
    for (i, &c) in t.iter().enumerate() {
        rev[c as usize] = i as u8;
    }
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        let v = rev[c as usize];
        if v == 255 {
            continue;
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

// ----------------------------------------------------------------------------------------------
// cheatcodes + account setup
// ----------------------------------------------------------------------------------------------
fn set_account(pubkey: &Pubkey, lamports: u64, data: &[u8], owner: &str, executable: bool) {
    let v = rpc(
        "surfnet_setAccount",
        json!([pubkey.to_string(), {
            "lamports": lamports,
            "data": to_hex(data),
            "owner": owner,
            "executable": executable,
            "rentEpoch": 0
        }]),
    );
    assert!(
        v.as_ref()
            .map(|x| x.get("error").is_none())
            .unwrap_or(false),
        "surfnet_setAccount({pubkey}) failed: {v:?}"
    );
}

fn load_program(id: &Pubkey, so: &[u8]) {
    set_account(id, 1_000_000_000, so, BPF_LOADER2, true);
}

fn fund_system(pubkey: &Pubkey, lamports: u64) {
    set_account(pubkey, lamports, &[], SYSTEM_PROGRAM, false);
}

fn token_account_bytes(owner_field: &Pubkey, amount: u64) -> Vec<u8> {
    let mut d = vec![0u8; 165];
    d[32..64].copy_from_slice(owner_field.as_ref());
    d[64..72].copy_from_slice(&amount.to_le_bytes());
    d[108] = 1; // Initialized
    d
}

fn set_token(pubkey: &Pubkey, owner_field: &Pubkey, amount: u64, onchain_owner: &Pubkey) {
    set_account(
        pubkey,
        2_000_000,
        &token_account_bytes(owner_field, amount),
        &onchain_owner.to_string(),
        false,
    );
}

fn latest_blockhash() -> Hash {
    let v =
        rpc("getLatestBlockhash", json!([{"commitment":"processed"}])).expect("getLatestBlockhash");
    let s = v["result"]["value"]["blockhash"]
        .as_str()
        .unwrap_or_else(|| panic!("no blockhash: {v}"));
    Hash::from_str(s).expect("parse blockhash")
}

fn extract_custom_code(err: &Value) -> Option<u32> {
    err.get("data")?
        .get("err")?
        .get("InstructionError")?
        .get(1)?
        .get("Custom")?
        .as_u64()
        .map(|c| c as u32)
}

/// Submit with preflight on, so a revert surfaces immediately as an RPC error carrying the
/// program's Custom code. Ok(signature) on accept; Err((custom_code, message)) on revert.
fn send_tx(tx: &Transaction) -> Result<String, (Option<u32>, String)> {
    let bytes = bincode::serialize(tx).expect("serialize tx");
    let b64 = b64_encode(&bytes);
    let v = rpc(
        "sendTransaction",
        json!([b64, {"encoding":"base64","skipPreflight":false,"preflightCommitment":"processed"}]),
    )
    .expect("sendTransaction rpc");
    if let Some(err) = v.get("error") {
        return Err((extract_custom_code(err), err.to_string()));
    }
    Ok(v["result"].as_str().unwrap_or_default().to_string())
}

fn wait_confirmed(sig: &str) -> bool {
    for _ in 0..40 {
        if let Some(v) = rpc("getSignatureStatuses", json!([[sig]])) {
            let status = &v["result"]["value"][0];
            if !status.is_null() {
                if status.get("err").map(|e| !e.is_null()).unwrap_or(false) {
                    return false;
                }
                if status.get("confirmationStatus").is_some() {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    false
}

fn read_token_amount(pubkey: &Pubkey) -> u64 {
    let v = rpc(
        "getAccountInfo",
        json!([pubkey.to_string(), {"encoding":"base64"}]),
    )
    .expect("getAccountInfo");
    let data_b64 = v["result"]["value"]["data"][0]
        .as_str()
        .unwrap_or_else(|| panic!("no account data for {pubkey}: {v}"));
    let bytes = b64_decode(data_b64);
    u64::from_le_bytes(bytes[64..72].try_into().unwrap())
}

fn so(var: &str) -> Option<Vec<u8>> {
    std::fs::read(std::env::var(var).ok()?).ok()
}

fn allowlisted_dex() -> Pubkey {
    Pubkey::new_from_array(arb_config::WAVE1_DEX_ALLOWLIST[0].to_bytes())
}

struct Outcome {
    landed: bool,
    realized_final: Option<u64>,
    err_code: Option<u32>,
}

/// One round-trip on the real surfpool runtime: install programs + accounts via cheatcodes,
/// send TryArbitrage, and read back the realized base output.
#[allow(clippy::too_many_arguments)]
fn run_on_surfpool(
    arb: &[u8],
    harness: &[u8],
    pool_a: (u64, u64),
    pool_b: (u64, u64),
    base_funding: u64,
    delta_in: u64,
    min_profit: u64,
) -> Outcome {
    let dex = allowlisted_dex();
    let arb_id = Keypair::new().pubkey();
    load_program(&arb_id, arb);
    load_program(&dex, harness); // override the real Raydium id with the controlled CP harness

    let authority = Keypair::new();
    fund_system(&authority.pubkey(), 1_000_000_000);

    let base_ata = Pubkey::new_unique();
    let inter_ata = Pubkey::new_unique();
    let pa_in = Pubkey::new_unique();
    let pa_out = Pubkey::new_unique();
    let pb_in = Pubkey::new_unique();
    let pb_out = Pubkey::new_unique();

    set_token(&base_ata, &authority.pubkey(), base_funding, &dex);
    set_token(&inter_ata, &authority.pubkey(), 0, &dex);
    set_token(&pa_in, &authority.pubkey(), pool_a.0, &dex);
    set_token(&pa_out, &authority.pubkey(), pool_a.1, &dex);
    set_token(&pb_in, &authority.pubkey(), pool_b.0, &dex);
    set_token(&pb_out, &authority.pubkey(), pool_b.1, &dex);

    let data = TryArbitrageData {
        min_profit,
        leg_a: LegDescriptor {
            dex: DexKind::RaydiumCpmm,
            dir: SwapDir::AtoB,
            account_count: 4,
            amount_in: delta_in,
            min_out: 0,
        },
        leg_b: LegDescriptor {
            dex: DexKind::RaydiumCpmm,
            dir: SwapDir::AtoB,
            account_count: 4,
            amount_in: 0,
            min_out: 0,
        },
    }
    .pack();

    let metas = vec![
        AccountMeta::new(authority.pubkey(), true),
        AccountMeta::new(base_ata, false),
        AccountMeta::new(inter_ata, false),
        AccountMeta::new_readonly(dex, false),
        AccountMeta::new(base_ata, false),
        AccountMeta::new(inter_ata, false),
        AccountMeta::new(pa_in, false),
        AccountMeta::new(pa_out, false),
        AccountMeta::new_readonly(dex, false),
        AccountMeta::new(inter_ata, false),
        AccountMeta::new(base_ata, false),
        AccountMeta::new(pb_in, false),
        AccountMeta::new(pb_out, false),
    ];

    let ix = Instruction {
        program_id: arb_id,
        accounts: metas,
        data: data.to_vec(),
    };
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&authority.pubkey()),
        &[&authority],
        latest_blockhash(),
    );

    match send_tx(&tx) {
        Ok(sig) => {
            let ok = wait_confirmed(&sig);
            let realized = if ok {
                let post_base = read_token_amount(&base_ata) as i128;
                u64::try_from(post_base + delta_in as i128 - base_funding as i128).ok()
            } else {
                None
            };
            Outcome {
                landed: ok,
                realized_final: realized,
                err_code: None,
            }
        }
        Err((code, _msg)) => Outcome {
            landed: false,
            realized_final: None,
            err_code: code,
        },
    }
}

fn predicted_final(pool_a: (u64, u64), pool_b: (u64, u64), delta_in: u64) -> Option<u64> {
    let a = CpmmReserves::new(pool_a.0, pool_a.1, FEE_NUM, FEE_DEN);
    let b = CpmmReserves::new(pool_b.0, pool_b.1, FEE_NUM, FEE_DEN);
    RoundTrip::new(a, SwapDir::AtoB, b, SwapDir::AtoB).realized_out(delta_in)
}

#[test]
fn surfpool_runtime_differential_and_revert() {
    let Some((arb, harness)) = so("ARB_PROGRAM_SO").zip(so("SWAP_HARNESS_SO")) else {
        eprintln!(
            "SKIP surfpool_runtime_differential_and_revert: set ARB_PROGRAM_SO + SWAP_HARNESS_SO"
        );
        return;
    };
    if !reachable() {
        eprintln!(
            "SKIP surfpool_runtime_differential_and_revert: no surfpool fork at {} \
             (start tests/scripts/run_surfpool.sh)",
            rpc_addr()
        );
        return;
    }

    // The canonical profitable two-pool edge, small size -> profitable.
    let pool_a = (1_000_000u64, 2_000_000u64);
    let pool_b = (2_000_000u64, 1_100_000u64);
    let base_funding = 1_000_000u64;

    for delta in [1_000u64, 5_000, 25_000] {
        let predicted = predicted_final(pool_a, pool_b, delta);
        let o = run_on_surfpool(&arb, &harness, pool_a, pool_b, base_funding, delta, 0);
        assert!(
            o.landed,
            "delta={delta}: TryArbitrage must land on the real runtime, err_code={:?}",
            o.err_code
        );
        assert_eq!(
            o.realized_final, predicted,
            "delta={delta}: real-runtime realized {:?} != off-chain predicted {:?}",
            o.realized_final, predicted
        );
    }

    // Unprofitable: a min_profit far above any achievable net must revert Unprofitable(6000).
    let o = run_on_surfpool(
        &arb,
        &harness,
        pool_a,
        pool_b,
        base_funding,
        5_000,
        10_000_000,
    );
    assert!(!o.landed, "huge min_profit must revert on the real runtime");
    assert_eq!(
        o.err_code,
        Some(6000),
        "real-runtime revert must be Unprofitable(6000), got {:?}",
        o.err_code
    );

    eprintln!(
        "surfpool runtime parity GREEN: realized == predicted on agave runtime + revert proven"
    );
}
