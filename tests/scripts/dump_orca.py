#!/usr/bin/env python3
"""Snapshot a real Orca Whirlpool pool + program for the LiteSVM real-venue differential.

Selects a LIQUID small-tick-spacing WSOL pool whose active tick-array and its 2 neighbours on
EACH side are all initialized (so both swap directions have 3 distinct real tick arrays). Snapshots
pool + 2 vaults + 2 mints + oracle PDA + the (up to 5) tick arrays. Fixtures ->
$REAL_VENUE_FIXTURES/orca_whirlpool.
"""
import base64, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cs_dump as cs

PROGRAM = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc"
SPL = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
WSOL = "So11111111111111111111111111111111111111112"
USDC = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
FIXDIR = os.path.join(os.environ.get("REAL_VENUE_FIXTURES", os.path.expanduser("~/arbit-fixtures")), "orca_whirlpool")
LEN = 653
DISC = bytes([63, 149, 209, 12, 225, 128, 99, 9])  # account:Whirlpool
TICK_ARRAY_SIZE = 88

O = dict(tick_spacing=41, fee_rate=45, liquidity=49, sqrt_price=65, tick_current=81,
         mint_a=101, vault_a=133, mint_b=181, vault_b=213)

def read_i32(d, off):
    return int.from_bytes(d[off:off + 4], "little", signed=True)

def read_u16(d, off):
    return int.from_bytes(d[off:off + 2], "little")

def start_index(tick_current, tick_spacing):
    span = tick_spacing * TICK_ARRAY_SIZE
    # floor toward -inf (div_euclid)
    return (tick_current // span) * span

def tick_array_pda(pool_b58, start):
    return cs.find_pda([b"tick_array", cs.pk_bytes(pool_b58), str(start).encode()], PROGRAM)[0]

def main():
    print("getVersion:", cs.rpc("getVersion", []).get("result"))
    # SOL/USDC class: mint_a == WSOL, mint_b == USDC (both classic SPL, deeply liquid, dense ticks).
    v = cs.rpc("getProgramAccounts", [PROGRAM, {
        "encoding": "base64",
        "filters": [
            {"dataSize": LEN},
            {"memcmp": {"offset": O["mint_a"], "bytes": WSOL}},
            {"memcmp": {"offset": O["mint_b"], "bytes": USDC}},
        ],
        "dataSlice": {"offset": O["vault_a"], "length": 32},  # WSOL vault, to rank liquidity
    }])
    accts = v.get("result") or []
    print(f"  {len(accts)} WSOL/USDC Whirlpools")
    cand = [(a["pubkey"], cs.b58encode(base64.b64decode(a["account"]["data"][0]))) for a in accts]
    sample = cand[:100]
    res = cs.rpc("getMultipleAccounts", [[vb for _, vb in sample], {"encoding": "base64"}])
    ranked = []
    for (pool_pk, _vb), val in zip(sample, res["result"]["value"]):
        if val:
            ranked.append((cs.read_u64(base64.b64decode(val["data"][0]), 64), pool_pk))
    ranked.sort(reverse=True)

    chosen = None
    for bal, pool_pk in ranked[:15]:
        d, owner, _ = cs.get_account(pool_pk)
        if len(d) != LEN or d[:8] != DISC:
            continue
        ts = read_u16(d, O["tick_spacing"]); tc = read_i32(d, O["tick_current"])
        if ts == 0:
            continue
        span = ts * TICK_ARRAY_SIZE
        s0 = start_index(tc, ts)
        starts = {k: s0 + k * span for k in (-2, -1, 0, 1, 2)}
        pdas = {k: tick_array_pda(pool_pk, s) for k, s in starts.items()}
        # check existence of all 5 via getMultipleAccounts
        ex = cs.rpc("getMultipleAccounts", [list(pdas.values()), {"encoding": "base64", "dataSlice": {"offset": 0, "length": 0}}])
        exist = {k: (val is not None) for k, val in zip(pdas.keys(), ex["result"]["value"])}
        n_exist = sum(exist.values())
        print(f"  candidate {pool_pk} WSOL={bal/1e9:.1f} ts={ts} tc={tc} tick_arrays_exist={exist}")
        if exist[0] and ((exist[-1] and exist[-2]) or (exist[1] and exist[2])):
            chosen = (pool_pk, d, ts, tc, span, s0, starts, pdas, exist)
            print(f"  -> selected {pool_pk}")
            break
    if not chosen:
        raise SystemExit("no Whirlpool with active+2 tick arrays on a side found in sample")

    pool_pk, d, ts, tc, span, s0, starts, pdas, exist = chosen
    snap = cs.Snapshotter(FIXDIR)
    snap.fetch("pool", pool_pk)
    snap.fetch("token_a_vault", cs.read_pubkey(d, O["vault_a"]))
    snap.fetch("token_b_vault", cs.read_pubkey(d, O["vault_b"]))
    snap.fetch("token_a_mint", cs.read_pubkey(d, O["mint_a"]))
    snap.fetch("token_b_mint", cs.read_pubkey(d, O["mint_b"]))
    oracle = cs.find_pda([b"oracle", cs.pk_bytes(pool_pk)], PROGRAM)[0]
    # oracle may not exist for older pools; fetch if present
    o = cs.rpc("getAccountInfo", [oracle, {"encoding": "base64"}])
    if o["result"]["value"] is not None:
        snap.fetch("oracle", oracle)
    else:
        print(f"  oracle {oracle} not present (v1 swap reads it read-only; ok if uninitialized)")
    for k, exists in exist.items():
        if exists:
            snap.fetch(f"tick_array_{k}", pdas[k])  # role names: tick_array_-2..tick_array_2
    snap.write_manifest()
    snap.dump_program(PROGRAM, "orca.so")
    snap.dump_program(SPL, "spl_token.so")
    print(f"ORCA DUMP OK (tick_spacing={ts}, tick_current={tc})")

main()
