//! XTB official report importer (Kilo's slice, round 7).
//!
//! Reads the reports the owner exports from XTB's web platform (History → Export): cash
//! operations, closed positions and open positions, as XLSX, or CSV saved from them. It
//! maps columns by header text (English and Portuguese aliases), never by position, and
//! records every row it can't read as an [`ImportIssue`](crate::contracts::ImportIssue).
//!
//! XTB discontinued its API on 2025-03-14. This module never signs in, never scrapes and
//! never contacts XTB: it only reads bytes the owner hands it.
use crate::contracts::{ImportError, ImportOptions, MAX_REPORT_BYTES, ParsedReport};

/// Read an XTB export. `file_name` decides XLSX (`.xlsx`) vs CSV (`.csv`).
///
/// M0 stub: rejects every file. Kilo replaces the body; the signature is the contract.
pub fn parse(
    bytes: &[u8],
    file_name: &str,
    options: &ImportOptions,
) -> Result<ParsedReport, ImportError> {
    let _ = (file_name, options);
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(ImportError::TooLarge(bytes.len()));
    }
    Err(ImportError::Unreadable(
        "the XTB importer is not implemented yet (M0 stub)".to_string(),
    ))
}
