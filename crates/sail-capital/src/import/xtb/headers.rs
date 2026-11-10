//! Header detection, column mapping, and sheet classification for XTB exports.
//!
//! Maps columns by header text in English and Portuguese, never by column position.
//! Scans preamble rows to find the table header row and classify the sheet.
use crate::contracts::SheetKind;
use std::collections::HashMap;

use super::table::RawSheet;

/// Canonical column identity recognised in XTB reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CanonicalColumn {
    /// Broker operation or position ID.
    Id,
    /// Operation or order type.
    Type,
    /// Timestamp of operation.
    Time,
    /// Comment or details.
    Comment,
    /// Ticker symbol.
    Symbol,
    /// Cash amount effect.
    Amount,
    /// Currency code.
    Currency,
    /// ISIN code.
    Isin,
    /// Security name.
    Name,
    /// Position ticket number.
    Position,
    /// Units/volume traded or held.
    Volume,
    /// When position was opened.
    OpenTime,
    /// Price at open.
    OpenPrice,
    /// When position was closed.
    CloseTime,
    /// Price at close.
    ClosePrice,
    /// Market price at export.
    MarketPrice,
    /// Purchase value.
    PurchaseValue,
    /// Sale value.
    SaleValue,
    /// Commission paid.
    Commission,
    /// Financing swap charged or credited.
    Swap,
    /// Gross profit/loss.
    GrossPl,
    /// Margin required.
    Margin,
    /// Nominal value.
    NominalValue,
}

