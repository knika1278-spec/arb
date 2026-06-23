#!/usr/bin/env python3
"""Shared Chainstack-dump helpers for the LiteSVM real-venue M1-GATE differentials.

RPC over `curl -u` against the bare Chainstack host (Basic Auth; urllib gets 403). Used by the
per-venue dump_<venue>.py scripts to snapshot a real program .so + a pool's account graph from
mainnet into $REAL_VENUE_FIXTURES/<venue>/{<pubkey>.bin, manifest.txt, <name>.so}.
"""
import base64, json, os, subprocess
from solders.pubkey import Pubkey as _Pk

def find_pda(seeds, program_id_b58):
    """Solana find_program_address (off-curve PDA). seeds = list of bytes; returns (b58, bump)."""
    pda, bump = _Pk.find_program_address(seeds, _Pk.from_string(program_id_b58))
    return str(pda), bump

def pk_bytes(b58):
    return bytes(_Pk.from_string(b58))

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58encode(b: bytes) -> str:
    n = int.from_bytes(b, "big"); s = ""
    while n > 0:
        n, r = divmod(n, 58); s = B58[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\x00"))) + s

def _load_env():
    env = {}
    with open("/mnt/d/arbit/.env") as f:
        for line in f:
            line = line.strip().replace("\r", "")
            if "=" in line and not line.startswith("#"):
                k, v = line.split("=", 1); env[k] = v
    return env

_ENV = _load_env()
_HOST = _ENV["CHAINSTACK_SOLANA_RPC_URL"].strip().split("://", 1)
HOST = _HOST[0] + "://" + _HOST[1].split("/", 1)[0]
USER = _ENV["CHAINSTACK_USERNAME"].strip()
PW = _ENV["CHAINSTACK_PASSWORD"].strip()

def rpc(method, params):
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    r = subprocess.run(["curl", "-s", "-u", f"{USER}:{PW}", "-X", "POST", HOST,
                        "-H", "Content-Type: application/json", "-d", payload],
                       capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit(f"curl failed: {r.stderr}")
    return json.loads(r.stdout)

def get_account(pubkey):
    v = rpc("getAccountInfo", [pubkey, {"encoding": "base64"}])
    val = v["result"]["value"]
    if val is None:
        raise SystemExit(f"account {pubkey} not found")
    return base64.b64decode(val["data"][0]), val["owner"], val["lamports"]

def read_u64(data, off):
    return int.from_bytes(data[off:off + 8], "little")

def read_pubkey(data, off):
    return b58encode(data[off:off + 32])

class Snapshotter:
    def __init__(self, fixdir):
        self.fixdir = os.path.expanduser(fixdir)
        os.makedirs(self.fixdir, exist_ok=True)
        self.manifest = []

    def fetch(self, role, pubkey):
        data, owner, lamports = get_account(pubkey)
        open(os.path.join(self.fixdir, f"{pubkey}.bin"), "wb").write(data)
        self.manifest.append(f"{role} {pubkey} {owner} {lamports}")
        print(f"  {role:18} {pubkey}  owner={owner[:10]}.. {len(data)}B")
        return data

    def dump_program(self, pubkey, outname):
        data, owner, _ = get_account(pubkey)
        if owner == "BPFLoaderUpgradeab1e11111111111111111111111":
            pd = b58encode(data[4:36])
            pdata, _, _ = get_account(pd)
            elf = pdata[45:]
        else:
            elf = data
        open(os.path.join(self.fixdir, outname), "wb").write(elf)
        print(f"  PROGRAM {pubkey} -> {outname} ({len(elf)}B)")

    def write_manifest(self):
        open(os.path.join(self.fixdir, "manifest.txt"), "w").write("\n".join(self.manifest) + "\n")
        print(f"manifest ({len(self.manifest)} accounts) -> {self.fixdir}/manifest.txt")

def find_liquid_pool(program_id, data_size, quote_mint, quote_mint_off, quote_vault_off, sample=100):
    """getProgramAccounts(program, dataSize + memcmp quote_mint@quote_mint_off), rank a sample by
    the quote vault (@quote_vault_off) balance, return the most-liquid pool pubkey."""
    v = rpc("getProgramAccounts", [program_id, {
        "encoding": "base64",
        "filters": [{"dataSize": data_size}, {"memcmp": {"offset": quote_mint_off, "bytes": quote_mint}}],
        "dataSlice": {"offset": quote_vault_off, "length": 32},
    }])
    accts = v.get("result") or []
    print(f"  {len(accts)} pools with quote_mint@{quote_mint_off}=={quote_mint[:6]}..")
    cand = [(a["pubkey"], b58encode(base64.b64decode(a["account"]["data"][0]))) for a in accts]
    cand = cand[:sample]
    if not cand:
        raise SystemExit("no candidate pools")
    res = rpc("getMultipleAccounts", [[v for _, v in cand], {"encoding": "base64"}])
    best, best_bal = None, 0
    for (pool_pk, _v), val in zip(cand, res["result"]["value"]):
        if not val:
            continue
        amt = read_u64(base64.b64decode(val["data"][0]), 64)
        if amt > best_bal:
            best_bal, best = amt, pool_pk
    if best is None:
        raise SystemExit("no liquid pool found")
    print(f"  selected {best} (quote vault = {best_bal} = {best_bal/1e9:.3f} if SOL)")
    return best
