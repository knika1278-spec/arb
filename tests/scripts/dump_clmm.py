#!/usr/bin/env python3
"""Snapshot a real Raydium CLMM pool + programs for the LiteSVM real-venue differential.

Targets a both-SPL WSOL/USDC CLMM pool (deeply liquid, dense tick arrays). Snapshots pool +
amm_config + observation + vaults + mints + bitmap-extension (if any) + the active tick array and
its neighbours; dumps the CLMM, SPL, Token-2022 and Memo programs. -> $REAL_VENUE_FIXTURES/raydium_clmm.
"""
import base64, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cs_dump as cs

PROGRAM = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK"
SPL = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
TOKEN2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
MEMO = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr"
WSOL = "So11111111111111111111111111111111111111112"
USDC = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
FIXDIR = os.path.join(os.environ.get("REAL_VENUE_FIXTURES", os.path.expanduser("~/arbit-fixtures")), "raydium_clmm")
LEN = 1544
DISC = bytes([247, 237, 227, 245, 215, 195, 222, 70])  # account:PoolState
TICK_ARRAY_SIZE = 60

O = dict(amm_config=9, mint0=73, mint1=105, vault0=137, vault1=169, observation=201,
         tick_spacing=235, liquidity=237, sqrt_price=253, tick_current=269, status=389)

def read_i32(d, off):
    return int.from_bytes(d[off:off + 4], "little", signed=True)

def read_u16(d, off):
    return int.from_bytes(d[off:off + 2], "little")

def start_index(tick_current, tick_spacing):
    span = tick_spacing * TICK_ARRAY_SIZE
    return (tick_current // span) * span  # floor toward -inf

def tick_array_pda(pool, start):
    seed = int(start).to_bytes(4, "big", signed=True)  # i32 BE
    return cs.find_pda([b"tick_array", cs.pk_bytes(pool), seed], PROGRAM)[0]

def bitmap_ext_pda(pool):
    return cs.find_pda([b"pool_tick_array_bitmap_extension", cs.pk_bytes(pool)], PROGRAM)[0]

def main():
    print("getVersion:", cs.rpc("getVersion", []).get("result"))
    # WSOL/USDC CLMM (try token0=WSOL,token1=USDC then swapped)
    accts = []
    for ma, mb, vslice in [(WSOL, USDC, O["vault0"]), (USDC, WSOL, O["vault1"])]:
        v = cs.rpc("getProgramAccounts", [PROGRAM, {
            "encoding": "base64",
            "filters": [{"dataSize": LEN}, {"memcmp": {"offset": O["mint0"], "bytes": ma}},
                        {"memcmp": {"offset": O["mint1"], "bytes": mb}}],
            "dataSlice": {"offset": vslice, "length": 32},
        }])
        accts = v.get("result") or []
        if accts:
            print(f"  {len(accts)} pools mint0={ma[:4]} mint1={mb[:4]}")
            break
    cand = [(a["pubkey"], cs.b58encode(base64.b64decode(a["account"]["data"][0]))) for a in accts]
    res = cs.rpc("getMultipleAccounts", [[vb for _, vb in cand[:100]], {"encoding": "base64"}])
    ranked = sorted(
        [(cs.read_u64(base64.b64decode(val["data"][0]), 64), pk) for (pk, _v), val in zip(cand[:100], res["result"]["value"]) if val],
        reverse=True)
    if not ranked:
        raise SystemExit("no WSOL/USDC CLMM pool found")
    bal, pool = ranked[0]
    d, _, _ = cs.get_account(pool)
    ts = read_u16(d, O["tick_spacing"]); tc = read_i32(d, O["tick_current"])
    print(f"  selected {pool} (vault {bal/1e9:.1f}) ts={ts} tc={tc}")

    snap = cs.Snapshotter(FIXDIR)
    snap.fetch("pool", pool)
    snap.fetch("amm_config", cs.read_pubkey(d, O["amm_config"]))
    snap.fetch("observation", cs.read_pubkey(d, O["observation"]))
    snap.fetch("vault0", cs.read_pubkey(d, O["vault0"]))
    snap.fetch("vault1", cs.read_pubkey(d, O["vault1"]))
    snap.fetch("mint0", cs.read_pubkey(d, O["mint0"]))
    snap.fetch("mint1", cs.read_pubkey(d, O["mint1"]))
    # bitmap extension (may not exist)
    bx = bitmap_ext_pda(pool)
    bxa = cs.rpc("getAccountInfo", [bx, {"encoding": "base64"}])
    if bxa["result"]["value"] is not None:
        snap.fetch("bitmap_ext", bx)
    else:
        print(f"  bitmap_ext {bx} not present (in-range swap should not need it)")
    # tick arrays: active + neighbours both sides
    span = ts * TICK_ARRAY_SIZE
    s0 = start_index(tc, ts)
    for k in (-2, -1, 0, 1, 2):
        pda = tick_array_pda(pool, s0 + k * span)
        a = cs.rpc("getAccountInfo", [pda, {"encoding": "base64", "dataSlice": {"offset": 0, "length": 0}}])
        if a["result"]["value"] is not None:
            snap.fetch(f"tick_array_{k}", pda)
    snap.write_manifest()
    snap.dump_program(PROGRAM, "clmm.so")
    snap.dump_program(SPL, "spl_token.so")
    snap.dump_program(TOKEN2022, "token2022.so")
    snap.dump_program(MEMO, "memo.so")
    print(f"CLMM DUMP OK (ts={ts} tc={tc} s0={s0})")

main()
