//! Deterministic replay input manifests, provenance aggregation and canonical hashing.
//!
//! Every point-in-time replay produces a [`ReplayManifest`] recording the exact decision
//! timestamp/session, sorted instrument keys, canonical bar revision hashes, membership
//! snapshot hashes, corporate-action versions, model-output hashes, policy/cost version,
//! code/model version, and per-source [`InputProvenance`]. Neither wall-clock time nor
//! SQLite rowids ever enter the hash.

use crate::contracts::{DecisionTime, InputProvenance, sha256_hex};
use crate::dividends::Payment;
use crate::market::MarketObservation;
use crate::outlook::ExternalPath;
use crate::signals::SignalId;
use crate::universe::ResearchMember;
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Schema version of [`ReplayManifest`].
pub const MANIFEST_SCHEMA_VERSION: &str = "capital-replay-manifest/1";

/// Code/model version tag included in replay manifests.
pub const REPLAY_CODE_VERSION: &str = "capital-replay/2";

/// Per-instrument price bar input summary in a [`ReplayManifest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BarInputManifest {
    /// Ledger or watchlist instrument key.
    pub instrument_key: String,
    /// Display/broker symbol.
    pub symbol: String,
    /// Provenance across the bars used (`Observed` only if all bars were observed by `decision.at`).
    pub provenance: InputProvenance,
    /// Number of bars included.
    pub bar_count: u32,
    /// Earliest bar session included.
    pub first_session: Option<NaiveDate>,
    /// Latest bar session included.
    pub last_session: Option<NaiveDate>,
    /// SHA-256 of canonicalized bar observations (ordered by session).
    pub bars_sha256: String,
}

/// Universe or index-membership input summary in a [`ReplayManifest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipInputManifest {
    /// Source identifier (e.g. `"portfolio"`, `"watchlist"`, `"research:sp500"`).
    pub source: String,
    /// Provenance of this membership input.
    pub provenance: InputProvenance,
    /// Snapshot timestamp if snapshot-backed (e.g. `fetched_at` or `decision.at`).
    pub snapshot_at: Option<DateTime<Utc>>,
    /// SHA-256 of canonicalized sorted member records and `(from, to)` intervals.
    pub snapshot_sha256: String,
}

/// Corporate-action / dividend / earnings input summary in a [`ReplayManifest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorporateActionInputManifest {
    /// Instrument or Yahoo symbol key.
    pub key: String,
    /// Kind of corporate-action / event series (e.g. `"dividends"`, `"earnings"`).
    pub kind: String,
    /// Provenance of this series.
    pub provenance: InputProvenance,
    /// Snapshot timestamp when fetched.
    pub fetched_at: Option<DateTime<Utc>>,
    /// Canonical SHA-256 of the corporate-action version.
    pub version_sha256: String,
}

/// External model output input summary in a [`ReplayManifest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOutputInputManifest {
    /// Signal or outlook component name.
    pub component_or_signal: String,
    /// Model version string if available.
    pub model_version: Option<String>,
    /// Provenance of the model output.
    pub provenance: InputProvenance,
    /// Number of outputs included.
    pub count: u32,
    /// SHA-256 of canonicalized sorted outputs.
    pub outputs_sha256: String,
}

/// Deterministic input manifest and hash for a point-in-time replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayManifest {
    /// [`MANIFEST_SCHEMA_VERSION`].
    pub schema_version: String,
    /// Explicit decision timestamp and session.
    pub decision: DecisionTime,
    /// Exchange calendar identifier used for session labels and origin selection.
    pub calendar_id: String,
    /// Sorted instrument keys in scope.
    pub instrument_keys: Vec<String>,
    /// Per-instrument bar manifests, sorted by `instrument_key`.
    pub bars: Vec<BarInputManifest>,
    /// Membership manifests, sorted by `source`.
    pub membership: Vec<MembershipInputManifest>,
    /// Corporate-action manifests, sorted by `(key, kind)`.
    pub corporate_actions: Vec<CorporateActionInputManifest>,
    /// External model output manifests, sorted by `component_or_signal`.
    pub model_outputs: Vec<ModelOutputInputManifest>,
    /// Policy / cost version tag.
    pub cost_policy_version: String,
    /// Code / model version tag.
    pub code_model_version: String,
    /// Aggregate provenance (`Reconstructed` if any present input was reconstructed;
    /// `Unavailable` if no observed or reconstructed inputs were present).
    pub provenance: InputProvenance,
    /// Deterministic SHA-256 hex digest over the canonicalized manifest fields above.
    pub input_hash: String,
}

