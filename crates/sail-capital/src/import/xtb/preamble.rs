//! Preamble metadata extraction for XTB reports.
//!
//! Extracts account number and reporting currency from preamble rows located above the
//! main table header. Never extracts or stores the owner's name.
use crate::contracts::Currency;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use super::datetime::parse_time_text;
use super::table::RawSheet;

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
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract preamble metadata from rows before `header_row` (1-based).
pub fn extract_preamble(sheet: &RawSheet, header_row: u32, tz: Tz) -> PreambleMetadata {
    let mut meta = PreambleMetadata::default();
    let limit = (header_row.saturating_sub(1) as usize).min(sheet.rows.len());

    for row in &sheet.rows[..limit] {
        if row.is_all_empty() {
            continue;
        }

        for (col_idx, cell) in row.cells.iter().enumerate() {
            let text = cell.as_str().trim();
            if text.is_empty() {
                continue;
            }

            // If text contains ':', determine whether the value is inside the same cell
            // or in the subsequent cell.
            let (label_to_check, val_in_same_cell) = if let Some((lbl, val)) = text.split_once(':')
            {
                let v = val.trim();
                if !v.is_empty() {
                    (lbl.trim(), Some(v))
                } else {
                    (lbl.trim(), None)
                }
            } else {
                (text, None)
            };

            if let Some(v) = val_in_same_cell {
                process_key_value(label_to_check, v, tz, &mut meta);
                continue;
            }

            // Check if label is in this cell and value is in the next cell
            let norm = normalise_key(label_to_check);
            if is_known_preamble_label(&norm) {
                let next_val = row
                    .cells
                    .get(col_idx + 1)
                    .map(|c| c.as_str().trim())
                    .unwrap_or("");
                if !next_val.is_empty() {
                    process_key_value(&norm, next_val, tz, &mut meta);
                }
            }
        }
    }

    meta
}

/// Returns true if this string looks like a preamble label.
fn is_known_preamble_label(norm: &str) -> bool {
    matches!(
        norm,
        "account"
            | "conta"
            | "conta no"
            | "conta n"
            | "n da conta"
            | "currency"
            | "moeda"
            | "date to"
            | "data ate"
            | "ate"
            | "as of"
            | "saldo"
            | "balance"
    )
}

/// Process a single key-value pair found in the preamble.
fn process_key_value(key: &str, val: &str, tz: Tz, meta: &mut PreambleMetadata) {
    let norm = normalise_key(key);
    match norm.as_str() {
        // Explicitly IGNORE owner name rows.
        "name and surname" | "nome e apelido" | "nome" | "name" => {}

        // Account label / number
        "account" | "conta" | "conta no" | "conta n" | "n da conta" => {
            if meta.account_label.is_none() && !val.is_empty() {
                meta.account_label = Some(val.to_string());
            }
        }

        // Reporting currency
        "currency" | "moeda" if meta.account_currency.is_none() => {
            if let Some(c) = Currency::parse(val) {
                meta.account_currency = Some(c);
            }
        }

        // Snapshot timestamp / Date to
        "date to" | "data ate" | "ate" | "as of" if meta.as_of.is_none() => {
            if let Some(pt) = parse_time_text(val, tz, None) {
                meta.as_of = Some(pt.time.at);
            }
        }

        _ => {}
    }
}
