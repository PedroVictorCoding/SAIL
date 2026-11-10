//! Parsing for XTB cash operation history sheets.
//!
//! Maps official cash operation types into [`EntryKind`], parses quantity and price
//! from trade comments using exact patterns, derives natural keys with collision
//! disambiguation, and emits typed [`ParsedEntry`] records.
use crate::contracts::{
    Currency, EntryKind, FeeKind, ImportIssue, Instrument, InstrumentKind, Money, ParsedEntry,
    RowRef, Severity, Side, TaxKind, isin_is_valid, sha256_hex,
};
use rust_decimal::Decimal;
use std::collections::HashMap;

use super::datetime::parse_cell_time;
use super::headers::{CanonicalColumn, HeaderMap};
use super::table::{RawCell, RawRow};

/// Context and parameters for parsing cash rows in a sheet.
pub struct CashRowContext<'a> {
    /// Column mapping for this sheet.
    pub headers: &'a HeaderMap,
    /// Sheet name if in a workbook.
    pub sheet_name: Option<&'a str>,
    /// Reporting currency discovered from preamble.
    pub account_currency: Option<Currency>,
    /// Default currency from import options.
    pub default_currency: Option<Currency>,
    /// Report time zone.
    pub tz: chrono_tz::Tz,
}

/// Normalise operation type strings for case-insensitive matching.
fn normalise_type(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    let mut out = String::with_capacity(lower.len());
    for c in lower.chars() {
        match c {
            'á' | 'à' | 'ã' | 'â' => out.push('a'),
            'é' | 'è' | 'ê' => out.push('e'),
            'í' | 'ì' => out.push('i'),
            'ó' | 'ò' | 'õ' | 'ô' => out.push('o'),
            'ú' | 'ù' => out.push('u'),
            'ç' => out.push('c'),
            _ => out.push(c),
        }
    }
    out
}

/// Parse quantity, price and optional position reference from a trade comment.
///
/// Supported patterns include:
/// - `OPEN BUY 3 @ 172.50`
/// - `CLOSE BUY 2/5 @ 180.00`
/// - `BUY 10 @ 45.20`
/// - `ABERTURA COMPRA 3 @ 172.50`
/// - `FECHO COMPRA 2/5 @ 180.00`
///
/// Returns `(None, None, None)` if the comment does not strictly conform to an expected pattern.
pub fn parse_trade_comment(
    comment: &str,
    currency: Option<Currency>,
) -> (Option<Decimal>, Option<Money>, Option<String>) {
    let words: Vec<&str> = comment.split_whitespace().collect();
    if words.is_empty() {
        return (None, None, None);
    }

    let mut idx = 0;

    // Optional prefix: OPEN / CLOSE / ABERTURA / FECHO
    let first = normalise_type(words[0]);
    if matches!(
        first.as_str(),
        "open" | "close" | "abertura" | "fecho" | "fechamento"
    ) {
        idx += 1;
    }

    if idx >= words.len() {
        return (None, None, None);
    }

    // Optional or expected direction: BUY / SELL / COMPRA / VENDA
    let dir = normalise_type(words[idx]);
    if matches!(dir.as_str(), "buy" | "sell" | "compra" | "venda") {
        idx += 1;
    }

    if idx >= words.len() {
        return (None, None, None);
    }

    // Quantity token: e.g. "3", "2/5", "0.5"
    let qty_token = words[idx];
    idx += 1;

    let (qty_str, position_ref) = if let Some((q, p)) = qty_token.split_once('/') {
        let pos = if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() {
            Some(p.to_string())
        } else {
            None
        };
        (q, pos)
    } else {
        (qty_token, None)
    };

    let quantity = crate::contracts::parse_decimal_text(qty_str);
    if quantity.is_none() {
        return (None, None, None);
    }

    if idx >= words.len() {
        return (None, None, None);
    }

    // Separator '@' or 'at' or '@<price>'
    let sep = words[idx];
    let price_str = if sep == "@" || sep.eq_ignore_ascii_case("at") {
        idx += 1;
        if idx >= words.len() {
            return (None, None, None);
        }
        words[idx]
    } else if let Some(p) = sep.strip_prefix('@') {
        p
    } else {
        return (None, None, None);
    };
    idx += 1;

    // The comment's price is in the instrument's trading currency, which the cash row
    // doesn't state (its amount is in account currency), so callers pass `None` and the
    // price is dropped rather than mislabelled. The quantity is still exact.
    let price_val = crate::contracts::parse_decimal_text(price_str);
    let price = match (price_val, currency) {
        (Some(pv), Some(curr)) => Some(Money::new(pv, curr)),
        _ => None,
    };

    // Optional trailing position ref, e.g. "/ 123456"
    let mut final_pos_ref = position_ref;
    if final_pos_ref.is_none() && idx < words.len() {
        let rest = words[idx..].join(" ");
        if let Some(pos) = rest.strip_prefix('/').map(str::trim) {
            let is_digits = !pos.is_empty() && pos.chars().all(|c| c.is_ascii_digit());
            if is_digits {
                final_pos_ref = Some(pos.to_string());
            }
        }
    }

    (quantity, price, final_pos_ref)
}

