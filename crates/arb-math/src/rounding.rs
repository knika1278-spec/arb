//! Rounding direction. The on-chain DEX math **always favors the pool**: output is
//! floored, required-input is ceiled. Off-chain prediction MUST mirror this exactly or
//! realized output diverges from predicted and the tx reverts (burning CU + fee).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundDirection {
    /// Round toward zero — used for swap *output* amounts.
    Floor,
    /// Round away from zero — used for *required input* amounts.
    Ceil,
}
