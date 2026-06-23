#!/usr/bin/env python3
"""Dump the real Raydium CPMM program .so + a consistent pool account snapshot from mainnet
(Chainstack, bare-host Basic Auth) into a fixtures dir, for the LiteSVM real-venue M1-GATE
differential. RPC via curl (Basic Auth works); .so extracted from program/programdata accounts.

Writes <FIXDIR>/{cpmm.so, spl_token.so, <pubkey>.bin..., manifest.txt}.
manifest.txt lines: `role pubkey owner lamports`.
"""
import base64, json, os, subprocess, sys

FIXDIR = os.path.expanduser(os.environ.get("REAL_VENUE_FIXTURES", "~/arbit-fixtures/raydium_cpmm"))
os.makedirs(FIXDIR, exist_ok=True)

RAYDIUM_CPMM = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C"
SPL_TOKEN = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
WSOL = "So11111111111111111111111111111111111111112"
POOL = os.environ.get("RAYDIUM_POOL", "")  # empty => auto-select a liquid WSOL-quote pool
OFF = dict(amm_config=8, vault0=72, vault1=104, mint0=168, mint1=200,
           prog0=232, prog1=264, observation=296)

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58encode(b: bytes) -> str:
    n = int.from_bytes(b, "big"); s = ""
    while n > 0:
        n, r = divmod(n, 58); s = B58[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\x00"))) + s

def load_env():
    env = {}
    with open("/mnt/d/arbit/.env") as f:
        for line in f:
            line = line.strip().replace("\r", "")
            if "=" in line and not line.startswith("#"):
                k, v = line.split("=", 1); env[k] = v
    return env

ENV = load_env()
HOST = ENV["CHAINSTACK_SOLANA_RPC_URL"].strip().split("://", 1)
HOST = HOST[0] + "://" + HOST[1].split("/", 1)[0]   # scheme://host (strip any path)
USER = ENV["CHAINSTACK_USERNAME"].strip()
PW = ENV["CHAINSTACK_PASSWORD"].strip()

def rpc(method, params):
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    res = subprocess.run(
        ["curl", "-s", "-u", f"{USER}:{PW}", "-X", "POST", HOST,
         "-H", "Content-Type: application/json", "-d", payload],
        capture_output=True, text=True)
    if res.returncode != 0:
        raise SystemExit(f"curl failed: {res.stderr}")
    return json.loads(res.stdout)

def get_account(pubkey):
    v = rpc("getAccountInfo", [pubkey, {"encoding": "base64"}])
    val = v["result"]["value"]
    if val is None:
        raise SystemExit(f"account {pubkey} not found")
    return base64.b64decode(val["data"][0]), val["owner"], val["lamports"]

def read_pubkey(data, off):  # offsets are ABSOLUTE (already past the 8-byte anchor disc)
    return b58encode(data[off: off + 32])

def find_liquid_pool():
    """getProgramAccounts for CPMM pools with token_1_mint == WSOL (offset 200), then rank a
    sample by WSOL vault (token_1_vault @104) balance and return the most-liquid pool pubkey."""
    print("scanning CPMM for liquid WSOL-quote pools...")
    v = rpc("getProgramAccounts", [RAYDIUM_CPMM, {
        "encoding": "base64",
        "filters": [{"dataSize": 637}, {"memcmp": {"offset": 200, "bytes": WSOL}}],
        "dataSlice": {"offset": 104, "length": 32},  # token_1_vault pubkey
    }])
    accts = v.get("result") or []
    print(f"  {len(accts)} WSOL-quote pools")
    cand = []
    for a in accts:
        pool_pk = a["pubkey"]
        vault1 = b58encode(base64.b64decode(a["account"]["data"][0]))
        cand.append((pool_pk, vault1))
    # Rank a sample (<=100) by WSOL vault balance via getMultipleAccounts.
    sample = cand[:100]
    res = rpc("getMultipleAccounts", [[v for _, v in sample], {"encoding": "base64"}])
    vals = res["result"]["value"]
    best, best_bal = None, 0
    for (pool_pk, _vault1), val in zip(sample, vals):
        if not val:
            continue
        amt = read_u64_le(base64.b64decode(val["data"][0]), 64)
        if amt > best_bal:
            best_bal, best = amt, pool_pk
    if best is None:
        raise SystemExit("no liquid WSOL CPMM pool found")
    print(f"  selected {best} (WSOL vault = {best_bal} lamports = {best_bal/1e9:.3f} SOL)")
    return best

def read_u64_le(data, off):
    return int.from_bytes(data[off:off + 8], "little")

manifest = []
def fetch(role, pubkey):
    data, owner, lamports = get_account(pubkey)
    open(os.path.join(FIXDIR, f"{pubkey}.bin"), "wb").write(data)
    manifest.append(f"{role} {pubkey} {owner} {lamports}")
    print(f"  {role:12} {pubkey}  owner={owner[:10]}.. {len(data)}B {lamports}lp")
    return data

def dump_program(pid, outname):
    """Extract the BPF ELF: non-upgradeable program account data IS the ELF; upgradeable
    programs store it in the programdata account after a 45-byte UpgradeableLoaderState header."""
    data, owner, _ = get_account(pid)
    if owner == "BPFLoaderUpgradeab1e11111111111111111111111":
        # UpgradeableLoaderState::Program { programdata_address }: tag(4) + pubkey(32)
        pd = b58encode(data[4:36])
        pdata, _, _ = get_account(pd)
        elf = pdata[45:]  # ProgramData header = 4(enum)+8(slot)+1(option)+32(authority)
        print(f"  {pid}: upgradeable; programdata={pd}; elf={len(elf)}B")
    else:
        elf = data
        print(f"  {pid}: loader={owner[:10]}..; elf={len(elf)}B")
    open(os.path.join(FIXDIR, outname), "wb").write(elf)

print(f"fixtures -> {FIXDIR}")
print("getVersion:", rpc("getVersion", []).get("result"))
if not POOL:
    POOL = find_liquid_pool()
pool_data = fetch("pool", POOL)
amm_config = read_pubkey(pool_data, OFF["amm_config"])
vault0 = read_pubkey(pool_data, OFF["vault0"])
vault1 = read_pubkey(pool_data, OFF["vault1"])
mint0 = read_pubkey(pool_data, OFF["mint0"])
mint1 = read_pubkey(pool_data, OFF["mint1"])
prog0 = read_pubkey(pool_data, OFF["prog0"])
prog1 = read_pubkey(pool_data, OFF["prog1"])
observation = read_pubkey(pool_data, OFF["observation"])
print(f"  token programs: {prog0} / {prog1} (expect SPL {SPL_TOKEN})")
fetch("amm_config", amm_config)
fetch("observation", observation)
fetch("vault0", vault0)
fetch("vault1", vault1)
fetch("mint0", mint0)
fetch("mint1", mint1)
open(os.path.join(FIXDIR, "manifest.txt"), "w").write("\n".join(manifest) + "\n")

print("dumping programs...")
dump_program(RAYDIUM_CPMM, "cpmm.so")
dump_program(SPL_TOKEN, "spl_token.so")
print("DUMP OK\nmanifest:\n" + "\n".join(manifest))
