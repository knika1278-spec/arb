//! Cross-module shared types so the on-chain program and the off-chain bot agree on
//! wire/ABI shapes without a circular dependency. `no_std` so the on-chain hot path can
//! link it directly.
//!
//! `ArbError` codes are **stable** and Anchor-style 6000-based: the program returns them
//! via `ProgramError::Custom(code)`, and the bot decodes revert reasons against the same
//! enum. Never renumber an existing variant — only append.
#![no_std]
#![forbid(unsafe_code)]

/// Stable error codes returned by the on-chain `TryArbitrage` program and decoded by the
/// bot. `#[repr(u32)]` + explicit discriminants make the numeric ABI the contract.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArbError {
    /// Terminal profit-assert failed: `out < in + min_profit + costs`. The runtime
    /// reverts ALL state. This is the *expected* outcome of most attempts.
    Unprofitable = 6000,
    /// A swap-CPI target was not in the Wave-1 DEX allowlist (trust-boundary breach).
    UnauthorizedProgram = 6001,
    /// A balance-read token account is not owned by the bot authority (griefer-supplied
    /// account via `remaining_accounts`).
    UnauthorizedTokenAccountOwner = 6002,
    /// A routed mint carries a HARD-REJECT Token-2022 extension (hook/frozen/etc.).
    ForbiddenTokenExtension = 6003,
    /// Instruction data could not be parsed into `TryArbitrageData`.
    MalformedInstructionData = 6004,
    /// `remaining_accounts` did not match the canonical per-leg ordering/count.
    InvalidAccountsList = 6005,
    /// Realized output of a leg was below the caller-asserted minimum (slippage).
    SlippageExceeded = 6006,
    /// Checked integer arithmetic overflowed/underflowed while computing balances.
    ArithmeticOverflow = 6007,
    /// No arbitrage direction was supplied / the route was empty.
    NoArbitrage = 6008,
    /// An account expected to be a writable signer was not.
    MissingRequiredSignature = 6009,
    /// Round-trip did not close: leg B failed to consume the intermediate asset back to its
    /// pre-trade level, so the route would strand inventory (a mis-resolved second leg) even
    /// if the base balance grew. The inventory-safety invariant (plan §6 / add-2).
    RouteDoesNotClose = 6010,
}

impl ArbError {
    /// The stable numeric code (what crosses the program boundary as `Custom(code)`).
    #[inline]
    pub const fn code(self) -> u32 {
        self as u32
    }

    /// Decode a numeric custom-error code back into an `ArbError` (bot revert-reason path).
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            6000 => Some(Self::Unprofitable),
            6001 => Some(Self::UnauthorizedProgram),
            6002 => Some(Self::UnauthorizedTokenAccountOwner),
            6003 => Some(Self::ForbiddenTokenExtension),
            6004 => Some(Self::MalformedInstructionData),
            6005 => Some(Self::InvalidAccountsList),
            6006 => Some(Self::SlippageExceeded),
            6007 => Some(Self::ArithmeticOverflow),
            6008 => Some(Self::NoArbitrage),
            6009 => Some(Self::MissingRequiredSignature),
            6010 => Some(Self::RouteDoesNotClose),
            _ => None,
        }
    }
}

/// Venue discriminant shared across detection / sizing / tx-builder. Wave-1 only;
/// deferred venues are added when their adapters land (Fase 3).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DexKind {
    RaydiumCpmm = 0,
    OrcaWhirlpool = 1,
    PumpSwapAmm = 2,
}

impl DexKind {
    /// Stable wire tag.
    #[inline]
    pub const fn tag(self) -> u8 {
        self as u8
    }

    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::RaydiumCpmm),
            1 => Some(Self::OrcaWhirlpool),
            2 => Some(Self::PumpSwapAmm),
            _ => None,
        }
    }

    /// All three constant-product venues are pure `x*y=k` for Milestone 1.
    #[inline]
    pub const fn is_constant_product(self) -> bool {
        matches!(
            self,
            Self::RaydiumCpmm | Self::OrcaWhirlpool | Self::PumpSwapAmm
        )
    }
}

/// Swap direction for a single leg, used by both the sizing quoter and the on-chain
/// adapter so off-chain prediction and on-chain execution agree on orientation.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SwapDir {
    /// base -> quote (token A in, token B out)
    AtoB = 0,
    /// quote -> base (token B in, token A out)
    BtoA = 1,
}

