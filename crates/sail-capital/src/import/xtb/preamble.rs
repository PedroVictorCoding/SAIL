//! Preamble metadata extraction for XTB reports.
//!
//! Extracts account number and reporting currency from preamble rows located above the
//! main table header. Never extracts or stores the owner's name.
use crate::contracts::Currency;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use super::datetime::parse_cell_time;
use super::table::{RawCell, RawRow, RawSheet};

/// Metadata collected from preamble rows before the table header.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreambleMetadata {
    /// Account label/number from the file, e.g. `"51234567"`.
    pub account_label: Option<String>,
    /// Account reporting currency, e.g. `"EUR"`.
    pub account_currency: Option<Currency>,
    /// Snapshot or "as of" time if stated in the preamble.
    pub as_of: Option<DateTime<Utc>>,
}

/// Normalise label key text for preamble inspection.
fn normalise_key(text: &str) -> String {
    let lower = text.trim().to_lowercase();
    let mut out = String::with_capacity(lower.len());
    for c in lower.chars() {
        match c {
            'á' | 'à' | 'ã' | 'â' => out.push('a'),
            'é' | 'è' | 'ê' => out.push('e'),
            'í' | 'ì' => out.push('i'),
            'ó' | 'ò' | 'õ' | 'ô' => out.push('o'),
            'ú' | 'ù' => out.push('u'),
            'ç' => out.push('c'),
            ':' | '-' | '_' | '.' => out.push(' '),
            _ => out.push(c),
        }
    }
    while let (Some(open), Some(close)) = (out.find('('), out.find(')')) {
        if close < open {
            break;
        }
        out.replace_range(open..=close, " ");
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract preamble metadata from rows before `header_row` (1-based).
///
/// Values may be text or numbers: XTB writes the account number and dates as numeric
/// cells. Besides `label: value` pairs, the open-positions summary block (`Product |
/// Metric | Amount | Currency`, rows such as `My Trades | Open position value | … | EUR`)
/// states the account currency.
pub fn extract_preamble(sheet: &RawSheet, header_row: u32, tz: Tz) -> PreambleMetadata {
    let mut meta = PreambleMetadata::default();
    let limit = (header_row.saturating_sub(1) as usize).min(sheet.rows.len());

    for row in &sheet.rows[..limit] {
        if row.is_all_empty() {
            continue;
        }

        if meta.account_currency.is_none() && row_is_value_summary(row) {
            meta.account_currency = row
                .cells
                .iter()
                .find_map(|c| Currency::parse(c.as_str().trim()));
        }

        for (col_idx, cell) in row.cells.iter().enumerate() {
            let text = cell.as_str().trim();
            if text.is_empty() {
                continue;
            }

            // A `label: value` pair in one cell.
            if let Some((lbl, val)) = text.split_once(':') {
                let v = val.trim();
                if !v.is_empty() {
                    process_key_value(lbl.trim(), &RawCell::Text(v.to_string()), tz, &mut meta);
                    continue;
                }
            }

            // A label in this cell and its value in the next one.
            let label = text.trim_end_matches(':');
            if is_known_preamble_label(&normalise_key(label))
                && let Some(next) = row.cells.get(col_idx + 1).filter(|c| !c.is_empty())
            {
                process_key_value(label, next, tz, &mut meta);
            }
        }
    }

    meta
}

/// True for a summary row such as `My Trades | Open position value | 1234.5 | EUR`.
fn row_is_value_summary(row: &RawRow) -> bool {
    row.cells.iter().any(|c| {
        let norm = normalise_key(c.as_str());
        norm.starts_with("open position") || norm.starts_with("valor das posicoes")
    })
}

/// Returns true if this string looks like a preamble label.
fn is_known_preamble_label(norm: &str) -> bool {
    matches!(
        norm,
        "account"
            | "account number"
            | "conta"
            | "conta no"
            | "conta n"
            | "n da conta"
            | "numero da conta"
            | "currency"
            | "moeda"
            | "date to"
            | "data ate"
            | "ate"
            | "as of"
            | "data as of report generated"
            | "saldo"
            | "balance"
    )
}

/// Process a single key-value pair found in the preamble.
fn process_key_value(key: &str, val: &RawCell, tz: Tz, meta: &mut PreambleMetadata) {
    let norm = normalise_key(key);
    let text = val.to_clean_text().0;
    match norm.as_str() {
        // Explicitly IGNORE owner name rows.
        "name and surname" | "nome e apelido" | "nome" | "name" => {}

        // Account label / number
        "account" | "account number" | "conta" | "conta no" | "conta n" | "n da conta"
        | "numero da conta" => {
            if meta.account_label.is_none() && !text.is_empty() {
                meta.account_label = Some(text);
            }
        }

        // Reporting currency
        "currency" | "moeda" if meta.account_currency.is_none() => {
            if let Some(c) = Currency::parse(&text) {
                meta.account_currency = Some(c);
            }
        }

        // Snapshot timestamp / Date to
        "date to" | "data ate" | "ate" | "as of" | "data as of report generated"
            if meta.as_of.is_none() =>
        {
            if let Some(pt) = parse_cell_time(val, tz, None) {
                meta.as_of = Some(pt.time.at);
            }
        }

        _ => {}
    }
}
