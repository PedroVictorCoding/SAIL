//! Round 25 B7: dividend tax for a Portuguese-resident owner, treaty-aware.
//!
//! Two taxes reach a dividend:
//!
//! 1. **Withholding at source**, by the paying company's (or fund's) country of domicile,
//!    taken from the ISIN's first two letters when known, else the XTB listing suffix. It
//!    is only a default: the projection uses the rate *observed* in the ledger whenever
//!    the holding has paid in the last 24 months ([`super::Holding::withholding_source`]).
//!    - US: 15% with a W-8BEN on file (the US–PT treaty rate), else the statutory 30%.
//!    - Funds (ETFs) not domiciled in the US: 0% wherever they're listed. Irish and
//!      Luxembourg UCITS distribute without withholding, and German funds don't withhold
//!      from non-residents. The US tax on an Irish fund's own US dividends (15% under the
//!      US–IE treaty) is paid inside the fund and already lowers what it distributes, so
//!      it isn't taken again here. XTB's reports carry no ISIN, so for a fund the
//!      listing suffix says nothing about domicile.
//!    - IE (a company or unknown kind): 0%.
//!    - PT: 28%, the final *taxa liberatória* withheld from residents (not the 25% a
//!      non-resident pays).
//!    - The rest as before ([`super::default_withholding`]): GB 0%, DE 26.375%, FR 25%,
//!      NL 15%, ES 19%, IT 26%, anything else 15%.
//! 2. **Portuguese tax** on what is left, set aside only when the owner asks for the
//!    reserve. `reserve = max(0, rate × gross − withheld)`: foreign withholding is
//!    credited up to the Portuguese tax on that income and never refunded.
//!    - **Flat** (the default, *tributação autónoma*): `rate = 28%`.
//!    - **Englobamento** (opting to add capital income to the year's other income):
//!      `rate = marginal × inclusion`, where inclusion is 50% for dividends of companies
//!      domiciled in the EU or EEA (CIRS art. 40.º-A) and 100% otherwise and for funds.
//!      The marginal rate is the owner's own, clamped to `[0, 53%]` (48% top bracket plus
//!      the 5% solidarity surcharge). The election covers all capital income of the year;
//!      SAIL only models the dividends.
//!
//! **Accumulating funds** pay nothing, so the projection can't show income from them. The
//! *income equivalent* is what selling [`ACCUMULATING_WITHDRAWAL`] of their value a year
//! would give (the classic 4% withdrawal rule, fixed a priori). It is before the 28% tax
//! on the gain part of each sale, which depends on cost and isn't modelled.
//!
//! **Purity.** No I/O, no clock.

use serde::{Deserialize, Serialize};

/// The Portuguese flat rate on dividends (*taxa liberatória / autónoma*).
pub const PT_FLAT: f64 = super::PT_TAX;

/// Highest marginal rate accepted for englobamento: 48% top bracket plus the 5%
/// solidarity surcharge (fixed by law, 2026).
pub const MAX_MARGINAL: f64 = 0.53;

/// Share of an accumulating fund's value counted as yearly income (the 4% rule, fixed a
/// priori).
pub const ACCUMULATING_WITHDRAWAL: f64 = 0.04;

/// US withholding with a W-8BEN (US–PT treaty, art. 10).
pub const US_TREATY: f64 = 0.15;

/// US statutory withholding without a W-8BEN.
pub const US_STATUTORY: f64 = 0.30;

/// EU and EEA member countries (ISO 3166-1 alpha-2), for the englobamento 50% inclusion.
const EU_EEA: &[&str] = &[
    "AT", "BE", "BG", "HR", "CY", "CZ", "DK", "EE", "FI", "FR", "DE", "GR", "HU", "IE", "IT", "LV",
    "LT", "LU", "MT", "NL", "PL", "PT", "RO", "SK", "SI", "ES", "SE", "IS", "LI", "NO",
];

/// How Portuguese tax on dividends is settled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PtRegime {
    /// 28% flat.
    #[default]
    Flat,
    /// Added to other income at the owner's marginal rate.
    Englobamento {
        /// Marginal IRS rate, 0..=[`MAX_MARGINAL`].
        marginal_rate: f64,
    },
}

/// The owner's tax situation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaxProfile {
    /// A W-8BEN is on file with the broker.
    pub w8ben: bool,
    /// How Portuguese tax is settled.
    pub pt_regime: PtRegime,
}

