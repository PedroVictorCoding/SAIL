//! The owner's risk policy (round 11, CAP-18). Every policy proposal and every idea is
//! checked against it. The EPIC requires that no numeric policy be used without the
//! owner's review, so SAIL starts from a **draft** that the owner edits and confirms.
//! Until a version is confirmed, no proposal is sized against it.
//!
//! Versions are append-only: each edit is a new version, and suggestions name the
//! version they used.
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Whether the owner has confirmed this version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyStatus {
    /// SAIL's starting values, or an edit not yet confirmed.
    Draft,
    /// The owner confirmed these values.
    Confirmed,
}

/// The limits. Percentages are shares of the account value (owned positions plus free
/// funds; CFDs are excluded), written as `Decimal` percent, e.g. `10` for 10%.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskLimits {
    /// Largest share any one instrument may have.
    #[serde(with = "rust_decimal::serde::str")]
    pub max_position_pct: Decimal,
    /// Least free funds to keep, as a reserve.
    #[serde(with = "rust_decimal::serde::str")]
    pub cash_min_pct: Decimal,
    /// Most free funds to leave idle.
    #[serde(with = "rust_decimal::serde::str")]
    pub cash_max_pct: Decimal,
    /// Annualized volatility above which an instrument counts as high-volatility.
    #[serde(with = "rust_decimal::serde::str")]
    pub high_vol_threshold_pct: Decimal,
    /// Largest combined share of high-volatility instruments.
    #[serde(with = "rust_decimal::serde::str")]
    pub max_high_vol_pct: Decimal,
    /// Largest share of the account one proposal may move (sum of absolute changes / 2).
    #[serde(with = "rust_decimal::serde::str")]
    pub max_turnover_pct: Decimal,
    /// Smallest change worth proposing, in the account currency.
    #[serde(with = "rust_decimal::serde::str")]
    pub min_trade: Decimal,
    /// Estimated cost of one trade, in basis points of its value (spread and fees).
    #[serde(with = "rust_decimal::serde::str")]
    pub trade_cost_bps: Decimal,
    /// Extra cost when the instrument trades in another currency (conversion fee), bps.
    #[serde(with = "rust_decimal::serde::str")]
    pub fx_cost_bps: Decimal,
    /// How much the owner dislikes swings: the `λ` in `α'w − λ/2·w'Σw`. Higher is more
    /// cautious. 1 is bold, 4 moderate, 10 very cautious.
    #[serde(with = "rust_decimal::serde::str")]
    pub risk_aversion: Decimal,
    /// Share of the account lost if a trade signal's stop is hit, percent (round 13).
    /// Signal cards are sized from it.
    #[serde(default = "default_risk_per_trade", with = "rust_decimal::serde::str")]
    pub risk_per_trade_pct: Decimal,
}

fn default_risk_per_trade() -> Decimal {
    Decimal::new(5, 1)
}

impl RiskLimits {
    /// SAIL's starting values. Moderate, long-only, and always shown as a draft.
    pub fn draft() -> Self {
        Self {
            max_position_pct: Decimal::from(10),
            cash_min_pct: Decimal::from(5),
            cash_max_pct: Decimal::from(15),
            high_vol_threshold_pct: Decimal::from(50),
            max_high_vol_pct: Decimal::from(15),
            max_turnover_pct: Decimal::from(20),
            min_trade: Decimal::from(50),
            trade_cost_bps: Decimal::from(10),
            fx_cost_bps: Decimal::from(50),
            risk_aversion: Decimal::from(4),
            risk_per_trade_pct: default_risk_per_trade(),
        }
    }

    /// Checks the values make sense together. The message is safe to show.
    pub fn validate(&self) -> Result<(), String> {
        let pct = |name: &str, v: Decimal| {
            if v < Decimal::ZERO || v > Decimal::from(100) {
                Err(format!("{name} must be between 0 and 100"))
            } else {
                Ok(())
            }
        };
        pct("Largest position", self.max_position_pct)?;
        pct("Cash reserve", self.cash_min_pct)?;
        pct("Idle cash limit", self.cash_max_pct)?;
        pct("High-volatility share", self.max_high_vol_pct)?;
        pct("Turnover per proposal", self.max_turnover_pct)?;
        if self.max_position_pct <= Decimal::ZERO {
            return Err("Largest position must be above 0".into());
        }
        if self.cash_min_pct > self.cash_max_pct {
            return Err("The cash reserve can't be above the idle cash limit".into());
        }
        if self.high_vol_threshold_pct <= Decimal::ZERO
            || self.high_vol_threshold_pct > Decimal::from(500)
        {
            return Err("The high-volatility threshold must be between 0 and 500".into());
        }
        if self.min_trade < Decimal::ZERO {
            return Err("The smallest change can't be negative".into());
        }
        if self.trade_cost_bps < Decimal::ZERO
            || self.trade_cost_bps > Decimal::from(500)
            || self.fx_cost_bps < Decimal::ZERO
            || self.fx_cost_bps > Decimal::from(500)
        {
            return Err("Costs must be between 0 and 500 basis points".into());
        }
        if self.risk_aversion <= Decimal::ZERO || self.risk_aversion > Decimal::from(100) {
            return Err("Caution must be between 0 and 100".into());
        }
        if self.risk_per_trade_pct <= Decimal::ZERO || self.risk_per_trade_pct > Decimal::from(5) {
            return Err("Risk per trade must be above 0% and at most 5%".into());
        }
        Ok(())
    }
}

/// One stored version of the policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskPolicy {
    /// 1, 2, ... per account; 0 for the never-stored built-in draft.
    pub version: u32,
    /// Draft or confirmed.
    pub status: PolicyStatus,
    /// The limits.
    pub limits: RiskLimits,
    /// When this version was stored (`None` for the built-in draft).
    pub saved_at: Option<DateTime<Utc>>,
}

impl RiskPolicy {
    /// The built-in draft, used until the owner saves a version.
    pub fn builtin_draft() -> Self {
        Self {
            version: 0,
            status: PolicyStatus::Draft,
            limits: RiskLimits::draft(),
            saved_at: None,
        }
    }

    /// True when proposals may be sized against it.
    pub fn is_confirmed(&self) -> bool {
        self.status == PolicyStatus::Confirmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_draft_is_valid_and_unconfirmed() {
        assert!(RiskLimits::draft().validate().is_ok());
        assert!(!RiskPolicy::builtin_draft().is_confirmed());
    }

    #[test]
    fn inconsistent_cash_limits_are_refused() {
        let mut limits = RiskLimits::draft();
        limits.cash_min_pct = Decimal::from(20);
        assert!(limits.validate().unwrap_err().contains("cash reserve"));
        limits = RiskLimits::draft();
        limits.max_position_pct = Decimal::from(101);
        assert!(limits.validate().is_err());
    }
}
