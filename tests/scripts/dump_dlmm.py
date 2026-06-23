#!/usr/bin/env python3
"""Snapshot a real Meteora DLMM (lb_clmm) pool + program for the LiteSVM real-venue differential.

Selects a both-SPL, status-enabled, collect_fee_mode==BothToken, **variable_fee_control == 0** pool
so total_fee_rate == base_fee_rate (deterministic, clock-INDEPENDENT). This is the key to a bit-exact
differential: DLMM's variable (volatility) fee is recomputed on-chain from VariableParameters + the
Clock and is NOT predictable from a static snapshot, but it is structurally **0** when
variable_fee_control == 0, leaving only the deterministic base fee. Snapshots lb_pair + reserve_x/y +
mints + oracle + the active BinArray (+ neighbours). -> $REAL_VENUE_FIXTURES/meteora_dlmm.
"""
import base64, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cs_dump as cs

PROGRAM = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo"
SPL = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
WSOL = "So11111111111111111111111111111111111111112"
FIXDIR = os.path.join(os.environ.get("REAL_VENUE_FIXTURES", os.path.expanduser("~/arbit-fixtures")), "meteora_dlmm")
LEN = 904
MAX_BIN_PER_ARRAY = 70

O = dict(base_factor=8, var_fee_control=16, base_fee_power=34, collect_fee_mode=36,
         active_id=76, bin_step=80, status=82, mint_x=88, mint_y=120, reserve_x=152,
         reserve_y=184, flag_x=880, flag_y=881)

def ru16(d, o): return int.from_bytes(d[o:o + 2], "little")
def ru32(d, o): return int.from_bytes(d[o:o + 4], "little")
def ri32(d, o): return int.from_bytes(d[o:o + 4], "little", signed=True)

def bin_array_pda(pool, index):
    return cs.find_pda([b"bin_array", cs.pk_bytes(pool), int(index).to_bytes(8, "little", signed=True)], PROGRAM)[0]

def qualifies(d):
    # vfc==0 enforced server-side; verify here + status, BothToken collect mode, and both-SPL
    # (flag_x/y == 0, no Token-2022 transfer-fee complication). RPC caps filters at 4.
    return (len(d) == LEN and d[O["status"]] == 0 and d[O["collect_fee_mode"]] == 0
            and ru32(d, O["var_fee_control"]) == 0 and d[O["flag_x"]] == 0 and d[O["flag_y"]] == 0)

def main():
    print("getVersion:", cs.rpc("getVersion", []).get("result"))
    # token_y == WSOL + variable_fee_control@16 == 0 + status@82 == 0; 4 filters max. memcmp "1" ==
    # base58 of one 0x00 byte; "1111" == four 0x00 bytes (the u32 vfc == 0). both-SPL re-checked below.
    v = cs.rpc("getProgramAccounts", [PROGRAM, {
        "encoding": "base64",
        "filters": [
            {"dataSize": LEN},
            {"memcmp": {"offset": O["mint_y"], "bytes": WSOL}},
            {"memcmp": {"offset": O["var_fee_control"], "bytes": "1111"}},
            {"memcmp": {"offset": O["status"], "bytes": "1"}},
        ],
        "dataSlice": {"offset": O["reserve_y"], "length": 32},
    }])
    accts = v.get("result") or []
    print(f"  {len(accts)} vfc==0 token_y==WSOL status==0 DLMM pools")
    cand = [(a["pubkey"], cs.b58encode(base64.b64decode(a["account"]["data"][0]))) for a in accts]
    sample = cand[:100]
    res = cs.rpc("getMultipleAccounts", [[r for _, r in sample], {"encoding": "base64"}])
    ranked = sorted([(cs.read_u64(base64.b64decode(val["data"][0]), 64), pk)
                     for (pk, _r), val in zip(sample, res["result"]["value"]) if val], reverse=True)
    chosen = None
    for bal, pool in ranked:
        d, _, _ = cs.get_account(pool)
        if not qualifies(d):
            continue
        active_id = ri32(d, O["active_id"])
        idx = active_id // MAX_BIN_PER_ARRAY  # floor toward -inf
        ba = bin_array_pda(pool, idx)
        bav = cs.rpc("getAccountInfo", [ba, {"encoding": "base64", "dataSlice": {"offset": 0, "length": 0}}])
        if bav["result"]["value"] is None:
            continue
        chosen = (pool, d, active_id, idx, ba, bal)
        print(f"  selected {pool} WSOL={bal/1e9:.2f} active_id={active_id} bin_step={ru16(d,O['bin_step'])} base_factor={ru16(d,O['base_factor'])} power={d[O['base_fee_power']]}")
        break
    if not chosen:
        raise SystemExit("no qualifying variable_fee_control==0 both-SPL liquid DLMM pool found")

    pool, d, active_id, idx, ba, bal = chosen
    snap = cs.Snapshotter(FIXDIR)
    snap.fetch("lb_pair", pool)
    snap.fetch("reserve_x", cs.read_pubkey(d, O["reserve_x"]))
    snap.fetch("reserve_y", cs.read_pubkey(d, O["reserve_y"]))
    snap.fetch("mint_x", cs.read_pubkey(d, O["mint_x"]))
    snap.fetch("mint_y", cs.read_pubkey(d, O["mint_y"]))
    oracle = cs.find_pda([b"oracle", cs.pk_bytes(pool)], PROGRAM)[0]
    snap.fetch("oracle", oracle)
    # active bin array + the two neighbours (covers a small swap that nudges across bins)
    for k in (-1, 0, 1):
        pda = bin_array_pda(pool, idx + k)
        a = cs.rpc("getAccountInfo", [pda, {"encoding": "base64", "dataSlice": {"offset": 0, "length": 0}}])
        if a["result"]["value"] is not None:
            role = "bin_array_active" if k == 0 else f"bin_array_{k}"
            snap.fetch(role, pda)
    snap.write_manifest()
    snap.dump_program(PROGRAM, "dlmm.so")
    snap.dump_program(SPL, "spl_token.so")
    print(f"DLMM DUMP OK (active_id={active_id} bin_array_index={idx})")

main()
