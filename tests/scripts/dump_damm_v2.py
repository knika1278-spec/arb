#!/usr/bin/env python3
"""Snapshot a real Meteora DAMM v2 (CP-AMM) pool + program for the LiteSVM real-venue differential.

Selects a LIQUID, CONSTANT-FEE pool: WSOL-quote (token_b_mint), collect_fee_mode==0 (BothToken),
both-SPL, status enabled, dynamic_fee_initialized==0, number_of_period==0 (so total fee ==
cliff_fee_numerator, clock-independent). Writes fixtures to $REAL_VENUE_FIXTURES/meteora_damm_v2.
"""
import base64, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cs_dump as cs

PROGRAM = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG"
SPL = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
WSOL = "So11111111111111111111111111111111111111112"
FIXDIR = os.path.join(os.environ.get("REAL_VENUE_FIXTURES", os.path.expanduser("~/arbit-fixtures")), "meteora_damm_v2")
LEN = 1112
DISC = bytes([241, 154, 109, 4, 17, 177, 109, 188])  # account:Pool

# offsets (absolute, incl 8-byte disc)
O = dict(cliff=8, base_fee_mode=16, num_period=22, dyn_fee=56, mint_a=168, mint_b=200,
         vault_a=232, vault_b=264, liquidity=360, status=481, flag_a=482, flag_b=483,
         collect_fee_mode=484, sqrt_min=424, sqrt_max=440, sqrt_price=456)

def qualifies(d):
    return (d[O["collect_fee_mode"]] == 0 and d[O["flag_a"]] == 0 and d[O["flag_b"]] == 0
            and d[O["status"]] == 0 and d[O["dyn_fee"]] == 0
            and int.from_bytes(d[O["num_period"]:O["num_period"] + 2], "little") == 0
            and cs.read_u64(d, O["cliff"]) > 0
            and int.from_bytes(d[O["liquidity"]:O["liquidity"] + 16], "little") > 0)

def main():
    print("getVersion:", cs.rpc("getVersion", []).get("result"))
    # candidate WSOL-token_b pools, slice token_b_vault to rank by WSOL balance
    v = cs.rpc("getProgramAccounts", [PROGRAM, {
        "encoding": "base64",
        "filters": [{"dataSize": LEN}, {"memcmp": {"offset": O["mint_b"], "bytes": WSOL}}],
        "dataSlice": {"offset": O["vault_b"], "length": 32},
    }])
    accts = v.get("result") or []
    print(f"  {len(accts)} WSOL-token_b DAMM v2 pools")
    cand = [(a["pubkey"], cs.b58encode(base64.b64decode(a["account"]["data"][0]))) for a in accts]
    # rank a sample by token_b (WSOL) vault balance
    sample = cand[:100]
    res = cs.rpc("getMultipleAccounts", [[vb for _, vb in sample], {"encoding": "base64"}])
    ranked = []
    for (pool_pk, _vb), val in zip(sample, res["result"]["value"]):
        if val:
            bal = cs.read_u64(base64.b64decode(val["data"][0]), 64)
            ranked.append((bal, pool_pk))
    ranked.sort(reverse=True)
    pool = None
    for bal, pool_pk in ranked:
        d, owner, _ = cs.get_account(pool_pk)
        if len(d) == LEN and d[:8] == DISC and qualifies(d):
            pool = pool_pk
            print(f"  selected {pool_pk} (WSOL vault {bal/1e9:.3f} SOL, cliff_fee={cs.read_u64(d, O['cliff'])}/1e9)")
            break
        else:
            print(f"  skip {pool_pk} (WSOL {bal/1e9:.2f}) collect={d[O['collect_fee_mode']]} flags={d[O['flag_a']]}/{d[O['flag_b']]} status={d[O['status']]} dyn={d[O['dyn_fee']]} nperiod={int.from_bytes(d[O['num_period']:O['num_period']+2],'little')}")
    if not pool:
        raise SystemExit("no qualifying constant-fee liquid DAMM v2 pool found")

    snap = cs.Snapshotter(FIXDIR)
    pool_data = snap.fetch("pool", pool)
    snap.fetch("token_a_vault", cs.read_pubkey(pool_data, O["vault_a"]))
    snap.fetch("token_b_vault", cs.read_pubkey(pool_data, O["vault_b"]))
    snap.fetch("token_a_mint", cs.read_pubkey(pool_data, O["mint_a"]))
    snap.fetch("token_b_mint", cs.read_pubkey(pool_data, O["mint_b"]))
    snap.write_manifest()
    snap.dump_program(PROGRAM, "damm_v2.so")
    snap.dump_program(SPL, "spl_token.so")
    print("DAMM v2 DUMP OK")

main()