/// Output of a point-in-time replay paired with its deterministic [`ReplayManifest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayOutput<T> {
    /// The computed replay value (outlook, forecasts, signals, evaluation, or track record).
    pub value: T,
    /// Deterministic input manifest and hash.
    pub manifest: ReplayManifest,
}

impl ReplayManifest {
    /// Creates an unfinalized manifest with the given decision, calendar, and version tags.
    pub fn new(
        decision: DecisionTime,
        calendar_id: impl Into<String>,
        cost_policy_version: impl Into<String>,
        code_model_version: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION.to_string(),
            decision,
            calendar_id: calendar_id.into(),
            instrument_keys: Vec::new(),
            bars: Vec::new(),
            membership: Vec::new(),
            corporate_actions: Vec::new(),
            model_outputs: Vec::new(),
            cost_policy_version: cost_policy_version.into(),
            code_model_version: code_model_version.into(),
            provenance: InputProvenance::Unavailable,
            input_hash: String::new(),
        }
    }

    /// Canonicalizes ordering, computes aggregate provenance, and computes [`Self::input_hash`].
    pub fn finalize(mut self) -> Self {
        self.instrument_keys.sort();
        self.instrument_keys.dedup();
        self.bars
            .sort_by(|a, b| a.instrument_key.cmp(&b.instrument_key));
        self.membership.sort_by(|a, b| a.source.cmp(&b.source));
        self.corporate_actions
            .sort_by(|a, b| (&a.key, &a.kind).cmp(&(&b.key, &b.kind)));
        self.model_outputs
            .sort_by(|a, b| a.component_or_signal.cmp(&b.component_or_signal));

        self.provenance = combine_provenance(
            self.bars
                .iter()
                .map(|b| b.provenance)
                .chain(self.membership.iter().map(|m| m.provenance))
                .chain(self.corporate_actions.iter().map(|c| c.provenance))
                .chain(self.model_outputs.iter().map(|m| m.provenance)),
        );

        self.input_hash = self.compute_hash();
        self
    }

    fn compute_hash(&self) -> String {
        let mut buf = String::new();
        buf.push_str(&self.schema_version);
        buf.push('\n');
        buf.push_str(&canonical_ts(self.decision.at));
        buf.push('|');
        buf.push_str(&self.decision.session.format("%Y-%m-%d").to_string());
        buf.push('|');
        buf.push_str(&self.calendar_id);
        buf.push('\n');
        buf.push_str(&self.cost_policy_version);
        buf.push('|');
        buf.push_str(&self.code_model_version);
        buf.push('\n');
        buf.push_str(provenance_tag(self.provenance));
        buf.push('\n');

        for key in &self.instrument_keys {
            buf.push_str("K:");
            buf.push_str(key);
            buf.push('\n');
        }
        for bar in &self.bars {
            buf.push_str("B:");
            buf.push_str(&bar.instrument_key);
            buf.push('|');
            buf.push_str(&bar.symbol);
            buf.push('|');
            buf.push_str(provenance_tag(bar.provenance));
            buf.push('|');
            buf.push_str(&bar.bar_count.to_string());
            buf.push('|');
            buf.push_str(&opt_date(bar.first_session));
            buf.push('|');
            buf.push_str(&opt_date(bar.last_session));
            buf.push('|');
            buf.push_str(&bar.bars_sha256);
            buf.push('\n');
        }
        for mem in &self.membership {
            buf.push_str("M:");
            buf.push_str(&mem.source);
            buf.push('|');
            buf.push_str(provenance_tag(mem.provenance));
            buf.push('|');
            buf.push_str(&opt_ts(mem.snapshot_at));
            buf.push('|');
            buf.push_str(&mem.snapshot_sha256);
            buf.push('\n');
        }
        for ca in &self.corporate_actions {
            buf.push_str("C:");
            buf.push_str(&ca.key);
            buf.push('|');
            buf.push_str(&ca.kind);
            buf.push('|');
            buf.push_str(provenance_tag(ca.provenance));
            buf.push('|');
            buf.push_str(&opt_ts(ca.fetched_at));
            buf.push('|');
            buf.push_str(&ca.version_sha256);
            buf.push('\n');
        }
        for mo in &self.model_outputs {
            buf.push_str("O:");
            buf.push_str(&mo.component_or_signal);
            buf.push('|');
            buf.push_str(mo.model_version.as_deref().unwrap_or(""));
            buf.push('|');
            buf.push_str(provenance_tag(mo.provenance));
            buf.push('|');
            buf.push_str(&mo.count.to_string());
            buf.push('|');
            buf.push_str(&mo.outputs_sha256);
            buf.push('\n');
        }
        sha256_hex(buf.as_bytes())
    }
}

