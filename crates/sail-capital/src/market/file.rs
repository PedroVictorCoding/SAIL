//! Parser for owner-uploaded price files (round 10, Antigravity's slice).
//!
//! Accepts CSV files with comma (`,`) or semicolon (`;`) separators and an optional
//! UTF-8 BOM. Headers are recognized in English or Portuguese. Closes must be positive.
use super::{
    Adjustment, MAX_PRICE_FILE_BYTES, PriceColumns, PriceSeries, PriceSourceError, read_price_bars,
};
use crate::contracts::Currency;
use chrono::NaiveDate;

/// Parse dates formatted as `YYYY-MM-DD`, `DD.MM.YYYY`, or `DD/MM/YYYY`.
fn parse_flexible_date(text: &str) -> Option<NaiveDate> {
    let t = text.trim();
    NaiveDate::parse_from_str(t, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(t, "%d.%m.%Y"))
        .or_else(|_| NaiveDate::parse_from_str(t, "%d/%m/%Y"))
        .ok()
}

fn detect_columns(headers: &csv::StringRecord) -> Option<PriceColumns> {
    let mut date_col = None;
    let mut primary_close_col = None;
    let mut adj_close_col = None;
    let mut open_col = None;
    let mut high_col = None;
    let mut low_col = None;
    let mut volume_col = None;

    for (idx, header) in headers.iter().enumerate() {
        let norm = header.trim().to_ascii_lowercase();
        match norm.as_str() {
            "date" | "data" => date_col = Some(idx),
            "close" | "fecho" | "price" | "preço" | "preco" => primary_close_col = Some(idx),
            "adj close" | "adj_close" | "adjclose" => adj_close_col = Some(idx),
            "open" | "abertura" => open_col = Some(idx),
            "high" | "máximo" | "maximo" | "max" => high_col = Some(idx),
            "low" | "mínimo" | "minimo" | "min" => low_col = Some(idx),
            "volume" | "vol" => volume_col = Some(idx),
            _ => {}
        }
    }

    let date = date_col?;
    let close = primary_close_col.or(adj_close_col)?;

    Some(PriceColumns {
        date,
        close,
        open: open_col,
        high: high_col,
        low: low_col,
        volume: volume_col,
    })
}

/// Reads an owner-uploaded price file: CSV with a header row naming at least a date and
/// a close column (English or Portuguese), comma or semicolon separated. `currency` and
/// `adjustment` are what the owner declares for the file.
pub fn parse_price_file(
    bytes: &[u8],
    symbol: &str,
    currency: Option<Currency>,
    adjustment: Adjustment,
) -> Result<PriceSeries, PriceSourceError> {
    if bytes.len() > MAX_PRICE_FILE_BYTES {
        return Err(PriceSourceError::TooLarge);
    }
    if bytes.is_empty() {
        return Err(PriceSourceError::Malformed("empty price file".into()));
    }

    // Strip optional UTF-8 BOM
    let payload = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    };

    // Try comma first, then semicolon
    let delimiters = b",;";
    let mut chosen = None;

    for &delim in delimiters {
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(delim)
            .has_headers(true)
            .flexible(true)
            .from_reader(payload);

        let cols = reader.headers().ok().and_then(detect_columns);
        if let Some(cols) = cols {
            chosen = Some((delim, cols));
            break;
        }
    }

    let (delimiter, cols) = chosen.ok_or_else(|| {
        PriceSourceError::Malformed(
            "missing date or close column in header (supported separators: comma or semicolon)"
                .into(),
        )
    })?;

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true)
        .from_reader(payload);

    let bars = read_price_bars(&mut reader, &cols, parse_flexible_date, "price file")?;

    Ok(PriceSeries {
        source_id: "owner-file".to_string(),
        symbol: symbol.to_string(),
        source_symbol: symbol.to_string(),
        currency,
        adjustment,
        bars,
        payload_sha256: crate::sha256_hex(bytes),
    })
}
