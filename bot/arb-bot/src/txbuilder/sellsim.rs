//! add-3 — route-specific SELL-simulation honeypot / rug gate.
//!
//! The buy leg succeeding tells us nothing about whether the acquired token can be SOLD back to
//! base: honeypots let you buy and trap you on the sell, and Token-2022 / AMM transfer taxes skim
//! the exit. Before an opportunity is priced, this gate simulates selling the acquired (non-base)
//! mint back to base **over our own vetted Wave-1 venues — never Jupiter** (we will not trust an
//! external aggregator's simulation as our safety gate), and classifies the result:
//!
//! * a reverting / dust-returning sell ⇒ [`SellVerdict::Honeypot`] — the route is HARD-REJECTED;
//! * a sell that returns materially less than the bit-exact quote ⇒ [`SellVerdict::Taxed`], whose
//!   skim feeds the cost model's `e_rug_honeypot_lamports` term (the §10 `E[rug/honeypot]` loss);
//! * a clean sell within tolerance ⇒ [`SellVerdict::Sellable`] (E[rug] ≈ 0).
//!
//! This is **necessary, not sufficient** (plan §9.1): a passed sell-sim only lowers the *expected*
//! rug/honeypot loss — the on-chain terminal assert on the realized base delta remains the final net
//! (invariant §2). The networked simulator implements [`SellSimulator`]; the gate logic + the
//! verdict→`EconParams` mapping are host-tested here.

use solana_pubkey::Pubkey;

const BPS_DENOM: u64 = 10_000;

/// What a route-specific SELL simulation reports for the acquired (non-base) mint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SellSimResult {
    /// `Some(code)` if the SELL leg reverted — the canonical honeypot signature (buy ok, sell
    /// impossible). Decode vs `arb_types::ArbError` for our own codes (else a venue-CPI revert).
    pub revert_code: Option<u32>,
    /// Base-asset lamports the SELL of `sold_amount` actually returned (post-tax, realized).
    pub realized_out_lamports: u64,
    /// The amount of the acquired token the simulation tried to sell (the position being exited).
    pub sold_amount: u64,
    /// Base-out the bit-exact off-chain quote expected for `sold_amount` with NO tax / honeypot.
    pub expected_out_lamports: u64,
}

/// Thresholds for classifying a sell simulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SellSimPolicy {
    /// A sell returning ≤ this fraction of the quote (in bps) is a honeypot, not merely taxed.
    /// e.g. `500` ⇒ a sell yielding ≤ 5 % of the expected out is treated as un-sellable.
    pub honeypot_floor_bps: u16,
    /// Skim (bps below the quote) at or under which the shortfall is tolerated routing noise; above
    /// it the route is [`SellVerdict::Taxed`] and the skim is priced into `e_rug_honeypot_lamports`.
    pub tax_tolerance_bps: u16,
}

impl Default for SellSimPolicy {
    fn default() -> Self {
        // Conservative for the hot-pool / fresh-launchpad niche: ≤5 % out ⇒ honeypot; >1 % skim ⇒
        // priced as expected rug/tax loss.
        Self {
            honeypot_floor_bps: 500,
            tax_tolerance_bps: 100,
        }
    }
}

/// The classification of an acquired mint's exit liquidity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SellVerdict {
    /// Sells cleanly within the tax tolerance — proceed; expected rug/honeypot loss ≈ 0.
    Sellable { realized_out_lamports: u64 },
    /// The sell reverted or returned dust — a honeypot. The route is hard-rejected regardless of
    /// the headline spread (an un-sellable token has no exit).
    Honeypot { code: Option<u32> },
    /// Sells, but a transfer tax / skim above tolerance eats the exit. `skim_lamports` (the
    /// quote-minus-realized shortfall) becomes the priced `E[rug/honeypot]` term.
    Taxed { tax_bps: u16, skim_lamports: u64 },
}

impl SellVerdict {
    /// Whether the route must be HARD-REJECTED (an un-sellable honeypot) before any EV pricing.
    pub fn is_honeypot(&self) -> bool {
        matches!(self, SellVerdict::Honeypot { .. })
    }

    /// The expected rug/honeypot loss term (lamports) to add into [`crate::metrics::econ::EconParams`]
    /// `e_rug_honeypot_lamports`. A honeypot is a hard reject (not a priced term), so this returns 0
    /// for it — gate on [`Self::is_honeypot`] FIRST, then price the surviving `Taxed` skim.
    pub fn e_rug_honeypot_lamports(&self) -> u64 {
        match self {
            SellVerdict::Taxed { skim_lamports, .. } => *skim_lamports,
            SellVerdict::Sellable { .. } | SellVerdict::Honeypot { .. } => 0,
        }
    }
}