/// Combines multiple input provenances into an aggregate provenance.
///
/// Any `Reconstructed` input makes the aggregate `Reconstructed`. `Observed` requires at
/// least one `Observed` input and zero `Reconstructed` inputs.
pub fn combine_provenance(iter: impl IntoIterator<Item = InputProvenance>) -> InputProvenance {
    let mut saw_observed = false;
    for p in iter {
        match p {
            InputProvenance::Reconstructed => return InputProvenance::Reconstructed,
            InputProvenance::Observed => saw_observed = true,
            InputProvenance::Unavailable => {}
        }
    }
    if saw_observed {
        InputProvenance::Observed
    } else {
        InputProvenance::Unavailable
    }
}

/// Canonical UTC timestamp formatting (microsecond precision, `Z` suffix).
pub fn canonical_ts(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// Canonical `Decimal` formatting (trailing zeros stripped).
pub fn canonical_decimal(d: Decimal) -> String {
    d.normalize().to_string()
}

/// Canonical `f64` bit representation (`-0.0` normalized to `+0.0`).
pub fn canonical_f64(v: f64) -> String {
    if v == 0.0 {
        format!("{:016x}", 0.0_f64.to_bits())
    } else if v.is_nan() {
        "nan".to_string()
    } else {
        format!("{:016x}", v.to_bits())
    }
}

fn provenance_tag(p: InputProvenance) -> &'static str {
    match p {
        InputProvenance::Observed => "observed",
        InputProvenance::Reconstructed => "reconstructed",
        InputProvenance::Unavailable => "unavailable",
    }
}

fn opt_date(d: Option<NaiveDate>) -> String {
    d.map_or_else(String::new, |v| v.format("%Y-%m-%d").to_string())
}

fn opt_ts(ts: Option<DateTime<Utc>>) -> String {
    ts.map_or_else(String::new, canonical_ts)
}

/// Computes a canonical SHA-256 over bar revision tuples `(id, bar_sha256, observation, provenance)`.
pub fn hash_bar_revisions(
    revisions: &[(String, String, MarketObservation, InputProvenance)],
) -> String {
    let mut sorted: Vec<_> = revisions.iter().collect();
    sorted.sort_by_key(|a| a.2.session);
    let mut buf = String::new();
    for (id, bar_sha, obs, prov) in sorted {
        buf.push_str(id);
        buf.push('|');
        buf.push_str(bar_sha);
        buf.push('|');
        buf.push_str(&obs.session.format("%Y-%m-%d").to_string());
        buf.push('|');
        buf.push_str(&obs.source_id);
        buf.push('|');
        buf.push_str(&canonical_decimal(obs.close));
        buf.push('|');
        if let Some(ohl) = &obs.ohl {
            buf.push_str(&canonical_decimal(ohl.open));
            buf.push(',');
            buf.push_str(&canonical_decimal(ohl.high));
            buf.push(',');
            buf.push_str(&canonical_decimal(ohl.low));
        }
        buf.push('|');
        buf.push_str(&canonical_ts(obs.first_available_at));
        buf.push('|');
        buf.push_str(provenance_tag(*prov));
        buf.push('\n');
    }
    sha256_hex(buf.as_bytes())
}