/// Normalise a header text string for resilient matching.
fn normalise_header(text: &str) -> String {
    let lower = text.trim().to_lowercase();
    let mut out = String::with_capacity(lower.len());
    for c in lower.chars() {
        match c {
            'á' | 'à' | 'ã' | 'â' | 'ä' => out.push('a'),
            'é' | 'è' | 'ê' | 'ë' => out.push('e'),
            'í' | 'ì' | 'î' | 'ï' => out.push('i'),
            'ó' | 'ò' | 'õ' | 'ô' | 'ö' => out.push('o'),
            'ú' | 'ù' | 'û' | 'ü' => out.push('u'),
            'ç' => out.push('c'),
            '/' | '-' | '_' | ':' | '.' | ',' => out.push(' '),
            _ => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Header alias table matching English and Portuguese labels to canonical columns.
/// Each line in this table maps a normalised alias to a canonical column.
const ALIASES: &[(&str, CanonicalColumn)] = &[
    // ID / Ticket
    ("id", CanonicalColumn::Id),
    ("ticket", CanonicalColumn::Id),
    ("ordem", CanonicalColumn::Id),
    ("transacao", CanonicalColumn::Id),
    ("transacao id", CanonicalColumn::Id),
    ("n da ordem", CanonicalColumn::Id),
    ("no da ordem", CanonicalColumn::Id),
    ("transaction id", CanonicalColumn::Id),
    // Type
    ("type", CanonicalColumn::Type),
    ("tipo", CanonicalColumn::Type),
    ("tipo de operacao", CanonicalColumn::Type),
    ("action", CanonicalColumn::Type),
    // Time
    ("time", CanonicalColumn::Time),
    ("hora", CanonicalColumn::Time),
    ("data", CanonicalColumn::Time),
    ("data hora", CanonicalColumn::Time),
    ("data e hora", CanonicalColumn::Time),
    ("date", CanonicalColumn::Time),
    ("date time", CanonicalColumn::Time),
    ("tempo", CanonicalColumn::Time),
    // Comment
    ("comment", CanonicalColumn::Comment),
    ("comentario", CanonicalColumn::Comment),
    ("comentarios", CanonicalColumn::Comment),
    ("comments", CanonicalColumn::Comment),
    // Symbol
    ("symbol", CanonicalColumn::Symbol),
    ("simbolo", CanonicalColumn::Symbol),
    ("ticker", CanonicalColumn::Symbol),
    ("instrument", CanonicalColumn::Symbol),
    ("instrumento", CanonicalColumn::Symbol),
    // Amount
    ("amount", CanonicalColumn::Amount),
    ("montante", CanonicalColumn::Amount),
    ("valor", CanonicalColumn::Amount),
    ("quantia", CanonicalColumn::Amount),
    ("cash amount", CanonicalColumn::Amount),
    // Currency
    ("currency", CanonicalColumn::Currency),
    ("moeda", CanonicalColumn::Currency),
    // ISIN
    ("isin", CanonicalColumn::Isin),
    ("isin code", CanonicalColumn::Isin),
    ("codigo isin", CanonicalColumn::Isin),
    // Name
    ("name", CanonicalColumn::Name),
    ("nome", CanonicalColumn::Name),
    ("instrument name", CanonicalColumn::Name),
    // Position
    ("position", CanonicalColumn::Position),
    ("posicao", CanonicalColumn::Position),
    ("posicoes", CanonicalColumn::Position),
    ("n da posicao", CanonicalColumn::Position),
    ("no da posicao", CanonicalColumn::Position),
    // Volume
    ("volume", CanonicalColumn::Volume),
    ("quantidade", CanonicalColumn::Volume),
    ("qtd", CanonicalColumn::Volume),
    ("lots", CanonicalColumn::Volume),
    ("shares", CanonicalColumn::Volume),
    ("acoes", CanonicalColumn::Volume),
    // Open time
    ("open time", CanonicalColumn::OpenTime),
    ("hora de abertura", CanonicalColumn::OpenTime),
    ("data de abertura", CanonicalColumn::OpenTime),
    ("data hora de abertura", CanonicalColumn::OpenTime),
    // Open price
    ("open price", CanonicalColumn::OpenPrice),
    ("preco de abertura", CanonicalColumn::OpenPrice),
    ("abertura", CanonicalColumn::OpenPrice),
    // Close time
    ("close time", CanonicalColumn::CloseTime),
    ("hora de fecho", CanonicalColumn::CloseTime),
    ("data de fecho", CanonicalColumn::CloseTime),
    ("hora de fechamento", CanonicalColumn::CloseTime),
    ("data de fechamento", CanonicalColumn::CloseTime),
    // Close price
    ("close price", CanonicalColumn::ClosePrice),
    ("preco de fecho", CanonicalColumn::ClosePrice),
    ("preco de fechamento", CanonicalColumn::ClosePrice),
    ("fecho", CanonicalColumn::ClosePrice),
    // Market price
    ("market price", CanonicalColumn::MarketPrice),
    ("preco de mercado", CanonicalColumn::MarketPrice),
    ("preco atual", CanonicalColumn::MarketPrice),
    ("cotacao", CanonicalColumn::MarketPrice),
    // Purchase value
    ("purchase value", CanonicalColumn::PurchaseValue),
    ("valor de compra", CanonicalColumn::PurchaseValue),
    // Sale value
    ("sale value", CanonicalColumn::SaleValue),
    ("valor de venda", CanonicalColumn::SaleValue),
    // Commission
    ("commission", CanonicalColumn::Commission),
    ("comissao", CanonicalColumn::Commission),
    // Swap
    ("swap", CanonicalColumn::Swap),
    // Gross P/L
    ("gross p l", CanonicalColumn::GrossPl),
    ("gross pl", CanonicalColumn::GrossPl),
    ("p l bruto", CanonicalColumn::GrossPl),
    ("pl bruto", CanonicalColumn::GrossPl),
    ("lucro bruto", CanonicalColumn::GrossPl),
    ("resultado bruto", CanonicalColumn::GrossPl),
    ("p l", CanonicalColumn::GrossPl),
    ("pl", CanonicalColumn::GrossPl),
    // Margin
    ("margin", CanonicalColumn::Margin),
    ("margem", CanonicalColumn::Margin),
    // Nominal value
    ("nominal value", CanonicalColumn::NominalValue),
    ("valor nominal", CanonicalColumn::NominalValue),
];

/// Match a raw header cell text against canonical columns.
pub fn match_header_column(text: &str) -> Option<CanonicalColumn> {
    let norm = normalise_header(text);
    if norm.is_empty() {
        return None;
    }
    for (alias, col) in ALIASES {
        if norm == *alias {
            return Some(*col);
        }
    }
    None
}

/// Complete mapping of canonical columns to 0-based column indices for a sheet.
#[derive(Clone, Debug)]
pub struct HeaderMap {
    /// Classified kind of this sheet.
    pub sheet_kind: SheetKind,
    /// 1-based row number of the header.
    pub header_row: u32,
    /// Map of canonical column to index.
    pub columns: HashMap<CanonicalColumn, usize>,
}

impl HeaderMap {
    /// Get the 0-based index of a canonical column if found.
    pub fn col(&self, col: CanonicalColumn) -> Option<usize> {
        self.columns.get(&col).copied()
    }
}

/// Find the table header row and classify the sheet by scanning the first 40 rows.
pub fn detect_sheet_header(sheet: &RawSheet) -> Option<HeaderMap> {
    let scan_limit = sheet.rows.len().min(40);
    let mut best_match: Option<(SheetKind, u32, HashMap<CanonicalColumn, usize>, u32)> = None;

    // Hint from sheet name if present
    let sheet_hint = sheet.name.as_deref().map(normalise_header);

    for (row_idx, row) in sheet.rows[..scan_limit].iter().enumerate() {
        if row.is_all_empty() {
            continue;
        }

        let mut cols = HashMap::new();
        for (col_idx, cell) in row.cells.iter().enumerate() {
            if let Some(canonical) = match_header_column(cell.as_str()) {
                cols.entry(canonical).or_insert(col_idx);
            }
        }

        // Try classifying this candidate header row.
        let has_type = cols.contains_key(&CanonicalColumn::Type);
        let has_time = cols.contains_key(&CanonicalColumn::Time);
        let has_amount = cols.contains_key(&CanonicalColumn::Amount);
        let has_volume = cols.contains_key(&CanonicalColumn::Volume);
        let has_open_price = cols.contains_key(&CanonicalColumn::OpenPrice);
        let has_close_price = cols.contains_key(&CanonicalColumn::ClosePrice);
        let has_close_time = cols.contains_key(&CanonicalColumn::CloseTime);
        let has_market_price = cols.contains_key(&CanonicalColumn::MarketPrice);
        let has_position = cols.contains_key(&CanonicalColumn::Position);
        let has_symbol = cols.contains_key(&CanonicalColumn::Symbol);

        // Score for CashOperations: requires Amount, plus Type or Time
        let is_cash = has_amount && (has_type || has_time);
        let cash_score = if is_cash {
            let mut score = 10;
            if has_type {
                score += 5;
            }
            if has_time {
                score += 5;
            }
            if cols.contains_key(&CanonicalColumn::Id) {
                score += 5;
            }
            if cols.contains_key(&CanonicalColumn::Comment) {
                score += 5;
            }
            if has_symbol {
                score += 5;
            }
            if let Some(hint) = &sheet_hint {
                let matches_cash = hint.contains("cash") || hint.contains("caixa");
                if matches_cash {
                    score += 20;
                }
            }
            score
        } else {
            0
        };

        // Score for ClosedPositions: requires ClosePrice or CloseTime, plus Volume or OpenPrice
        let is_closed = (has_close_price || has_close_time)
            && (has_volume || has_open_price || has_position || has_symbol);
        let closed_score = if is_closed {
            let mut score = 10;
            if has_close_price {
                score += 10;
            }
            if has_close_time {
                score += 10;
            }
            if has_open_price {
                score += 5;
            }
            if has_volume {
                score += 5;
            }
            if cols.contains_key(&CanonicalColumn::GrossPl) {
                score += 5;
            }
            if let Some(hint) = &sheet_hint {
                let matches_closed = hint.contains("closed") || hint.contains("fechada");
                if matches_closed {
                    score += 20;
                }
            }
            score
        } else {
            0
        };

        // Score for OpenPositions: OpenPrice / MarketPrice without ClosePrice / CloseTime
        let is_open = (has_market_price || (has_open_price && !has_close_price && !has_close_time))
            && (has_volume || has_symbol || has_position);
        let open_score = if is_open {
            let mut score = 10;
            if has_market_price {
                score += 10;
            }
            if has_open_price {
                score += 5;
            }
            if has_volume {
                score += 5;
            }
            if cols.contains_key(&CanonicalColumn::PurchaseValue) {
                score += 5;
            }
            if let Some(hint) = &sheet_hint {
                let matches_open = hint.contains("open") || hint.contains("aberta");
                if matches_open {
                    score += 20;
                }
            }
            score
        } else {
            0
        };

        // Score for Orders:
        let is_orders = cols.contains_key(&CanonicalColumn::Id)
            && has_type
            && has_volume
            && sheet_hint
                .as_ref()
                .map(|h| h.contains("order") || h.contains("ordem") || h.contains("pending"))
                .unwrap_or(false);
        let orders_score = if is_orders { 15 } else { 0 };

        let (kind, score) =
            if cash_score > closed_score && cash_score > open_score && cash_score > orders_score {
                (SheetKind::CashOperations, cash_score)
            } else if closed_score > open_score && closed_score > orders_score {
                (SheetKind::ClosedPositions, closed_score)
            } else if open_score > orders_score {
                (SheetKind::OpenPositions, open_score)
            } else if orders_score > 0 {
                (SheetKind::Orders, orders_score)
            } else {
                (SheetKind::Unknown, 0)
            };

        if score > 0 {
            let row_number = (row_idx + 1) as u32;
            if best_match
                .as_ref()
                .map(|(_, _, _, s)| score > *s)
                .unwrap_or(true)
            {
                best_match = Some((kind, row_number, cols, score));
            }
        }
    }

    best_match.map(|(sheet_kind, header_row, columns, _)| HeaderMap {
        sheet_kind,
        header_row,
        columns,
    })
}