impl Default for TaxProfile {
    /// XTB collects a W-8BEN at account opening; flat 28% is the default regime.
    fn default() -> Self {
        Self {
            w8ben: true,
            pt_regime: PtRegime::Flat,
        }
    }
}

impl TaxProfile {
    /// The profile with every number inside its allowed range (a non-finite marginal rate
    /// becomes 0).
    pub fn clamped(self) -> Self {
        let pt_regime = match self.pt_regime {
            PtRegime::Flat => PtRegime::Flat,
            PtRegime::Englobamento { marginal_rate } => PtRegime::Englobamento {
                marginal_rate: if marginal_rate.is_finite() {
                    marginal_rate.clamp(0.0, MAX_MARGINAL)
                } else {
                    0.0
                },
            },
        };
        Self {
            w8ben: self.w8ben,
            pt_regime,
        }
    }
}

/// Country of domicile: the ISIN's first two letters when they look like a country code,
/// else the XTB suffix (`.UK` → `GB`), else `None`.
pub fn domicile(isin: Option<&str>, symbol: &str) -> Option<String> {
    if let Some(prefix) = isin.and_then(|i| i.get(..2))
        && prefix.chars().all(|c| c.is_ascii_uppercase())
    {
        return Some(prefix.to_string());
    }
    let suffix = symbol.rsplit_once('.')?.1.to_ascii_uppercase();
    match suffix.as_str() {
        "UK" => Some("GB".into()),
        s if s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()) => Some(s.into()),
        _ => None,
    }
}

/// Default withholding at source and a short reason, for a holding with no observed rate.
///
/// A fund (ETF) that isn't US-domiciled defaults to 0% wherever it is listed: Irish and
/// Luxembourg UCITS distribute without withholding, and so do German funds to
/// non-residents. XTB's reports carry no ISIN, so a fund's listing suffix says nothing
/// about where it is domiciled.
pub fn source_withholding(
    profile: &TaxProfile,
    domicile: Option<&str>,
    symbol: &str,
    fund: bool,
) -> (f64, &'static str) {
    match domicile {
        Some("US") if profile.w8ben => (US_TREATY, "US treaty rate with W-8BEN"),
        Some("US") => (US_STATUTORY, "US rate without W-8BEN"),
        _ if fund => (0.0, "fund: no withholding on distributions"),
        Some("IE") => (0.0, "Irish fund: no withholding on distributions"),
        Some("PT") => (PT_FLAT, "Portuguese final withholding"),
        Some("GB") => (0.0, "UK: no withholding"),
        Some("DE") => (0.26375, "German withholding"),
        Some("FR") => (0.25, "French withholding"),
        Some("NL") => (0.15, "Dutch withholding"),
        Some("ES") => (0.19, "Spanish withholding"),
        Some("IT") => (0.26, "Italian withholding"),
        Some(_) => (0.15, "default rate"),
        None => (super::default_withholding(symbol), "default by listing"),
    }
}

/// The Portuguese rate applied to the gross dividend before crediting withholding.
pub fn pt_rate(profile: &TaxProfile, domicile: Option<&str>, fund: bool) -> f64 {
    match profile.clamped().pt_regime {
        PtRegime::Flat => PT_FLAT,
        PtRegime::Englobamento { marginal_rate } => {
            let eu = domicile.is_some_and(|d| EU_EEA.contains(&d));
            let inclusion = if eu && !fund { 0.5 } else { 1.0 };
            marginal_rate * inclusion
        }
    }
}

/// Portuguese tax still due on one payment: `max(0, rate × gross − withheld)`.
pub fn pt_reserve(gross: f64, withheld: f64, rate: f64) -> f64 {
    if !(gross.is_finite() && withheld.is_finite() && rate.is_finite()) {
        return 0.0;
    }
    let due = rate.max(0.0) * gross - withheld.max(0.0);
    // Rounding noise (28% × gross − the same 28% withheld) is not tax due.
    if due <= 1e-9 * gross.abs() { 0.0 } else { due }
}

