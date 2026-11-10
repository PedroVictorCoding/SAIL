//! From filings to holdings (Antigravity's slice, round 8).
//!
//! These are pure, deterministic functions with no model and no I/O. An instrument maps to
//! an issuer only through an exact ticker match in a recorded map version. A filing becomes
//! a [`WatchItem`] only if it is visible at the [`AsOf`] and the owner held the issuer then.
//! The outcome is watch or research proposal, never an order.

use std::collections::HashMap;

use rust_decimal::{Decimal, RoundingStrategy};

use crate::AsOf;
use crate::contracts::{Currency, Instrument, InstrumentKind};
use crate::evidence::{
    Cik, EvidenceRecord, Exposure, Filing, IssuerMatch, ResearchAction, TickerEntry, TickerIndex,
    WatchItem,
};
use crate::ledger::{Position, Projection};

/// Canonical SEC 8-K item numbers and their descriptive titles.
const SEC_8K_ITEMS: &[(&str, &str)] = &[
    ("1.01", "entry into a material definitive agreement"),
    ("1.02", "termination of a material definitive agreement"),
    ("1.03", "bankruptcy or receivership"),
    ("1.04", "mine safety reporting"),
    ("2.01", "completion of acquisition or disposition of assets"),
    ("2.02", "results of operations"),
    ("2.03", "creation of a direct financial obligation"),
    (
        "2.04",
        "triggering events accelerating direct financial obligation",
    ),
    ("2.05", "costs associated with exit or disposal activities"),
    ("2.06", "material impairments"),
    ("3.01", "notice of delisting"),
    ("3.02", "unregistered sales of equity securities"),
    (
        "3.03",
        "material modification to rights of security holders",
    ),
    ("4.01", "changes in certifying accountant"),
    (
        "4.02",
        "non-reliance on previously issued financial statements",
    ),
    ("5.01", "changes in control of registrant"),
    ("5.02", "departure or election of directors or officers"),
    ("5.03", "amendments to articles of incorporation or bylaws"),
    (
        "5.04",
        "temporary suspension of trading under employee benefit plans",
    ),
    ("5.05", "amendment to registrant's code of ethics"),
    ("5.06", "change in shell company status"),
    (
        "5.07",
        "submission of matters to a vote of security holders",
    ),
    ("5.08", "shareholder director nominations"),
    ("6.01", "asset-backed securities informational material"),
    ("7.01", "regulation fd disclosure"),
    ("8.01", "other events"),
    ("9.01", "financial statements and exhibits"),
];

/// Items on Form 8-K that trigger a research proposal rather than a watch.
const MATERIAL_8K_ITEMS: &[&str] = &[
    "1.01", "1.02", "1.03", "2.01", "2.02", "2.03", "2.04", "2.05", "2.06", "3.01", "4.01", "4.02",
    "5.01", "5.02",
];

/// Links one owned instrument to a SEC issuer.
///
/// Rules:
/// - Only applies to owned instruments. CFDs return `Unresolved`.
/// - The broker symbol must end in `.US` (case-insensitive); otherwise `NotUsListed`.
/// - The ticker is everything before `.US`, uppercased.
/// - If the ticker contains `.`, the primary ticker is tried first. If 0 matches,
///   the alternative ticker with `.` replaced by `-` is tried (e.g. `BRK.B` -> `BRK-B`).
/// - Exactly one match yields `IssuerMatch::Matched`. Zero or multiple matches yield
///   `IssuerMatch::Unresolved`.
pub fn match_issuer(instrument: &Instrument, index: &TickerIndex) -> IssuerMatch {
    if instrument.kind == InstrumentKind::Cfd {
        return IssuerMatch::Unresolved {
            reason: "CFDs are contracts, not shares of the issuer".into(),
        };
    }

    let sym = instrument.symbol.trim();
    if !sym.to_ascii_uppercase().ends_with(".US") {
        return IssuerMatch::NotUsListed;
    }

    let ticker = sym[..sym.len() - 3].to_ascii_uppercase();
    if ticker.is_empty() {
        return IssuerMatch::Unresolved {
            reason: "empty ticker symbol".into(),
        };
    }

    let primary_matches: Vec<&TickerEntry> = index
        .entries
        .iter()
        .filter(|e| e.ticker.eq_ignore_ascii_case(&ticker))
        .collect();

    if primary_matches.len() == 1 {
        let entry = primary_matches[0];
        return IssuerMatch::Matched {
            cik: entry.cik,
            ticker: entry.ticker.clone(),
            title: entry.title.clone(),
            map_fetched_at: index.fetched_at,
        };
    }

    if primary_matches.len() > 1 {
        return IssuerMatch::Unresolved {
            reason: format!(
                "ticker {ticker} is ambiguous: matched {} entries in SEC ticker map",
                primary_matches.len()
            ),
        };
    }

    // primary_matches.is_empty(): try dot-to-dash alternative if ticker has '.'
    if ticker.contains('.') {
        let alt_ticker = ticker.replace('.', "-");
        let alt_matches: Vec<&TickerEntry> = index
            .entries
            .iter()
            .filter(|e| e.ticker.eq_ignore_ascii_case(&alt_ticker))
            .collect();

        if alt_matches.len() == 1 {
            let entry = alt_matches[0];
            return IssuerMatch::Matched {
                cik: entry.cik,
                ticker: entry.ticker.clone(),
                title: entry.title.clone(),
                map_fetched_at: index.fetched_at,
            };
        }

        if alt_matches.len() > 1 {
            return IssuerMatch::Unresolved {
                reason: format!(
                    "ticker {alt_ticker} is ambiguous: matched {} entries in SEC ticker map",
                    alt_matches.len()
                ),
            };
        }
    }

    IssuerMatch::Unresolved {
        reason: format!("ticker {ticker} not found in SEC ticker map"),
    }
}

