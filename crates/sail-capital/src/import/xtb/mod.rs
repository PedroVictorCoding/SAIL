//! XTB official report importer (Antigravity's slice, round 7).
//!
//! Reads official XTB reports exported from the web platform (History → Export):
//! cash operations, closed positions, and open positions, in XLSX or CSV format.
//!
//! Header-driven and tolerant: supports both English and Portuguese exports, extracts
//! preamble metadata (masked account and reporting currency, never owner names),
//! generates idempotent natural keys for cash operations, and tracks all issues cleanly.
//!
//! XTB discontinued its API on 2025-03-14. This module never signs in, never scrapes,
//! and never contacts external networks: it only reads bytes the owner provides.
use std::collections::HashMap;

use crate::contracts::{
    Broker, BrokerSnapshot, Currency, ImportError, ImportIssue, ImportOptions, MAX_ISSUES,
    MAX_REPORT_BYTES, MAX_REPORT_ROWS, ParsedEntry, ParsedReport, Severity, SheetKind,
    SheetSummary,
};

mod cash;
mod datetime;
mod headers;
mod positions;
mod preamble;
mod table;

pub use cash::parse_trade_comment;
pub use headers::{CanonicalColumn, match_header_column};

use cash::{CashRowContext, parse_cash_row};
use datetime::parse_time_text;
use headers::detect_sheet_header;
use positions::{
    detect_open_positions_summary_row, parse_closed_position_row, parse_open_position_row,
};
use preamble::extract_preamble;
use table::{load_csv, load_xlsx};