/// Yearly income equivalent of an accumulating holding worth `value`.
pub fn income_equivalent(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value * ACCUMULATING_WITHDRAWAL
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENGLOBAMENTO_35: TaxProfile = TaxProfile {
        w8ben: true,
        pt_regime: PtRegime::Englobamento {
            marginal_rate: 0.35,
        },
    };

    #[test]
    fn domicile_prefers_the_isin_then_the_listing() {
        assert_eq!(
            domicile(Some("IE00B3RBWM25"), "VWRL.DE").as_deref(),
            Some("IE")
        );
        assert_eq!(domicile(None, "AAPL.US").as_deref(), Some("US"));
        assert_eq!(domicile(None, "VUSA.UK").as_deref(), Some("GB"));
        assert_eq!(domicile(Some("x"), "EDP.PT").as_deref(), Some("PT"));
        assert_eq!(domicile(None, "BTCUSD"), None);
    }

    #[test]
    fn w8ben_halves_us_withholding_and_irish_funds_pay_none() {
        let with = TaxProfile::default();
        let without = TaxProfile {
            w8ben: false,
            ..with
        };
        assert_eq!(
            source_withholding(&with, Some("US"), "KO.US", false).0,
            0.15
        );
        assert_eq!(
            source_withholding(&without, Some("US"), "KO.US", false).0,
            0.30
        );
        // An Irish UCITS listed in Germany is not taxed as a German dividend.
        assert_eq!(
            source_withholding(&with, Some("IE"), "VWRL.DE", false).0,
            0.0
        );
        assert_eq!(
            source_withholding(&with, Some("PT"), "EDP.PT", false).0,
            0.28
        );
        assert_eq!(source_withholding(&with, None, "SAP.DE", false).0, 0.26375);
        // A fund listed in Germany is not a German dividend; a US ETF still is a US one.
        assert_eq!(
            source_withholding(&with, Some("DE"), "EVSD.DE", true).0,
            0.0
        );
        assert_eq!(
            source_withholding(&with, Some("US"), "SCHD.US", true).0,
            0.15
        );
    }

    #[test]
    fn flat_regime_credits_withholding_up_to_28_percent() {
        let rate = pt_rate(&TaxProfile::default(), Some("US"), false);
        assert_eq!(rate, 0.28);
        assert!((pt_reserve(100.0, 15.0, rate) - 13.0).abs() < 1e-9);
        // More withheld than Portugal would take: nothing more, nothing back.
        assert_eq!(pt_reserve(100.0, 30.0, rate), 0.0);
        // Portuguese dividends: the final 28% was already withheld.
        assert_eq!(pt_reserve(100.0, 28.0, rate), 0.0);
    }

    #[test]
    fn englobamento_includes_half_of_eu_company_dividends_only() {
        assert!((pt_rate(&ENGLOBAMENTO_35, Some("DE"), false) - 0.175).abs() < 1e-12);
        assert!((pt_rate(&ENGLOBAMENTO_35, Some("PT"), false) - 0.175).abs() < 1e-12);
        assert!((pt_rate(&ENGLOBAMENTO_35, Some("US"), false) - 0.35).abs() < 1e-12);
        // Funds don't get the 50% exclusion, even EU-domiciled ones.
        assert!((pt_rate(&ENGLOBAMENTO_35, Some("IE"), true) - 0.35).abs() < 1e-12);
        // A low marginal rate can mean nothing further is due on a US dividend.
        let low = TaxProfile {
            pt_regime: PtRegime::Englobamento {
                marginal_rate: 0.13,
            },
            ..ENGLOBAMENTO_35
        };
        assert_eq!(
            pt_reserve(100.0, 15.0, pt_rate(&low, Some("US"), false)),
            0.0
        );
    }

    #[test]
    fn hostile_numbers_are_clamped() {
        for marginal_rate in [f64::NAN, -1.0, 2.0, f64::INFINITY] {
            let profile = TaxProfile {
                w8ben: true,
                pt_regime: PtRegime::Englobamento { marginal_rate },
            };
            let rate = pt_rate(&profile, Some("US"), false);
            assert!(
                (0.0..=MAX_MARGINAL).contains(&rate),
                "{marginal_rate} → {rate}"
            );
        }
        assert_eq!(pt_reserve(f64::NAN, 0.0, 0.28), 0.0);
        assert_eq!(income_equivalent(-5.0), 0.0);
        assert_eq!(income_equivalent(f64::NAN), 0.0);
        assert!((income_equivalent(10_000.0) - 400.0).abs() < 1e-9);
    }

    #[test]
    fn pure_functions_repeat() {
        let p = ENGLOBAMENTO_35;
        assert_eq!(
            pt_rate(&p, Some("FR"), false),
            pt_rate(&p, Some("FR"), false)
        );
        assert_eq!(
            source_withholding(&p, Some("US"), "X.US", false),
            source_withholding(&p, Some("US"), "X.US", false)
        );
    }
}