/// Parse a single cash operation row into a [`ParsedEntry`].
pub fn parse_cash_row(
    row: &RawRow,
    ctx: &CashRowContext<'_>,
    seen_hashes: &mut HashMap<String, u32>,
    issues: &mut Vec<ImportIssue>,
) -> Result<ParsedEntry, ImportIssue> {
    let row_ref = RowRef {
        sheet: ctx.sheet_name.map(str::to_string),
        sheet_kind: ctx.headers.sheet_kind,
        row: row.row_number,
    };

    // 1. Amount (cash effect)
    let amount_col = ctx
        .headers
        .col(CanonicalColumn::Amount)
        .ok_or_else(|| ImportIssue {
            severity: Severity::Error,
            code: "missing_column".to_string(),
            message: "cash operations sheet missing Amount column".to_string(),
            source: Some(row_ref.clone()),
        })?;

    let amount_cell = row.get(amount_col).unwrap_or(&RawCell::Empty);
    let amount_val = amount_cell.to_decimal().ok_or_else(|| ImportIssue {
        severity: Severity::Error,
        code: "bad_number".to_string(),
        message: format!("cannot parse amount as number: {:?}", amount_cell.as_str()),
        source: Some(row_ref.clone()),
    })?;

    // 2. Currency
    let currency_col_val = ctx
        .headers
        .col(CanonicalColumn::Currency)
        .and_then(|c| row.get(c))
        .and_then(|c| Currency::parse(c.as_str()));

    let currency = currency_col_val
        .or_else(|| ctx.account_currency.clone())
        .or_else(|| ctx.default_currency.clone())
        .ok_or_else(|| ImportIssue {
            severity: Severity::Error,
            code: "missing_currency".to_string(),
            message: "no currency specified for cash operation".to_string(),
            source: Some(row_ref.clone()),
        })?;

    let cash_effect = Some(Money::new(amount_val, currency.clone()));

    // 3. Time
    let time_col = ctx
        .headers
        .col(CanonicalColumn::Time)
        .ok_or_else(|| ImportIssue {
            severity: Severity::Error,
            code: "missing_column".to_string(),
            message: "cash operations sheet missing Time column".to_string(),
            source: Some(row_ref.clone()),
        })?;

    let time_cell = row.get(time_col).unwrap_or(&RawCell::Empty);
    let parsed_time =
        parse_cell_time(time_cell, ctx.tz, Some(row_ref.clone())).ok_or_else(|| ImportIssue {
            severity: Severity::Error,
            code: "bad_datetime".to_string(),
            message: format!("cannot parse operation timestamp: {:?}", time_cell.as_str()),
            source: Some(row_ref.clone()),
        })?;

    if let Some(time_issue) = parsed_time.issue {
        issues.push(time_issue);
    }
    let occurred = parsed_time.time;

    // 4. Broker ID
    let broker_ref = ctx
        .headers
        .col(CanonicalColumn::Id)
        .and_then(|c| row.get(c))
        .map(|c| c.to_clean_text())
        .and_then(|(s, cut)| {
            if cut {
                issues.push(ImportIssue {
                    severity: Severity::Warning,
                    code: "cell_text_truncated".to_string(),
                    message: "ID cell text was truncated".to_string(),
                    source: Some(row_ref.clone()),
                });
            }
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

    // 5. Raw Type
    let raw_type_text = ctx
        .headers
        .col(CanonicalColumn::Type)
        .and_then(|c| row.get(c))
        .map(|c| c.to_clean_text().0)
        .unwrap_or_default();

    // 6. Comment
    let comment = ctx
        .headers
        .col(CanonicalColumn::Comment)
        .and_then(|c| row.get(c))
        .map(|c| c.to_clean_text())
        .and_then(|(s, cut)| {
            if cut {
                issues.push(ImportIssue {
                    severity: Severity::Warning,
                    code: "cell_text_truncated".to_string(),
                    message: "comment cell text was truncated".to_string(),
                    source: Some(row_ref.clone()),
                });
            }
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

    // 7. Symbol and Instrument
    let raw_symbol = ctx
        .headers
        .col(CanonicalColumn::Symbol)
        .and_then(|c| row.get(c))
        .map(|c| c.to_clean_text().0)
        .unwrap_or_default();

    let isin = ctx
        .headers
        .col(CanonicalColumn::Isin)
        .and_then(|c| row.get(c))
        .map(|c| c.as_str().trim().to_string())
        .filter(|s| isin_is_valid(s));

    let instrument_name = ctx
        .headers
        .col(CanonicalColumn::Name)
        .and_then(|c| row.get(c))
        .map(|c| c.to_clean_text().0)
        .filter(|s| !s.is_empty());

    let norm_type = normalise_type(&raw_type_text);

    // Instrument kind rules:
    // - Stock only when row type says Stocks/ETF
    // - Cfd only when sheet or type says so (e.g. close trade, swap, or CFD sheet)
    // - Otherwise Unknown
    let is_cfd_type = matches!(
        norm_type.as_str(),
        "close trade" | "swap" | "fecho de negociacao" | "fechar negociacao"
    );
    let is_stock_type = norm_type.contains("stocks/etf")
        || norm_type.contains("stock/etf")
        || norm_type.contains("acoes/etf")
        || norm_type.contains("acoes")
        || norm_type.contains("stocks");

    let inst_kind = if is_cfd_type {
        InstrumentKind::Cfd
    } else if is_stock_type {
        InstrumentKind::Stock
    } else {
        InstrumentKind::Unknown
    };

    let instrument = if !raw_symbol.is_empty() {
        Some(Instrument {
            symbol: raw_symbol.clone(),
            isin,
            name: instrument_name,
            currency: None,
            kind: inst_kind,
        })
    } else {
        None
    };

    // 8. Map EntryKind
    let kind = match norm_type.as_str() {
        "deposit" | "deposito" => EntryKind::Deposit,
        "withdrawal" | "levantamento" => EntryKind::Withdrawal,
        "sub account transfer"
        | "sub-account transfer"
        | "transfer"
        | "transferencia entre subcontas"
        | "transferencia" => EntryKind::Transfer,
        "stocks/etf purchase"
        | "compra de acoes/etf"
        | "stock purchase"
        | "etf purchase"
        | "compra de acoes"
        | "compra de etf" => {
            let inst = instrument.clone().unwrap_or_else(|| Instrument {
                symbol: "UNKNOWN".to_string(),
                isin: None,
                name: None,
                currency: None,
                kind: InstrumentKind::Stock,
            });
            let (quantity, price, pos_ref) = comment
                .as_deref()
                .map(|c| parse_trade_comment(c, None))
                .unwrap_or((None, None, None));
            EntryKind::Trade {
                instrument: inst,
                side: Side::Buy,
                quantity,
                price,
                position_ref: pos_ref.or_else(|| broker_ref.clone()),
            }
        }
        "stocks/etf sale" | "venda de acoes/etf" | "stock sale" | "etf sale" | "venda de acoes"
        | "venda de etf" => {
            let inst = instrument.clone().unwrap_or_else(|| Instrument {
                symbol: "UNKNOWN".to_string(),
                isin: None,
                name: None,
                currency: None,
                kind: InstrumentKind::Stock,
            });
            let (quantity, price, pos_ref) = comment
                .as_deref()
                .map(|c| parse_trade_comment(c, None))
                .unwrap_or((None, None, None));
            EntryKind::Trade {
                instrument: inst,
                side: Side::Sell,
                quantity,
                price,
                position_ref: pos_ref.or_else(|| broker_ref.clone()),
            }
        }
        "divident" | "dividend" | "dividendo" | "dividendos" => EntryKind::Dividend {
            instrument: instrument.clone(),
        },
        "withholding tax" | "imposto retido na fonte" | "retencao na fonte" => EntryKind::Tax {
            kind: TaxKind::Withholding,
            instrument: instrument.clone(),
        },
        "free-funds interest" | "juros de fundos livres" | "interest" | "juros" => {
            EntryKind::Interest
        }
        "free-funds interest tax" | "imposto sobre juros de fundos livres" => EntryKind::Tax {
            kind: TaxKind::OnInterest,
            instrument: None,
        },
        "sec fee" | "taxa sec" => EntryKind::Fee {
            kind: FeeKind::Regulatory,
            instrument: instrument.clone(),
        },
        "commission" | "comissao" => EntryKind::Fee {
            kind: FeeKind::Commission,
            instrument: instrument.clone(),
        },
        "swap" => EntryKind::Fee {
            kind: FeeKind::Swap,
            instrument: instrument.clone(),
        },
        "close trade" | "fecho de negociacao" | "fechar negociacao" => {
            let inst = instrument.clone().unwrap_or_else(|| Instrument {
                symbol: raw_symbol.clone(),
                isin: None,
                name: None,
                currency: None,
                kind: InstrumentKind::Cfd,
            });
            EntryKind::DerivativeResult {
                instrument: inst,
                position_ref: broker_ref.clone(),
            }
        }
        _ => {
            // Unrecognised type becomes Unsupported with a warning issue; row is kept.
            issues.push(ImportIssue {
                severity: Severity::Warning,
                code: "unknown_type".to_string(),
                message: format!("unsupported cash operation type: {raw_type_text}"),
                source: Some(row_ref.clone()),
            });
            EntryKind::Unsupported {
                raw_type: raw_type_text.clone(),
            }
        }
    };

    // 9. Natural key generation
    let natural_key = match &broker_ref {
        Some(id) => format!("xtb:cash:{id}"),
        None => {
            let kind_label = match &kind {
                EntryKind::Deposit => "deposit",
                EntryKind::Withdrawal => "withdrawal",
                EntryKind::Transfer => "transfer",
                EntryKind::Trade {
                    side: Side::Buy, ..
                } => "trade:buy",
                EntryKind::Trade {
                    side: Side::Sell, ..
                } => "trade:sell",
                EntryKind::Dividend { .. } => "dividend",
                EntryKind::Tax {
                    kind: TaxKind::Withholding,
                    ..
                } => "tax:withholding",
                EntryKind::Tax {
                    kind: TaxKind::OnInterest,
                    ..
                } => "tax:on_interest",
                EntryKind::Tax { .. } => "tax:other",
                EntryKind::Interest => "interest",
                EntryKind::Fee {
                    kind: FeeKind::Regulatory,
                    ..
                } => "fee:regulatory",
                EntryKind::Fee {
                    kind: FeeKind::Commission,
                    ..
                } => "fee:commission",
                EntryKind::Fee {
                    kind: FeeKind::Swap,
                    ..
                } => "fee:swap",
                EntryKind::Fee { .. } => "fee:other",
                EntryKind::DerivativeResult { .. } => "derivative_result",
                EntryKind::Unsupported { raw_type } => raw_type.as_str(),
            };
            let time_str = occurred.at.to_rfc3339();
            let amount_str = amount_val.to_string();
            let curr_str = currency.as_str();
            let sym_str = instrument.as_ref().map(|i| i.symbol.as_str()).unwrap_or("");
            let comm_str = comment.as_deref().unwrap_or("");

            let norm_row =
                format!("{kind_label}|{time_str}|{amount_str}|{curr_str}|{sym_str}|{comm_str}");
            let hash = sha256_hex(norm_row.as_bytes());
            let base_key = format!("xtb:cash:h:{hash}");

            let count = seen_hashes.entry(base_key.clone()).or_insert(0);
            *count += 1;
            if *count > 1 {
                let disambiguated = format!("{base_key}#{}", *count);
                issues.push(ImportIssue {
                    severity: Severity::Info,
                    code: "duplicate_row_disambiguated".to_string(),
                    message: format!(
                        "duplicate operation without ID assigned disambiguated key: {disambiguated}"
                    ),
                    source: Some(row_ref.clone()),
                });
                disambiguated
            } else {
                base_key
            }
        }
    };

    Ok(ParsedEntry {
        natural_key,
        source: row_ref,
        broker_ref,
        occurred,
        kind,
        cash_effect,
        comment,
    })
}