/// Decides whether a filing matters, and how much. `None` means "don't surface it".
///
/// Rules:
/// - `10-K`, `20-F`, `40-F`: `ResearchProposal`, e.g. `"Annual report (10-K)"`
/// - `10-Q`: `ResearchProposal`, `"Quarterly report (10-Q)"`
/// - `8-K` with material items: `ResearchProposal`, e.g. `"Current report: 2.02 results of operations"`
/// - `8-K` otherwise and `6-K`: `Watch`, e.g. `"Current report (8-K: 7.01, 9.01)"` or `"Current report (6-K)"`
/// - `4`: `Watch`, `"Insider transaction report (Form 4)"`
/// - `SC 13D`/`SC 13G` (and `SCHEDULE 13D`/`13G`, their names since 2024): `Watch`,
///   e.g. `"Beneficial ownership report (SC 13D)"`
/// - `DEF 14A`, `S-1`, `S-3`, `424B*`: `Watch`, `"Proxy statement / registration"`
/// - Any amendment `…/A`: uses base form's action, but never higher than `Watch` for `8-K`.
///   Appends `" (amended)"` to the reason.
pub fn classify(filing: &Filing) -> Option<(ResearchAction, String)> {
    let form_trimmed = filing.form.trim();
    let form_upper = form_trimmed.to_ascii_uppercase();

    let (base, is_amendment) = if let Some(stripped) = form_upper.strip_suffix("/A") {
        (stripped.trim(), true)
    } else {
        (form_upper.as_str(), false)
    };

    let (action, reason) = if base == "10-K" || base == "20-F" || base == "40-F" {
        (
            ResearchAction::ResearchProposal,
            format!("Annual report ({base})"),
        )
    } else if base == "10-Q" {
        (
            ResearchAction::ResearchProposal,
            "Quarterly report (10-Q)".to_string(),
        )
    } else if base == "8-K" {
        let material: Vec<&str> = filing
            .items
            .iter()
            .map(|s| s.trim())
            .filter(|s| MATERIAL_8K_ITEMS.contains(s))
            .collect();

        if !material.is_empty() {
            let parts: Vec<String> = material
                .iter()
                .map(|item| match SEC_8K_ITEMS.iter().find(|(k, _)| k == item) {
                    Some((_, desc)) => format!("{item} {desc}"),
                    None => (*item).to_string(),
                })
                .collect();
            let act = if is_amendment {
                ResearchAction::Watch
            } else {
                ResearchAction::ResearchProposal
            };
            (act, format!("Current report: {}", parts.join(", ")))
        } else {
            let reason = if filing.items.is_empty() {
                "Current report (8-K)".to_string()
            } else {
                let items_str = filing
                    .items
                    .iter()
                    .map(|s| s.trim())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Current report (8-K: {items_str})")
            };
            (ResearchAction::Watch, reason)
        }
    } else if base == "6-K" {
        let reason = if filing.items.is_empty() {
            "Current report (6-K)".to_string()
        } else {
            let items_str = filing
                .items
                .iter()
                .map(|s| s.trim())
                .collect::<Vec<_>>()
                .join(", ");
            format!("Current report (6-K: {items_str})")
        };
        (ResearchAction::Watch, reason)
    } else if base == "4" {
        (
            ResearchAction::Watch,
            "Insider transaction report (Form 4)".to_string(),
        )
    } else if matches!(
        base,
        "SC 13D" | "SC 13G" | "13D" | "13G" | "SCHEDULE 13D" | "SCHEDULE 13G"
    ) {
        (
            ResearchAction::Watch,
            format!("Beneficial ownership report ({base})"),
        )
    } else if base == "DEF 14A" || base == "S-1" || base == "S-3" || base.starts_with("424B") {
        (
            ResearchAction::Watch,
            "Proxy statement / registration".to_string(),
        )
    } else {
        return None;
    };

    let action = if is_amendment && base == "8-K" {
        ResearchAction::Watch
    } else {
        action
    };

    let reason = if is_amendment {
        format!("{reason} (amended)")
    } else {
        reason
    };

    Some((action, reason))
}