impl SwapDir {
    /// Stable wire tag.
    #[inline]
    pub const fn tag(self) -> u8 {
        self as u8
    }

    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::AtoB),
            1 => Some(Self::BtoA),
            _ => None,
        }
    }

    #[inline]
    pub const fn flip(self) -> Self {
        match self {
            Self::AtoB => Self::BtoA,
            Self::BtoA => Self::AtoB,
        }
    }
}

/// **dec-3 — the single canonical `min_profit` definition.** Base/priority/tip are SOL-lamport
/// costs that are *not* visible as a base-asset balance delta, so the off-chain side must bake
/// them into the on-chain assert's `min_profit`. This struct pins the one formula
/// (`min_profit = swap_fees + priority + base_fee + tip + margin`, all in base-asset units)
/// that sizing, the tx-builder's profit expectation, and the on-chain `TryArbitrage` assert all
/// reference, so a profitable-looking trade cannot revert from a definition drift.
///
/// `no_std` + integer-only + `saturating_*` so the value is bit-identical wherever it is computed
/// (and the on-chain assert never panics on overflow). For base==WSOL the lamport costs and the
/// WSOL balance delta are the same asset (plan §9 dec-3); for base==USDC the caller converts the
/// SOL-lamport costs into base units before populating these fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CostTerms {
    /// Pool swap fees already paid inside the legs, expressed in base-asset units.
    pub swap_fees: u64,
    /// Priority fee (`cu_price * cu_limit`) in base-asset units.
    pub priority: u64,
    /// Base signature fee (5000 lamports/sig) in base-asset units.
    pub base_fee: u64,
    /// Jito tip placed *inside* the atomic tx, in base-asset units.
    pub tip: u64,
    /// Safety margin (rounding/overflow/opportunity-decay buffer).
    pub margin: u64,
}

impl CostTerms {
    /// The single `min_profit` value handed to `TryArbitrageData.min_profit`. The on-chain
    /// assert requires `out >= in + min_profit`; off-chain sizing must clear the same bar.
    #[inline]
    pub const fn min_profit(&self) -> u64 {
        self.swap_fees
            .saturating_add(self.priority)
            .saturating_add(self.base_fee)
            .saturating_add(self.tip)
            .saturating_add(self.margin)
    }

    /// Sum of the *unavoidable* costs paid even on a reverted attempt (base + priority). The tip
    /// rides inside the atomic tx, so it is NOT burned on revert (plan §2) and is excluded here.
    #[inline]
    pub const fn burn_on_revert(&self) -> u64 {
        self.base_fee.saturating_add(self.priority)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_profit_sums_all_cost_terms() {
        let c = CostTerms {
            swap_fees: 1_000,
            priority: 2_000,
            base_fee: 5_000,
            tip: 10_000,
            margin: 500,
        };
        assert_eq!(c.min_profit(), 18_500);
        // Tip rides inside the atomic tx => excluded from revert burn.
        assert_eq!(c.burn_on_revert(), 7_000);
    }

    #[test]
    fn min_profit_saturates_instead_of_overflowing() {
        let c = CostTerms {
            swap_fees: u64::MAX,
            priority: 1,
            base_fee: 0,
            tip: 0,
            margin: 0,
        };
        assert_eq!(c.min_profit(), u64::MAX);
    }

    #[test]
    fn error_codes_are_stable_and_roundtrip() {
        assert_eq!(ArbError::Unprofitable.code(), 6000);
        assert_eq!(ArbError::MissingRequiredSignature.code(), 6009);
        assert_eq!(ArbError::RouteDoesNotClose.code(), 6010);
        for code in 6000..=6010u32 {
            let e = ArbError::from_code(code).expect("known code");
            assert_eq!(e.code(), code);
        }
        assert_eq!(ArbError::from_code(5999), None);
        assert_eq!(ArbError::from_code(6011), None);
    }

    #[test]
    fn dexkind_tags_roundtrip() {
        for k in [
            DexKind::RaydiumCpmm,
            DexKind::OrcaWhirlpool,
            DexKind::PumpSwapAmm,
        ] {
            assert_eq!(DexKind::from_tag(k.tag()), Some(k));
            assert!(k.is_constant_product());
        }
        assert_eq!(DexKind::from_tag(3), None);
    }

    #[test]
    fn swapdir_flips() {
        assert_eq!(SwapDir::AtoB.flip(), SwapDir::BtoA);
        assert_eq!(SwapDir::BtoA.flip().flip(), SwapDir::BtoA);
    }
}