/// Read an XTB export into a [`ParsedReport`].
///
/// Decides XLSX vs CSV from `file_name` extension or file magic bytes. Rejects files
/// larger than [`MAX_REPORT_BYTES`], limits rows to [`MAX_REPORT_ROWS`], and records
/// up to [`MAX_ISSUES`] before counting dropped issues.
pub fn parse(
    bytes: &[u8],
    file_name: &str,
    options: &ImportOptions,
) -> Result<ParsedReport, ImportError> {
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(ImportError::TooLarge(bytes.len()));
    }
    if bytes.is_empty() {
        return Err(ImportError::Unreadable("file is empty".to_string()));
    }

    let lower_name = file_name.to_ascii_lowercase();
    let is_xlsx = lower_name.ends_with(".xlsx")
        || (bytes.starts_with(b"PK\x03\x04") && !lower_name.ends_with(".csv"));

    let raw_sheets = if is_xlsx {
        load_xlsx(bytes)?
    } else {
        load_csv(bytes)?
    };

    if raw_sheets.is_empty() {
        return Err(ImportError::NoKnownSheet);
    }

    let mut entries: Vec<ParsedEntry> = Vec::new();
    let mut snapshot = BrokerSnapshot::default();
    let mut sheets: Vec<SheetSummary> = Vec::with_capacity(raw_sheets.len());
    let mut issues: Vec<ImportIssue> = Vec::new();
    let mut issues_dropped = 0u32;
    let mut seen_hashes: HashMap<String, u32> = HashMap::new();
    let mut total_data_rows = 0usize;

    let mut account_label: Option<String> = None;
    let mut account_currency: Option<Currency> = None;

    // Helper closure to record an issue respecting MAX_ISSUES
    let record_issue =
        |issue: ImportIssue, issues_vec: &mut Vec<ImportIssue>, dropped_count: &mut u32| {
            if issues_vec.len() < MAX_ISSUES {
                issues_vec.push(issue);
            } else {
                *dropped_count += 1;
            }
        };

    // Phase 1: Pre-scan sheets for preamble metadata and snapshot "as of" date
    for sheet in &raw_sheets {
        if let Some(header) = detect_sheet_header(sheet) {
            let meta = extract_preamble(sheet, header.header_row, options.report_tz);
            if account_label.is_none() && meta.account_label.is_some() {
                account_label = meta.account_label;
            }
            if account_currency.is_none() && meta.account_currency.is_some() {
                account_currency = meta.account_currency;
            }
            if snapshot.as_of.is_none() && meta.as_of.is_some() {
                snapshot.as_of = meta.as_of;
            }
        }

        // Try extracting snapshot as_of date from sheet name, e.g. "OPEN POSITION 24.03.2025"
        if snapshot.as_of.is_none() {
            let sheet_title = sheet.name.as_deref().unwrap_or("");
            for token in sheet_title.split_whitespace() {
                if let Some(pt) = parse_time_text(token, options.report_tz, None) {
                    snapshot.as_of = Some(pt.time.at);
                    break;
                }
            }
        }
    }

    let effective_currency = account_currency
        .clone()
        .or_else(|| options.default_currency.clone());

    let mut known_sheets_count = 0;

    // Phase 2: Process each sheet
    for sheet in &raw_sheets {
        let header_opt = detect_sheet_header(sheet);

        let Some(header) = header_opt else {
            record_issue(
                ImportIssue {
                    severity: Severity::Info,
                    code: "unknown_sheet".to_string(),
                    message: format!(
                        "sheet {:?} was not recognised as a known XTB export and was skipped",
                        sheet.name
                    ),
                    source: None,
                },
                &mut issues,
                &mut issues_dropped,
            );
            sheets.push(SheetSummary {
                name: sheet.name.clone(),
                kind: SheetKind::Unknown,
                rows: 0,
                accepted: 0,
                rejected: 0,
            });
            continue;
        };

        match header.sheet_kind {
            SheetKind::CashOperations => {
                known_sheets_count += 1;
                let mut accepted = 0u32;
                let mut rejected = 0u32;
                let mut rows_count = 0u32;

                let ctx = CashRowContext {
                    headers: &header,
                    sheet_name: sheet.name.as_deref(),
                    account_currency: effective_currency.clone(),
                    default_currency: options.default_currency.clone(),
                    tz: options.report_tz,
                };

                let data_rows = &sheet.rows[(header.header_row as usize)..];
                for row in data_rows {
                    if row.is_all_empty() {
                        continue;
                    }
                    if total_data_rows >= MAX_REPORT_ROWS {
                        record_issue(
                            ImportIssue {
                                severity: Severity::Warning,
                                code: "max_rows_exceeded".to_string(),
                                message: format!(
                                    "reached maximum report rows limit ({MAX_REPORT_ROWS})"
                                ),
                                source: None,
                            },
                            &mut issues,
                            &mut issues_dropped,
                        );
                        break;
                    }
                    total_data_rows += 1;
                    rows_count += 1;

                    let mut row_issues = Vec::new();
                    match parse_cash_row(row, &ctx, &mut seen_hashes, &mut row_issues) {
                        Ok(entry) => {
                            for issue in row_issues {
                                record_issue(issue, &mut issues, &mut issues_dropped);
                            }
                            entries.push(entry);
                            accepted += 1;
                        }
                        Err(issue) => {
                            record_issue(issue, &mut issues, &mut issues_dropped);
                            for extra_issue in row_issues {
                                record_issue(extra_issue, &mut issues, &mut issues_dropped);
                            }
                            rejected += 1;
                        }
                    }
                }

                sheets.push(SheetSummary {
                    name: sheet.name.clone(),
                    kind: SheetKind::CashOperations,
                    rows: rows_count,
                    accepted,
                    rejected,
                });
            }

            SheetKind::OpenPositions => {
                known_sheets_count += 1;
                let mut accepted = 0u32;
                let mut rejected = 0u32;
                let mut rows_count = 0u32;

                let data_rows = &sheet.rows[(header.header_row as usize)..];
                for row in data_rows {
                    if row.is_all_empty() {
                        continue;
                    }

                    // Check for total/balance summary row
                    if let Some((bal, eq)) = detect_open_positions_summary_row(
                        row,
                        effective_currency.as_ref().unwrap_or(&Currency::eur()),
                    ) {
                        if bal.is_some() {
                            snapshot.balance = bal;
                        }
                        if eq.is_some() {
                            snapshot.equity = eq;
                        }
                        continue;
                    }

                    if total_data_rows >= MAX_REPORT_ROWS {
                        record_issue(
                            ImportIssue {
                                severity: Severity::Warning,
                                code: "max_rows_exceeded".to_string(),
                                message: format!(
                                    "reached maximum report rows limit ({MAX_REPORT_ROWS})"
                                ),
                                source: None,
                            },
                            &mut issues,
                            &mut issues_dropped,
                        );
                        break;
                    }
                    total_data_rows += 1;
                    rows_count += 1;

                    let mut row_issues = Vec::new();
                    match parse_open_position_row(
                        row,
                        &header,
                        sheet.name.as_deref(),
                        effective_currency.clone(),
                        options.default_currency.clone(),
                        options.report_tz,
                        &mut row_issues,
                    ) {
                        Ok(pos) => {
                            for issue in row_issues {
                                record_issue(issue, &mut issues, &mut issues_dropped);
                            }
                            snapshot.open_positions.push(pos);
                            accepted += 1;
                        }
                        Err(issue) => {
                            record_issue(issue, &mut issues, &mut issues_dropped);
                            for extra_issue in row_issues {
                                record_issue(extra_issue, &mut issues, &mut issues_dropped);
                            }
                            rejected += 1;
                        }
                    }
                }

                sheets.push(SheetSummary {
                    name: sheet.name.clone(),
                    kind: SheetKind::OpenPositions,
                    rows: rows_count,
                    accepted,
                    rejected,
                });
            }

            SheetKind::ClosedPositions => {
                known_sheets_count += 1;
                let mut accepted = 0u32;
                let mut rejected = 0u32;
                let mut rows_count = 0u32;

                let data_rows = &sheet.rows[(header.header_row as usize)..];
                for row in data_rows {
                    if row.is_all_empty() {
                        continue;
                    }
                    if total_data_rows >= MAX_REPORT_ROWS {
                        record_issue(
                            ImportIssue {
                                severity: Severity::Warning,
                                code: "max_rows_exceeded".to_string(),
                                message: format!(
                                    "reached maximum report rows limit ({MAX_REPORT_ROWS})"
                                ),
                                source: None,
                            },
                            &mut issues,
                            &mut issues_dropped,
                        );
                        break;
                    }
                    total_data_rows += 1;
                    rows_count += 1;

                    let mut row_issues = Vec::new();
                    match parse_closed_position_row(
                        row,
                        &header,
                        sheet.name.as_deref(),
                        effective_currency.clone(),
                        options.default_currency.clone(),
                        options.report_tz,
                        &mut row_issues,
                    ) {
                        Ok(pos) => {
                            for issue in row_issues {
                                record_issue(issue, &mut issues, &mut issues_dropped);
                            }
                            snapshot.closed_positions.push(pos);
                            accepted += 1;
                        }
                        Err(issue) => {
                            record_issue(issue, &mut issues, &mut issues_dropped);
                            for extra_issue in row_issues {
                                record_issue(extra_issue, &mut issues, &mut issues_dropped);
                            }
                            rejected += 1;
                        }
                    }
                }

                sheets.push(SheetSummary {
                    name: sheet.name.clone(),
                    kind: SheetKind::ClosedPositions,
                    rows: rows_count,
                    accepted,
                    rejected,
                });
            }

            SheetKind::Orders => {
                known_sheets_count += 1;
                let rows_count = sheet.rows.len().saturating_sub(header.header_row as usize) as u32;
                sheets.push(SheetSummary {
                    name: sheet.name.clone(),
                    kind: SheetKind::Orders,
                    rows: rows_count,
                    accepted: 0,
                    rejected: 0,
                });
            }

            SheetKind::Unknown => {
                record_issue(
                    ImportIssue {
                        severity: Severity::Info,
                        code: "unknown_sheet".to_string(),
                        message: format!(
                            "sheet {:?} was not recognised and was skipped",
                            sheet.name
                        ),
                        source: None,
                    },
                    &mut issues,
                    &mut issues_dropped,
                );
                sheets.push(SheetSummary {
                    name: sheet.name.clone(),
                    kind: SheetKind::Unknown,
                    rows: 0,
                    accepted: 0,
                    rejected: 0,
                });
            }
        }
    }

    if known_sheets_count == 0 {
        return Err(ImportError::NoKnownSheet);
    }

    Ok(ParsedReport {
        broker: Broker::Xtb,
        account_label,
        account_currency,
        entries,
        snapshot,
        sheets,
        issues,
        issues_dropped,
    })
}
