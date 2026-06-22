//! Executor config + validation (landing-1). Rejects an out-of-band tip percentile band or an
//! invalid profit-fraction cap at load, so a misconfig can never widen the tip auction unbounded.

use super::types::{Region, SenderMode};

/// Why a config failed validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Tip percentile band must satisfy `0 <= low <= high <= 1` within the documented 50–75 range.
    TipBandOutOfRange,
    /// `profit_cap_frac` must be in `(0, 1]`.
    ProfitCapFracOutOfRange,
    /// `no_land_ms` must be positive.
    NoLandTimeoutZero,
    /// `max_attempts` must be positive.
    MaxAttemptsZero,
}

/// Executor runtime config (mirrors `executor.toml` defaults).
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutorConfig {
    /// Region fan-out order (nearest first).
    pub region_order: Vec<Region>,
    /// Tip percentile band low edge (default 0.50).
    pub tip_percentile_low: f64,
    /// Tip percentile band high edge (default 0.75).
    pub tip_percentile_high: f64,
    /// Cap tip at this fraction of simulated profit (default 0.5).
    pub profit_cap_frac: f64,
    /// No-land rebuild threshold in ms (default 2500).
    pub no_land_ms: u64,
    /// Inflight poll window in seconds (default 300).
    pub inflight_window_s: u64,
    /// Max landing attempts before GaveUp (default 4).
    pub max_attempts: u8,
    /// Default Helius Sender mode.
    pub sender_mode: SenderMode,
    /// Durable-nonce path (landing-10) — disabled for M1.
    pub durable_nonce_enabled: bool,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            region_order: vec![Region::Frankfurt, Region::Amsterdam, Region::Ny],
            tip_percentile_low: 0.50,
            tip_percentile_high: 0.75,
            profit_cap_frac: 0.5,
            no_land_ms: 2_500,
            inflight_window_s: 300,
            max_attempts: 4,
            sender_mode: SenderMode::SwqosOnly,
            durable_nonce_enabled: false,
        }
    }
}

impl ExecutorConfig {
    /// Validate the band + caps. Called at load.
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Band must sit inside [0.50, 0.75] and be ordered (plan §6 default band).
        if !(0.50..=0.75).contains(&self.tip_percentile_low)
            || !(0.50..=0.75).contains(&self.tip_percentile_high)
            || self.tip_percentile_low > self.tip_percentile_high
        {
            return Err(ConfigError::TipBandOutOfRange);
        }
        if !(self.profit_cap_frac > 0.0 && self.profit_cap_frac <= 1.0) {
            return Err(ConfigError::ProfitCapFracOutOfRange);
        }
        if self.no_land_ms == 0 {
            return Err(ConfigError::NoLandTimeoutZero);
        }
        if self.max_attempts == 0 {
            return Err(ConfigError::MaxAttemptsZero);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        assert!(ExecutorConfig::default().validate().is_ok());
    }

    #[test]
    fn rejects_tip_band_outside_50_75() {
        let c = ExecutorConfig {
            tip_percentile_high: 0.90,
            ..Default::default()
        };
        assert_eq!(c.validate(), Err(ConfigError::TipBandOutOfRange));
        let c = ExecutorConfig {
            tip_percentile_low: 0.60,
            tip_percentile_high: 0.55, // low > high
            ..Default::default()
        };
        assert_eq!(c.validate(), Err(ConfigError::TipBandOutOfRange));
    }

    #[test]
    fn rejects_profit_cap_frac_outside_unit() {
        let c = ExecutorConfig {
            profit_cap_frac: 0.0,
            ..Default::default()
        };
        assert_eq!(c.validate(), Err(ConfigError::ProfitCapFracOutOfRange));
        let c = ExecutorConfig {
            profit_cap_frac: 1.5,
            ..Default::default()
        };
        assert_eq!(c.validate(), Err(ConfigError::ProfitCapFracOutOfRange));
    }
}