/// Classify a sell simulation against a policy. Pure: revert/dust ⇒ honeypot, over-tolerance skim ⇒
/// taxed (skim priced), else sellable. When the quote is zero (no quoted liquidity) the realized
/// amount alone decides: any realized out ⇒ sellable, none ⇒ honeypot.
pub fn classify_sell(result: SellSimResult, policy: &SellSimPolicy) -> SellVerdict {
    if let Some(code) = result.revert_code {
        return SellVerdict::Honeypot { code: Some(code) };
    }
    let expected = result.expected_out_lamports;
    let realized = result.realized_out_lamports;

    if expected == 0 {
        return if realized == 0 {
            SellVerdict::Honeypot { code: None }
        } else {
            SellVerdict::Sellable {
                realized_out_lamports: realized,
            }
        };
    }

    // realized_bps = realized / expected, in basis points (saturating: a sell that beats the quote
    // can exceed 10_000 — still clearly sellable).
    let realized_bps = (realized as u128 * BPS_DENOM as u128 / expected as u128) as u64;
    if realized_bps <= policy.honeypot_floor_bps as u64 {
        return SellVerdict::Honeypot { code: None };
    }

    // Skim = the shortfall below the quote (0 if the sell met or beat it).
    let skim = expected.saturating_sub(realized);
    let tax_bps = (skim as u128 * BPS_DENOM as u128 / expected as u128) as u64;
    if tax_bps <= policy.tax_tolerance_bps as u64 {
        SellVerdict::Sellable {
            realized_out_lamports: realized,
        }
    } else {
        SellVerdict::Taxed {
            tax_bps: tax_bps.min(BPS_DENOM) as u16,
            skim_lamports: skim,
        }
    }
}

/// Why a sell simulation could not be performed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SellSimError {
    /// The simulator transport (RPC / fork) failed before a result.
    Transport(String),
}

/// SELL-simulation seam. The real impl assembles a SELL of `sold_amount` of `output_mint` back to
/// base over our vetted venues (NOT Jupiter) and simulates it; here we own the gate logic + a host
/// mock. Kept sync like the other txbuilder seams ([`crate::txbuilder::SimulateRpc`]).
pub trait SellSimulator {
    fn simulate_sell(
        &self,
        output_mint: &Pubkey,
        sold_amount: u64,
    ) -> Result<SellSimResult, SellSimError>;
}

