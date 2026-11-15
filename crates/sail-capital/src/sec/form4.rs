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
use roxmltree::{Document, Node};
use serde::{Deserialize, Serialize};

use crate::evidence::{Cik, Filing, MAX_SEC_RESPONSE_BYTES, MAX_SEC_TEXT_CHARS, SourceError};

/// Most transactions read from one filing (fixed a priori).
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

/// Parses a Form 4 XML `body` for `filing`.
pub fn parse(body: &[u8], filing: &Filing) -> Result<Form4, SourceError> {
    if body.len() > MAX_SEC_RESPONSE_BYTES {
        return Err(SourceError::TooLarge);
    }
    let text = std::str::from_utf8(body)
        .map_err(|e| SourceError::Malformed(format!("form4 is not utf-8: {e}")))?;
    let doc = Document::parse(text)
        .map_err(|e| SourceError::Malformed(format!("form4 is not valid XML: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "ownershipDocument" {
        return Err(SourceError::Malformed(
            "form4 root is not ownershipDocument".to_string(),
        ));
    }

    let issuer = child_named(root, "issuer")
        .ok_or_else(|| SourceError::Malformed("form4 is missing issuer".to_string()))?;
    let issuer_cik_str = child_text(issuer, "issuerCik")
        .ok_or_else(|| SourceError::Malformed("form4 is missing issuerCik".to_string()))?;
    let issuer_cik_num: u64 = issuer_cik_str
        .parse()
        .map_err(|_| SourceError::Malformed("form4 issuerCik is not numeric".to_string()))?;
    let issuer_ticker = child_text(issuer, "issuerTradingSymbol").map(cap_text);

    let owners = root
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "reportingOwner")
        .filter_map(parse_owner)
        .collect();

    let mut transactions = Vec::new();
    if let Some(table) = child_named(root, "nonDerivativeTable") {
        for tx_node in table
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "nonDerivativeTransaction")
        {
            if transactions.len() >= MAX_TRANSACTIONS {
                break;
            }
            if let Some(tx) = parse_transaction(tx_node) {
                transactions.push(tx);
            }
        }
    }

    Ok(Form4 {
        accession: filing.accession.clone(),
        issuer_cik: Cik(issuer_cik_num),
        issuer_ticker,
        published_at: filing.published_at,
        owners,
        transactions,
        amendment: filing.form.trim().eq_ignore_ascii_case("4/A"),
    })
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

fn parse_owner(node: Node<'_, '_>) -> Option<ReportingOwner> {
    let id = child_named(node, "reportingOwnerId")?;
    let cik = child_text(id, "rptOwnerCik")
        .map(cap_text)
        .unwrap_or_default();
    let name = child_text(id, "rptOwnerName")
        .map(cap_text)
        .unwrap_or_default();
    if cik.is_empty() && name.is_empty() {
        return None;
    }
    let rel = child_named(node, "reportingOwnerRelationship");
    let is_director = rel
        .and_then(|r| child_text(r, "isDirector"))
        .is_some_and(is_xml_true);
    let is_officer = rel
        .and_then(|r| child_text(r, "isOfficer"))
        .is_some_and(is_xml_true);
    let is_ten_pct_owner = rel
        .and_then(|r| child_text(r, "isTenPercentOwner"))
        .is_some_and(is_xml_true);
    let officer_title = rel
        .and_then(|r| child_text(r, "officerTitle"))
        .map(cap_text);

    Some(ReportingOwner {
        cik,
        name,
        is_director,
        is_officer,
        is_ten_pct_owner,
        officer_title,
    })
}

fn parse_transaction(node: Node<'_, '_>) -> Option<InsiderTransaction> {
    let date_str = child_named(node, "transactionDate").and_then(|d| child_text(d, "value"))?;
    let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()?;

    let coding = child_named(node, "transactionCoding")?;
    let code = child_text(coding, "transactionCode")?.to_ascii_uppercase();

    let amounts = child_named(node, "transactionAmounts")?;
    let shares_str =
        child_named(amounts, "transactionShares").and_then(|s| child_text(s, "value"))?;
    let shares: f64 = shares_str.parse().ok()?;
    if !shares.is_finite() || shares < 0.0 {
        return None;
    }

    let price = child_named(amounts, "transactionPricePerShare")
        .and_then(|p| child_text(p, "value"))
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|p| p.is_finite() && *p >= 0.0);

    let ad_str = child_named(amounts, "transactionAcquiredDisposedCode")
        .and_then(|a| child_text(a, "value"))?;
    let acquired = match ad_str.to_ascii_uppercase().as_str() {
        "A" => true,
        "D" => false,
        _ => return None,
    };

    Some(InsiderTransaction {
        date,
        code,
        shares,
        price,
        acquired,
    })
}

fn child_named<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
}

fn child_text<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    let child = child_named(node, name)?;
    let text = child.text()?.trim();
    if text.is_empty() { None } else { Some(text) }
}

fn is_xml_true(s: &str) -> bool {
    s == "1" || s.eq_ignore_ascii_case("true")
}

fn cap_text(s: &str) -> String {
    s.chars().take(MAX_SEC_TEXT_CHARS).collect()
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
