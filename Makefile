# Single entrypoint for dev + CI so they run identical commands.
# (GNU make; CI runs on Linux. On Windows use the documented cargo commands directly.)
.PHONY: bootstrap build build-sbf fmt fmt-check lint test test-host test-surfpool \
        config-check audit deny verify-build clean

bootstrap:        ## install + version-verify the pinned toolchain
	bash infra/scripts/install-toolchain.sh

build:            ## build the whole workspace against the committed lockfile
	cargo build --workspace --locked

build-sbf:        ## build the on-chain program into a .so (needs Agave platform-tools)
	cd onchain/arb-program && cargo build-sbf

fmt:              ## auto-format
	cargo fmt --all

fmt-check:        ## CI: fail on unformatted code
	cargo fmt --all -- --check

lint:             ## CI: clippy as hard errors
	cargo clippy --workspace --all-targets --locked -- -D warnings

test: test-host   ## default test target = host unit/property tests

test-host:        ## host-runnable unit + property tests (math/sizing M1-GATE math)
	cargo test --workspace --locked

test-surfpool:    ## Surfpool mainnet-fork integration (needs surfpool + build-sbf)
	bash tests/scripts/run_surfpool.sh

config-check:     ## loader::validate + program-id <-> toml cross-check
	bash infra/scripts/verify-config.sh

audit:            ## supply-chain: known-vuln advisories + policy
	cargo audit
	cargo deny check
	bash infra/audit/verify-integrity.sh

deny: ; cargo deny check

verify-build:     ## reproducible/verifiable on-chain build hash (needs solana-verify)
	bash ops/scripts/verify_build.sh

clean: ; cargo clean