/// Computes a canonical SHA-256 over sorted `(key, symbol)` universe pairs.
pub fn hash_universe_pairs(pairs: &[(String, String)]) -> String {
    let mut sorted = pairs.to_vec();
    sorted.sort();
    let mut buf = String::new();
    for (key, symbol) in sorted {
        buf.push_str(&key);
        buf.push('|');
        buf.push_str(&symbol);
        buf.push('\n');
    }
    sha256_hex(buf.as_bytes())
}

/// Computes a canonical SHA-256 over [`ResearchMember`]s and their `(from, to)` intervals.
pub fn hash_research_members(members: &[ResearchMember]) -> String {
    let mut sorted = members.to_vec();
    sorted.sort_by(|a, b| a.yahoo.cmp(&b.yahoo));
    let mut buf = String::new();
    for m in sorted {
        buf.push_str(&m.yahoo);
        buf.push('|');
        buf.push_str(&format!("{:?}", m.class));
        let mut intervals = m.membership;
        intervals.sort();
        for (from, to) in intervals {
            buf.push('|');
            buf.push_str(&from.format("%Y-%m-%d").to_string());
            buf.push_str("..");
            buf.push_str(&opt_date(to));
        }
        buf.push('\n');
    }
    sha256_hex(buf.as_bytes())
}

/// Computes a canonical SHA-256 over dividend [`Payment`]s.
pub fn hash_dividend_payments(currency: Option<&str>, payments: &[Payment]) -> String {
    let mut sorted = payments.to_vec();
    sorted.sort_by_key(|a| a.ex_date);
    let mut buf = String::new();
    buf.push_str(currency.unwrap_or(""));
    buf.push('\n');
    for p in sorted {
        buf.push_str(&p.ex_date.format("%Y-%m-%d").to_string());
        buf.push('|');
        buf.push_str(&canonical_f64(p.per_share));
        buf.push('\n');
    }
    sha256_hex(buf.as_bytes())
}

/// Computes a canonical SHA-256 over earnings dates.
pub fn hash_earnings_dates(dates: &[NaiveDate]) -> String {
    let mut sorted = dates.to_vec();
    sorted.sort();
    let mut buf = String::new();
    for d in sorted {
        buf.push_str(&d.format("%Y-%m-%d").to_string());
        buf.push('\n');
    }
    sha256_hex(buf.as_bytes())
}

/// Computes a canonical SHA-256 over signal values `(instrument_key, session, value)`.
pub fn hash_signal_values(signal: SignalId, values: &[(String, NaiveDate, f64)]) -> String {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    let mut buf = String::new();
    buf.push_str(&format!("{signal:?}"));
    buf.push('\n');
    for (key, session, value) in sorted {
        buf.push_str(&key);
        buf.push('|');
        buf.push_str(&session.format("%Y-%m-%d").to_string());
        buf.push('|');
        buf.push_str(&canonical_f64(value));
        buf.push('\n');
    }
    sha256_hex(buf.as_bytes())
}

/// Computes a canonical SHA-256 over [`ExternalPath`]s.
pub fn hash_external_paths(paths: &[ExternalPath]) -> String {
    let mut sorted: Vec<_> = paths.iter().collect();
    sorted.sort_by(|a, b| {
        (&a.instrument_key, a.origin, a.horizon).cmp(&(&b.instrument_key, b.origin, b.horizon))
    });
    let mut buf = String::new();
    for p in sorted {
        buf.push_str(&format!("{:?}", p.component));
        buf.push('|');
        buf.push_str(&p.instrument_key);
        buf.push('|');
        buf.push_str(&p.origin.format("%Y-%m-%d").to_string());
        buf.push('|');
        buf.push_str(&p.horizon.to_string());
        for (step, vals) in p.path.steps.iter().zip(&p.path.values) {
            buf.push('|');
            buf.push_str(&step.to_string());
            for v in vals {
                buf.push(':');
                buf.push_str(&canonical_f64(*v));
            }
        }
        buf.push('\n');
    }
    sha256_hex(buf.as_bytes())
}
