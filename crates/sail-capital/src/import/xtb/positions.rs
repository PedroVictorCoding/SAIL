//! Parsing for XTB open and closed position sheets.
//!
//! Extracts [`BrokerPosition`] and [`BrokerClosedPosition`] records, handles summary/balance
//! rows in open position sheets, and maps instruments and timestamps according to contract rules.
use crate::contracts::{
    BrokerClosedPosition, BrokerPosition, Currency, ImportIssue, Instrument, InstrumentKind, Money,
    RowRef, Severity, isin_is_valid,
};
use rust_decimal::Decimal;

use super::datetime::parse_cell_time;
use super::headers::{CanonicalColumn, HeaderMap};
use super::table::{RawCell, RawRow};

/// Normalise a string for matching row types or summary labels.
fn normalise_text(raw: &str) -> String {
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

/// Check if a row in an open positions sheet represents a total or account summary row.
///
/// If detected, returns `Some((Option<Money>, Option<Money>))` containing `(balance, equity)`.
pub fn detect_open_positions_summary_row(
    row: &RawRow,
    currency: &Currency,
) -> Option<(Option<Money>, Option<Money>)> {
    let mut is_summary = false;
    let mut balance = None;
    let mut equity = None;

    for (idx, cell) in row.cells.iter().enumerate() {
        let norm = normalise_text(cell.as_str());
        if norm.contains("total")
            || norm.contains("totais")
            || norm.contains("balance")
            || norm.contains("saldo")
            || norm.contains("equity")
            || norm.contains("patrimonio")
        {
            is_summary = true;
        }

        if (norm.contains("balance") || norm.contains("saldo")) && balance.is_none() {
            let next_dec = row.cells.get(idx + 1).and_then(|c| c.to_decimal());
            if let Some(dec) = next_dec {
                balance = Some(Money::new(dec, currency.clone()));
            }
        }

        if (norm.contains("equity") || norm.contains("patrimonio")) && equity.is_none() {
            let next_dec = row.cells.get(idx + 1).and_then(|c| c.to_decimal());
            if let Some(dec) = next_dec {
                equity = Some(Money::new(dec, currency.clone()));
            }
        }
    }

    if is_summary {
        Some((balance, equity))
    } else {
        None
    }
}

/// Helper to parse an optional money column.
fn parse_money_col(
    row: &RawRow,
    headers: &HeaderMap,
    col: CanonicalColumn,
    currency: &Currency,
) -> Option<Money> {
    headers
        .col(col)
        .and_then(|c| row.get(c))
        .and_then(|c| c.to_decimal())
        .map(|d| Money::new(d, currency.clone()))
}

/// Parse a row from an open positions sheet into a [`BrokerPosition`].
pub fn parse_open_position_row(
    row: &RawRow,
    headers: &HeaderMap,
    sheet_name: Option<&str>,
    account_currency: Option<Currency>,
    default_currency: Option<Currency>,
    tz: chrono_tz::Tz,
    issues: &mut Vec<ImportIssue>,
) -> Result<BrokerPosition, ImportIssue> {
    let row_ref = RowRef {
        sheet: sheet_name.map(str::to_string),
        sheet_kind: headers.sheet_kind,
        row: row.row_number,
    };

    // 1. Currency
    let currency = headers
        .col(CanonicalColumn::Currency)
        .and_then(|c| row.get(c))
        .and_then(|c| Currency::parse(c.as_str()))
        .or(account_currency)
        .or(default_currency)
        .unwrap_or_else(Currency::eur);

    // 2. Volume (quantity)
    let volume_col = headers
        .col(CanonicalColumn::Volume)
        .ok_or_else(|| ImportIssue {
            severity: Severity::Error,
            code: "missing_column".to_string(),
            message: "open positions sheet missing Volume column".to_string(),
            source: Some(row_ref.clone()),
        })?;

    let volume_cell = row.get(volume_col).unwrap_or(&RawCell::Empty);
    let quantity = volume_cell.to_decimal().ok_or_else(|| ImportIssue {
        severity: Severity::Error,
        code: "bad_number".to_string(),
        message: format!("cannot parse volume as number: {:?}", volume_cell.as_str()),
        source: Some(row_ref.clone()),
    })?;

    if quantity <= Decimal::ZERO {
        return Err(ImportIssue {
            severity: Severity::Error,
            code: "bad_number".to_string(),
            message: format!("open position volume must be positive: {quantity}"),
            source: Some(row_ref.clone()),
        });
    }

    // 3. Symbol and Instrument
    let symbol_col = headers
        .col(CanonicalColumn::Symbol)
        .ok_or_else(|| ImportIssue {
            severity: Severity::Error,
            code: "missing_column".to_string(),
            message: "open positions sheet missing Symbol column".to_string(),
            source: Some(row_ref.clone()),
        })?;

    let raw_symbol = row.text(symbol_col).trim().to_string();
    if raw_symbol.is_empty() {
        return Err(ImportIssue {
            severity: Severity::Error,
            code: "missing_symbol".to_string(),
            message: "empty symbol in open positions row".to_string(),
            source: Some(row_ref.clone()),
        });
    }

    let isin = headers
        .col(CanonicalColumn::Isin)
        .and_then(|c| row.get(c))
        .map(|c| c.as_str().trim().to_string())
        .filter(|s| isin_is_valid(s));

    let instrument_name = headers
        .col(CanonicalColumn::Name)
        .and_then(|c| row.get(c))
        .map(|c| c.to_clean_text().0)
        .filter(|s| !s.is_empty());

    let raw_type = headers
        .col(CanonicalColumn::Type)
        .and_then(|c| row.get(c))
        .map(|c| c.as_str())
        .unwrap_or("");
    let norm_type = normalise_text(raw_type);
    let sheet_norm = sheet_name.map(normalise_text).unwrap_or_default();

    let is_cfd = norm_type.contains("cfd") || sheet_norm.contains("cfd");
    let is_stock = norm_type.contains("stocks/etf")
        || norm_type.contains("stock")
        || norm_type.contains("acoes");

    let kind = if is_cfd {
        InstrumentKind::Cfd
    } else if is_stock {
        InstrumentKind::Stock
    } else {
        InstrumentKind::Unknown
    };

    let instrument = Instrument {
        symbol: raw_symbol,
        isin,
        name: instrument_name,
        currency: Some(currency.clone()),
        kind,
    };

    // 4. Position ID
    let position_ref = headers
        .col(CanonicalColumn::Position)
        .or_else(|| headers.col(CanonicalColumn::Id))
        .and_then(|c| row.get(c))
        .map(|c| c.to_clean_text().0)
        .filter(|s| !s.is_empty());

    // 5. Open price & market price
    let open_price = parse_money_col(row, headers, CanonicalColumn::OpenPrice, &currency);
    let market_price = parse_money_col(row, headers, CanonicalColumn::MarketPrice, &currency);
    let market_value = parse_money_col(row, headers, CanonicalColumn::PurchaseValue, &currency);
    let unrealized = parse_money_col(row, headers, CanonicalColumn::GrossPl, &currency);

    // 6. Open time
    let opened = headers
        .col(CanonicalColumn::OpenTime)
        .and_then(|c| row.get(c))
        .and_then(|c| parse_cell_time(c, tz, Some(row_ref.clone())))
        .map(|pt| {
            if let Some(issue) = pt.issue {
                issues.push(issue);
            }
            pt.time
        });

    Ok(BrokerPosition {
        instrument,
        position_ref,
        quantity,
        open_price,
        opened,
        market_price,
        market_value,
        unrealized,
        source: row_ref,
    })
}

/// Parse a row from a closed positions sheet into a [`BrokerClosedPosition`].
pub fn parse_closed_position_row(
    row: &RawRow,
    headers: &HeaderMap,
    sheet_name: Option<&str>,
    account_currency: Option<Currency>,
    default_currency: Option<Currency>,
    tz: chrono_tz::Tz,
    issues: &mut Vec<ImportIssue>,
) -> Result<BrokerClosedPosition, ImportIssue> {
    let row_ref = RowRef {
        sheet: sheet_name.map(str::to_string),
        sheet_kind: headers.sheet_kind,
        row: row.row_number,
    };

    // 1. Currency
    let currency = headers
        .col(CanonicalColumn::Currency)
        .and_then(|c| row.get(c))
        .and_then(|c| Currency::parse(c.as_str()))
        .or(account_currency)
        .or(default_currency)
        .unwrap_or_else(Currency::eur);

    // 2. Volume (quantity)
    let volume_col = headers
        .col(CanonicalColumn::Volume)
        .ok_or_else(|| ImportIssue {
            severity: Severity::Error,
            code: "missing_column".to_string(),
            message: "closed positions sheet missing Volume column".to_string(),
            source: Some(row_ref.clone()),
        })?;

    let volume_cell = row.get(volume_col).unwrap_or(&RawCell::Empty);
    let quantity = volume_cell.to_decimal().ok_or_else(|| ImportIssue {
        severity: Severity::Error,
        code: "bad_number".to_string(),
        message: format!("cannot parse volume as number: {:?}", volume_cell.as_str()),
        source: Some(row_ref.clone()),
    })?;

    // 3. Symbol and Instrument
    let symbol_col = headers
        .col(CanonicalColumn::Symbol)
        .ok_or_else(|| ImportIssue {
            severity: Severity::Error,
            code: "missing_column".to_string(),
            message: "closed positions sheet missing Symbol column".to_string(),
            source: Some(row_ref.clone()),
        })?;

    let raw_symbol = row.text(symbol_col).trim().to_string();
    if raw_symbol.is_empty() {
        return Err(ImportIssue {
            severity: Severity::Error,
            code: "missing_symbol".to_string(),
            message: "empty symbol in closed positions row".to_string(),
            source: Some(row_ref.clone()),
        });
    }

    let isin = headers
        .col(CanonicalColumn::Isin)
        .and_then(|c| row.get(c))
        .map(|c| c.as_str().trim().to_string())
        .filter(|s| isin_is_valid(s));

    let instrument_name = headers
        .col(CanonicalColumn::Name)
        .and_then(|c| row.get(c))
        .map(|c| c.to_clean_text().0)
        .filter(|s| !s.is_empty());

    let raw_type = headers
        .col(CanonicalColumn::Type)
        .and_then(|c| row.get(c))
        .map(|c| c.as_str())
        .unwrap_or("");
    let norm_type = normalise_text(raw_type);
    let sheet_norm = sheet_name.map(normalise_text).unwrap_or_default();

    let is_cfd = norm_type.contains("cfd") || sheet_norm.contains("cfd");
    let is_stock = norm_type.contains("stocks/etf")
        || norm_type.contains("stock")
        || norm_type.contains("acoes");

    let kind = if is_cfd {
        InstrumentKind::Cfd
    } else if is_stock {
        InstrumentKind::Stock
    } else {
        InstrumentKind::Unknown
    };

    let instrument = Instrument {
        symbol: raw_symbol,
        isin,
        name: instrument_name,
        currency: Some(currency.clone()),
        kind,
    };

    // 4. Position ID
    let position_ref = headers
        .col(CanonicalColumn::Position)
        .or_else(|| headers.col(CanonicalColumn::Id))
        .and_then(|c| row.get(c))
        .map(|c| c.to_clean_text().0)
        .filter(|s| !s.is_empty());

    // 5. Open price & close price
    let open_price = parse_money_col(row, headers, CanonicalColumn::OpenPrice, &currency);
    let close_price = parse_money_col(row, headers, CanonicalColumn::ClosePrice, &currency);
    let realized = parse_money_col(row, headers, CanonicalColumn::GrossPl, &currency);

    // 6. Open time & close time
    let opened = headers
        .col(CanonicalColumn::OpenTime)
        .and_then(|c| row.get(c))
        .and_then(|c| parse_cell_time(c, tz, Some(row_ref.clone())))
        .map(|pt| {
            if let Some(issue) = pt.issue {
                issues.push(issue);
            }
            pt.time
        });

    let close_time_col = headers
        .col(CanonicalColumn::CloseTime)
        .ok_or_else(|| ImportIssue {
            severity: Severity::Error,
            code: "missing_column".to_string(),
            message: "closed positions sheet missing Close Time column".to_string(),
            source: Some(row_ref.clone()),
        })?;

    let close_time_cell = row.get(close_time_col).unwrap_or(&RawCell::Empty);
    let parsed_close =
        parse_cell_time(close_time_cell, tz, Some(row_ref.clone())).ok_or_else(|| ImportIssue {
            severity: Severity::Error,
            code: "bad_datetime".to_string(),
            message: format!("cannot parse close time: {:?}", close_time_cell.as_str()),
            source: Some(row_ref.clone()),
        })?;

    if let Some(issue) = parsed_close.issue {
        issues.push(issue);
    }
    let closed = parsed_close.time;

    Ok(BrokerClosedPosition {
        instrument,
        position_ref,
        quantity,
        open_price,
        close_price,
        opened,
        closed,
        realized,
        source: row_ref,
    })
}