/// Computes the share of owned value for all owned positions in `projection.positions`.
///
/// Returns `Some(map)` only when every owned position has a `broker_market_value`,
/// all in the same currency, and the total value is strictly above zero.
fn compute_owned_value_shares(positions: &[Position]) -> Option<HashMap<String, Decimal>> {
    if positions.is_empty() {
        return None;
    }

    let mut total = Decimal::ZERO;
    let mut common_currency: Option<Currency> = None;

    for pos in positions {
        let mv = pos.broker_market_value.as_ref()?;
        match &common_currency {
            None => common_currency = Some(mv.currency.clone()),
            Some(c) if c == &mv.currency => {}
            _ => return None, // mixed currency
        }
        total += mv.amount;
    }

    if total <= Decimal::ZERO {
        return None;
    }

    let mut shares = HashMap::new();
    for pos in positions {
        let mv = pos.broker_market_value.as_ref()?;
        let share =
            (mv.amount / total).round_dp_with_strategy(4, RoundingStrategy::MidpointNearestEven);
        shares.insert(pos.instrument.key(), share);
    }

    Some(shares)
}

/// Turns visible evidence into watch items for the positions in `projection`.
///
/// Rules:
/// - `matches` contains `(instrument_key, IssuerMatch)` for every owned position.
/// - Visibility: `first_observed_at <= as_of.known_by && published_at <= as_of.occurred_by && ingested_at <= as_of.known_by`.
/// - Revisions: for identical `natural_key`, the latest visible revision (greatest `ingested_at`) wins.
/// - Producing items: for each visible filing whose CIK matches an owned position, produce one `WatchItem`
///   per position if `classify` returns `Some`.
/// - `share_of_owned_value` is set only when all owned positions have market values in the same currency.
/// - Ordering: sorted by `published_at` desc, then accession asc, then instrument key asc. Deterministic.
pub fn watch_items(
    projection: &Projection,
    matches: &[(String, IssuerMatch)],
    evidence: &[EvidenceRecord],
    as_of: AsOf,
) -> Vec<WatchItem> {
    // 1. Calculate share of owned value across all owned positions.
    let shares = compute_owned_value_shares(&projection.positions);

    // 2. Identify owned positions matched to a CIK.
    let mut matched_positions: Vec<(&Position, Cik)> = Vec::new();
    for pos in &projection.positions {
        let key = pos.instrument.key();
        if let Some((_, IssuerMatch::Matched { cik, .. })) = matches.iter().find(|(k, _)| k == &key)
        {
            matched_positions.push((pos, *cik));
        }
    }

    if matched_positions.is_empty() {
        return Vec::new();
    }

    // 3. Filter visible records and select the latest revision per natural key.
    let mut latest_revisions: HashMap<&str, &EvidenceRecord> = HashMap::new();
    for record in evidence {
        if record.first_observed_at <= as_of.known_by
            && record.published_at <= as_of.occurred_by
            && record.ingested_at <= as_of.known_by
        {
            match latest_revisions.get(record.natural_key.as_str()) {
                Some(existing) => {
                    if record.ingested_at > existing.ingested_at
                        || (record.ingested_at == existing.ingested_at && record.id > existing.id)
                    {
                        latest_revisions.insert(&record.natural_key, record);
                    }
                }
                None => {
                    latest_revisions.insert(&record.natural_key, record);
                }
            }
        }
    }

    // 4. Match filings to owned positions and classify.
    let mut items = Vec::new();
    for record in latest_revisions.values() {
        let classification = classify(&record.filing);
        if let Some((action, reason)) = classification {
            for (pos, cik) in &matched_positions {
                if record.filing.cik == *cik {
                    let exposure = Exposure {
                        instrument_key: pos.instrument.key(),
                        quantity: pos.quantity,
                        cost_basis: pos.cost_basis.clone(),
                        broker_market_value: pos.broker_market_value.clone(),
                        share_of_owned_value: shares
                            .as_ref()
                            .and_then(|s| s.get(&pos.instrument.key()).copied()),
                    };

                    items.push(WatchItem {
                        evidence_id: record.id.clone(),
                        filing: record.filing.clone(),
                        issuer_name: record.issuer_name.clone(),
                        exposure,
                        action,
                        reason: reason.clone(),
                        published_at: record.published_at,
                        first_observed_at: record.first_observed_at,
                    });
                }
            }
        }
    }

    // 5. Deterministic sorting: published_at desc, accession asc, instrument_key asc.
    items.sort_by(|a, b| {
        b.published_at
            .cmp(&a.published_at)
            .then_with(|| a.filing.accession.cmp(&b.filing.accession))
            .then_with(|| a.exposure.instrument_key.cmp(&b.exposure.instrument_key))
    });

    items
}
