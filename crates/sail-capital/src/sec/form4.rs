//! Round 25 (Antigravity's slice): SEC Form 4, the insider transaction report.
//!
//! **What is read.** The filing's raw XML (`ownershipDocument`), fetched by
//! [`crate::evidence::EdgarApi::form4`]:
//! - `issuer/issuerCik` and `issuerTradingSymbol`;
//! - each `reportingOwner`: `rptOwnerCik`, `rptOwnerName`, and from
//!   `reportingOwnerRelationship` the flags `isDirector`, `isOfficer`,
//!   `isTenPercentOwner` (`1`/`true`, case-insensitive) and `officerTitle`;
//! - each `nonDerivativeTable/nonDerivativeTransaction`: `transactionDate/value`,
//!   `transactionCoding/transactionCode`, `transactionAmounts/transactionShares/value`,
//!   `transactionPricePerShare/value` (may be missing or a footnote only) and
//!   `transactionAcquiredDisposedCode/value` (`A`/`D`).
//!
//! Derivative transactions (options, RSUs) are skipped. With several reporting owners
//! (a joint filing) each transaction is attributed to every owner, and the signal
//! counts distinct owner CIKs, so a joint filing still counts once per person.
//!
//! **Hostile input.** At most [`crate::evidence::MAX_SEC_RESPONSE_BYTES`] are read
//! (the fetch caps it too); no DTDs or entities are expanded; at most
//! [`MAX_TRANSACTIONS`] transactions per filing; names capped at
//! [`crate::evidence::MAX_SEC_TEXT_CHARS`]; non-finite or negative shares and prices
//! are dropped. A document that isn't an `ownershipDocument` is
//! [`SourceError::Malformed`]. Parse with `roxmltree` (added to this crate at M0; its
//! default options reject DTDs, so no entity is ever expanded).
//!
//! **Purity.** No I/O and no clock.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::evidence::{Cik, Filing, SourceError};

/// Most transactions read from one filing.
pub const MAX_TRANSACTIONS: usize = 200;

/// Who reported, and their role at the issuer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportingOwner {
    /// The owner's CIK as written (digits).
    pub cik: String,
    /// Name, capped.
    pub name: String,
    /// A director.
    pub is_director: bool,
    /// An officer.
    pub is_officer: bool,
    /// A 10% owner.
    pub is_ten_pct_owner: bool,
    /// Officer title, if given, capped.
    pub officer_title: Option<String>,
}

/// One non-derivative transaction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InsiderTransaction {
    /// Transaction date.
    pub date: NaiveDate,
    /// SEC transaction code: `P` open-market purchase, `S` open-market sale, `A` grant,
    /// `M` option exercise, `F` tax withholding, `G` gift, …
    pub code: String,
    /// Shares.
    pub shares: f64,
    /// Price per share, when stated.
    pub price: Option<f64>,
    /// `true` for acquired (`A`), `false` for disposed (`D`).
    pub acquired: bool,
}

impl InsiderTransaction {
    /// An open-market purchase (`P`, acquired).
    pub fn is_open_market_buy(&self) -> bool {
        self.code == "P" && self.acquired
    }

    /// An open-market sale (`S`, disposed).
    pub fn is_open_market_sell(&self) -> bool {
        self.code == "S" && !self.acquired
    }
}

/// One parsed Form 4.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Form4 {
    /// Accession number, from the filing.
    pub accession: String,
    /// The issuer, from the document (should equal the filing's CIK).
    pub issuer_cik: Cik,
    /// The issuer's trading symbol, if given.
    pub issuer_ticker: Option<String>,
    /// When SEC made it public (from the filing), the time the signal may use it.
    pub published_at: DateTime<Utc>,
    /// The reporting owners.
    pub owners: Vec<ReportingOwner>,
    /// Non-derivative transactions, at most [`MAX_TRANSACTIONS`].
    pub transactions: Vec<InsiderTransaction>,
    /// `true` for `4/A`.
    pub amendment: bool,
}

/// Parses a Form 4 XML `body` for `filing`. M0 stub: always
/// [`SourceError::NotConfigured`].
pub fn parse(body: &[u8], filing: &Filing) -> Result<Form4, SourceError> {
    let _ = (body, filing);
    Err(SourceError::NotConfigured)
}

/// The raw XML document name from a submissions `primaryDocument`: SEC lists the
/// rendered `xslF345X05/form4.xml`; the raw file is the same name without the
/// `xsl…/` directory.
pub fn raw_document(primary_document: &str) -> &str {
    match primary_document.split_once('/') {
        Some((dir, rest)) if dir.starts_with("xsl") => rest,
        _ => primary_document,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_document_strips_the_stylesheet_directory() {
        assert_eq!(raw_document("xslF345X05/wf-form4_1.xml"), "wf-form4_1.xml");
        assert_eq!(raw_document("form4.xml"), "form4.xml");
    }
}
