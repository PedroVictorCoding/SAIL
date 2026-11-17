//! Round 25 B7: the tax profile reaches the projection's reserve, accumulating funds get
//! an income equivalent, and the profile is stored per account. Synthetic data only.

use chrono::{NaiveDate, TimeZone, Utc};
use sail_capital::contracts::AccountId;
use sail_capital::dividends::tax::{PtRegime, TaxProfile, pt_rate};
use sail_capital::dividends::{
    Allocation, DEFAULT_TOP_N, Holding, Payment, Scenario, Settings, project,
};
use sail_capital::store::CapitalStore;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
}

fn settings(reserve_pt_tax: bool) -> Settings {
    Settings {
        years: 1,
        reinvest_pct: 0.0,
        allocation: Allocation::Proportional,
        top_n: DEFAULT_TOP_N,
        monthly_contribution: 0.0,
        reserve_pt_tax,
        start: date(2027, 1, 1),
    }
}

/// 100 units paying 1.0 a share each June, so the year's gross is 100.
fn payer(symbol: &str, withholding: f64, pt_rate: f64) -> Holding {
    Holding {
        key: symbol.to_ascii_lowercase(),
        symbol: symbol.into(),
        units: 100.0,
        price: 25.0,
        fx: 1.0,
        history: (2021..=2026)
            .map(|y| Payment {
                ex_date: date(y, 6, 15),
                per_share: 1.0,
            })
            .collect(),
        withholding,
        withholding_source: "default".into(),
        pt_rate,
        price_growth: None,
    }
}

fn year_one(holdings: &[Holding], reserve: bool) -> (f64, f64) {
    let outlook = project(holdings, &settings(reserve), None);
    let base = outlook
        .scenarios
        .iter()
        .find(|p| p.scenario == Scenario::Base)
        .expect("base");
    (base.years[0].pt_tax, base.years[0].net)
}

#[test]
fn the_reserve_follows_each_holdings_portuguese_rate() {
    // Flat: 28% of 100 less the 15% withheld.
    let (pt, net) = year_one(&[payer("KO.US", 0.15, 0.28)], true);
    assert!((pt - 13.0).abs() < 1e-9, "{pt}");
    assert!((net - 72.0).abs() < 1e-9, "{net}");

    // Englobamento at 35% on a German company: half is included, 17.5% < 26.375% withheld.
    let englobamento = TaxProfile {
        w8ben: true,
        pt_regime: PtRegime::Englobamento {
            marginal_rate: 0.35,
        },
    };
    let rate = pt_rate(&englobamento, Some("DE"), false);
    let (pt, _) = year_one(&[payer("SAP.DE", 0.26375, rate)], true);
    assert_eq!(pt, 0.0);

    // Without the reserve, the rate is ignored.
    let (pt, net) = year_one(&[payer("KO.US", 0.15, 0.28)], false);
    assert_eq!(pt, 0.0);
    assert!((net - 85.0).abs() < 1e-9);
}

#[test]
fn accumulating_holdings_get_a_four_percent_income_equivalent_largest_first() {
    let mut small = payer("CSPX.UK", 0.0, 0.28);
    small.history.clear();
    small.units = 10.0;
    small.price = 500.0;
    let mut large = payer("IWDA.NL", 0.0, 0.28);
    large.history.clear();
    large.units = 200.0;
    large.price = 100.0;
    large.fx = 1.0;
    let outlook = project(
        &[small, payer("KO.US", 0.15, 0.28), large],
        &settings(false),
        None,
    );

    let rows: Vec<(&str, f64, f64)> = outlook
        .accumulating
        .iter()
        .map(|r| (r.symbol.as_str(), r.value, r.annual_equivalent))
        .collect();
    assert_eq!(
        rows,
        vec![("IWDA.NL", 20_000.0, 800.0), ("CSPX.UK", 5_000.0, 200.0)]
    );
    // Payers never appear, and the old non-paying list is unchanged.
    assert_eq!(outlook.non_paying, vec!["CSPX.UK", "IWDA.NL"]);
}

#[test]
fn the_profile_is_stored_per_account_and_replaced() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = CapitalStore::open(dir.path()).expect("store");
    let a = AccountId::from_stored("xtb:a");
    let b = AccountId::from_stored("xtb:b");
    let now = Utc.with_ymd_and_hms(2026, 11, 16, 12, 0, 0).unwrap();
    assert_eq!(store.tax_profile(&a).expect("read"), None);

    let first = TaxProfile {
        w8ben: false,
        pt_regime: PtRegime::Flat,
    };
    store.set_tax_profile(&a, &first, now).expect("write");
    let second = TaxProfile {
        w8ben: true,
        pt_regime: PtRegime::Englobamento {
            marginal_rate: 0.285,
        },
    };
    store.set_tax_profile(&a, &second, now).expect("replace");
    assert_eq!(store.tax_profile(&a).expect("read"), Some(second));
    assert_eq!(store.tax_profile(&b).expect("read"), None);
}

#[test]
fn the_profile_has_a_stable_wire_shape() {
    let profile = TaxProfile {
        w8ben: true,
        pt_regime: PtRegime::Englobamento {
            marginal_rate: 0.37,
        },
    };
    let json = serde_json::to_value(profile).expect("json");
    assert_eq!(
        json,
        serde_json::json!({"w8ben": true, "pt_regime": {"kind": "englobamento", "marginal_rate": 0.37}})
    );
    let flat: TaxProfile =
        serde_json::from_value(serde_json::json!({"w8ben": false, "pt_regime": {"kind": "flat"}}))
            .expect("parse");
    assert_eq!(flat.pt_regime, PtRegime::Flat);
}

#[test]
fn the_service_defaults_and_clamps_the_profile() {
    let dir = tempfile::tempdir().expect("temp dir");
    let service = sail_capital::service::CapitalService::open(dir.path()).expect("service");
    let account = AccountId::from_stored("xtb:svc");
    assert_eq!(
        service.tax_profile(&account).expect("default"),
        TaxProfile::default()
    );
    let stored = service
        .set_tax_profile(
            &account,
            TaxProfile {
                w8ben: false,
                pt_regime: PtRegime::Englobamento { marginal_rate: 0.9 },
            },
        )
        .expect("save");
    assert_eq!(
        stored.pt_regime,
        PtRegime::Englobamento {
            marginal_rate: sail_capital::dividends::tax::MAX_MARGINAL
        }
    );
    assert_eq!(service.tax_profile(&account).expect("read"), stored);
}