/// Run the gate end-to-end: simulate the exit sell over the seam, then classify it.
pub fn run_sell_sim_gate(
    sim: &dyn SellSimulator,
    output_mint: &Pubkey,
    sold_amount: u64,
    policy: &SellSimPolicy,
) -> Result<SellVerdict, SellSimError> {
    let result = sim.simulate_sell(output_mint, sold_amount)?;
    Ok(classify_sell(result, policy))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mint(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    fn res(revert: Option<u32>, realized: u64, expected: u64) -> SellSimResult {
        SellSimResult {
            revert_code: revert,
            realized_out_lamports: realized,
            sold_amount: 1_000_000,
            expected_out_lamports: expected,
        }
    }

    #[test]
    fn reverting_sell_is_a_honeypot() {
        let v = classify_sell(res(Some(6006), 0, 1_000_000), &SellSimPolicy::default());
        assert_eq!(v, SellVerdict::Honeypot { code: Some(6006) });
        assert!(v.is_honeypot());
        assert_eq!(v.e_rug_honeypot_lamports(), 0); // hard-reject, not priced
    }

    #[test]
    fn dust_returning_sell_is_a_honeypot() {
        // Returns 3 % of the quote, below the 5 % honeypot floor => un-sellable.
        let v = classify_sell(res(None, 30_000, 1_000_000), &SellSimPolicy::default());
        assert_eq!(v, SellVerdict::Honeypot { code: None });
        assert!(v.is_honeypot());
    }

    #[test]
    fn clean_sell_within_tolerance_is_sellable() {
        // 0.5 % skim, under the 1 % tolerance => clean, zero E[rug].
        let v = classify_sell(res(None, 995_000, 1_000_000), &SellSimPolicy::default());
        assert_eq!(
            v,
            SellVerdict::Sellable {
                realized_out_lamports: 995_000
            }
        );
        assert!(!v.is_honeypot());
        assert_eq!(v.e_rug_honeypot_lamports(), 0);
    }

    #[test]
    fn taxed_sell_prices_the_skim_into_e_rug() {
        // 8 % skim: above the 1 % tolerance but above the 5 % honeypot floor => taxed, skim priced.
        let v = classify_sell(res(None, 920_000, 1_000_000), &SellSimPolicy::default());
        assert_eq!(
            v,
            SellVerdict::Taxed {
                tax_bps: 800,
                skim_lamports: 80_000
            }
        );
        assert!(!v.is_honeypot());
        assert_eq!(v.e_rug_honeypot_lamports(), 80_000);
    }

    #[test]
    fn sell_beating_the_quote_is_sellable() {
        // Realized > expected (rounding / better fill) => sellable, no skim.
        let v = classify_sell(res(None, 1_010_000, 1_000_000), &SellSimPolicy::default());
        assert!(matches!(v, SellVerdict::Sellable { .. }));
        assert_eq!(v.e_rug_honeypot_lamports(), 0);
    }

    #[test]
    fn zero_quote_decides_on_realized_alone() {
        assert!(classify_sell(res(None, 0, 0), &SellSimPolicy::default()).is_honeypot());
        assert!(matches!(
            classify_sell(res(None, 1, 0), &SellSimPolicy::default()),
            SellVerdict::Sellable { .. }
        ));
    }

    #[test]
    fn honeypot_floor_is_inclusive_at_the_boundary() {
        let p = SellSimPolicy::default(); // floor 500 bps (5%)
                                          // EXACTLY 5 % of the quote => honeypot (the inclusive `<=` boundary).
        assert!(classify_sell(res(None, 50_000, 1_000_000), &p).is_honeypot());
        // One bps above the floor (501 bps) => no longer a honeypot; the large skim is Taxed.
        assert!(matches!(
            classify_sell(res(None, 50_100, 1_000_000), &p),
            SellVerdict::Taxed { .. }
        ));
    }

    #[test]
    fn tax_tolerance_is_inclusive_at_the_boundary() {
        let p = SellSimPolicy::default(); // tolerance 100 bps (1%)
                                          // Skim EXACTLY 1 % => tolerated => Sellable (inclusive `<=`).
        assert_eq!(
            classify_sell(res(None, 990_000, 1_000_000), &p),
            SellVerdict::Sellable {
                realized_out_lamports: 990_000
            }
        );
        // One bps over tolerance (101 bps skim) => Taxed, skim priced.
        assert_eq!(
            classify_sell(res(None, 989_900, 1_000_000), &p),
            SellVerdict::Taxed {
                tax_bps: 101,
                skim_lamports: 10_100
            }
        );
    }

    struct CannedSim(Result<SellSimResult, SellSimError>);
    impl SellSimulator for CannedSim {
        fn simulate_sell(&self, _mint: &Pubkey, _amt: u64) -> Result<SellSimResult, SellSimError> {
            self.0.clone()
        }
    }

    #[test]
    fn gate_threads_simulate_then_classify() {
        let sim = CannedSim(Ok(res(None, 920_000, 1_000_000)));
        let v = run_sell_sim_gate(&sim, &mint(7), 1_000_000, &SellSimPolicy::default()).unwrap();
        assert!(matches!(v, SellVerdict::Taxed { .. }));
    }

    #[test]
    fn transport_failure_surfaces() {
        let sim = CannedSim(Err(SellSimError::Transport("rpc down".into())));
        assert!(matches!(
            run_sell_sim_gate(&sim, &mint(7), 1_000_000, &SellSimPolicy::default()),
            Err(SellSimError::Transport(_))
        ));
    }

    /// add-3 → observ-4: a `Taxed` verdict's skim, folded into `EconParams.e_rug_honeypot_lamports`,
    /// lowers `E[net]` and can flip the cost-gate to reject — the whole point of the term.
    #[test]
    fn taxed_skim_feeds_the_cost_gate() {
        use crate::metrics::econ::{CostInputs, CostModel, EconParams};

        let inputs = CostInputs {
            spread_lamports: 100_000,
            swap_fees_lamports: 10_000,
            flash_fee_lamports: 0,
            tip_lamports: 20_000,
            prio_lamports: 5_000,
            base_lamports: 5_000,
            p_land: 1.0,
        };
        // Without the rug term the edge is positive and the gate proceeds.
        let clean = CostModel::new(EconParams::default());
        assert!(clean.gate(&inputs).is_proceed());

        // A heavy exit tax (skim) priced in turns it negative.
        let v = classify_sell(res(None, 400_000, 1_000_000), &SellSimPolicy::default());
        let taxed = CostModel::new(EconParams {
            e_rug_honeypot_lamports: v.e_rug_honeypot_lamports(),
            ..EconParams::default()
        });
        assert_eq!(v.e_rug_honeypot_lamports(), 600_000);
        assert!(!taxed.gate(&inputs).is_proceed());
    }
}
