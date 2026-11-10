//! In-memory spreadsheet and CSV table abstraction.
//!
//! Converts both Calamine XLSX sheets and CSV files into a uniform grid of [`RawSheet`]s
//! so header detection, preamble parsing and row mapping do not care about the underlying
//! file container.
use crate::contracts::{
    ImportError, MAX_SHEETS, clean_cell_text, decimal_from_cell_f64, parse_decimal_text,
};
use calamine::{Data, Reader, Xlsx};
use rust_decimal::Decimal;
use std::io::Cursor;

/// Untyped cell contents read directly from XLSX or CSV.
#[derive(Clone, Debug, PartialEq)]
pub enum RawCell {
    /// Empty cell or blank text.
    Empty,
    /// String cell.
    Text(String),
    /// 64-bit floating point number.
    Float(f64),
    /// 64-bit integer.
    Int(i64),
    /// Calendar date/time extracted from spreadsheet serial formats.
    DateTime {
        /// Calendar year.
        year: u16,
        /// Month (1-12).
        month: u8,
        /// Day of month (1-31).
        day: u8,
        /// Hour of day (0-23).
        hour: u8,
        /// Minute of hour (0-59).
        minute: u8,
        /// Second of minute (0-59).
        second: u8,
        /// Milliseconds (0-999).
        millisecond: u16,
        /// Whether the cell has a non-zero time of day.
        has_time: bool,
    },
}

impl RawCell {
    /// Returns true if the cell is empty or contains only whitespace.
    pub fn is_empty(&self) -> bool {
        match self {
            RawCell::Empty => true,
            RawCell::Text(s) => s.trim().is_empty(),
            _ => false,
        }
    }

    /// Read the cell as text for header matching or comments.
    pub fn as_str(&self) -> &str {
        match self {
            RawCell::Empty => "",
            RawCell::Text(s) => s.as_str(),
            _ => "",
        }
    }

    /// Clean cell text using the contract rules: stripped control chars, trimmed,
    /// and capped at [`crate::contracts::MAX_CELL_CHARS`].
    pub fn to_clean_text(&self) -> (String, bool) {
        match self {
            RawCell::Empty => (String::new(), false),
            RawCell::Text(s) => clean_cell_text(s),
            RawCell::Float(f) => clean_cell_text(&format!("{f}")),
            RawCell::Int(i) => clean_cell_text(&format!("{i}")),
            RawCell::DateTime {
                year,
                month,
                day,
                hour,
                minute,
                second,
                has_time,
                ..
            } => {
                if *has_time {
                    clean_cell_text(&format!(
                        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
                    ))
                } else {
                    clean_cell_text(&format!("{year:04}-{month:02}-{day:02}"))
                }
            }
        }
    }

    /// Convert numeric cell or text cell to exact decimal.
    pub fn to_decimal(&self) -> Option<Decimal> {
        match self {
            RawCell::Empty => None,
            RawCell::Float(f) => decimal_from_cell_f64(*f),
            RawCell::Int(i) => Some(Decimal::from(*i)),
            RawCell::Text(s) => parse_decimal_text(s),
            RawCell::DateTime { .. } => None,
        }
    }
}

/// A row of cells inside a sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct RawRow {
    /// 1-based row number as presented in spreadsheet software.
    pub row_number: u32,
    /// Cells in this row.
    pub cells: Vec<RawCell>,
}

impl RawRow {
    /// Returns true if every cell in this row is empty.
    pub fn is_all_empty(&self) -> bool {
        self.cells.iter().all(|c| c.is_empty())
    }

    /// Retrieve cell at column index (0-based).
    pub fn get(&self, index: usize) -> Option<&RawCell> {
        self.cells.get(index)
    }

    /// Retrieve text at column index (empty string if missing or not text).
    pub fn text(&self, index: usize) -> &str {
        self.cells.get(index).map(|c| c.as_str()).unwrap_or("")
    }
}

/// A sheet within a workbook or a single CSV document.
#[derive(Clone, Debug, PartialEq)]
pub struct RawSheet {
    /// Sheet name (`None` for CSV files).
    pub name: Option<String>,
    /// All rows in this sheet.
    pub rows: Vec<RawRow>,
}

/// Load an Excel workbook (.xlsx) from memory into a vector of [`RawSheet`]s.
pub fn load_xlsx(bytes: &[u8]) -> Result<Vec<RawSheet>, ImportError> {
    let cursor = Cursor::new(bytes);
    let mut workbook = Xlsx::new(cursor)
        .map_err(|e| ImportError::Unreadable(format!("failed to open XLSX: {e}")))?;

    let sheet_names: Vec<String> = workbook
        .sheet_names()
        .into_iter()
        .take(MAX_SHEETS)
        .collect();

    let mut sheets = Vec::with_capacity(sheet_names.len());

    for name in sheet_names {
        if let Ok(range) = workbook.worksheet_range(&name) {
            let mut rows = Vec::with_capacity(range.height());
            for (row_idx, row) in range.rows().enumerate() {
                let row_number = (row_idx + 1) as u32;
                let mut cells = Vec::with_capacity(row.len());
                for cell in row {
                    let raw_cell = match cell {
                        Data::Empty => RawCell::Empty,
                        Data::String(s) => {
                            let trimmed = s.trim();
                            if trimmed.is_empty() {
                                RawCell::Empty
                            } else {
                                RawCell::Text(s.clone())
                            }
                        }
                        Data::Float(f) => RawCell::Float(*f),
                        Data::Int(i) => RawCell::Int(*i),
                        Data::Bool(b) => RawCell::Text(b.to_string()),
                        Data::DateTime(dt) => {
                            let (y, m, d, h, min, s, milli) = dt.to_ymd_hms_milli();
                            let has_time = dt.as_f64().fract() != 0.0;
                            RawCell::DateTime {
                                year: y,
                                month: m,
                                day: d,
                                hour: h,
                                minute: min,
                                second: s,
                                millisecond: milli,
                                has_time,
                            }
                        }
                        Data::DateTimeIso(s) => RawCell::Text(s.clone()),
                        Data::DurationIso(s) => RawCell::Text(s.clone()),
                        Data::Error(e) => RawCell::Text(format!("{e:?}")),
                    };
                    cells.push(raw_cell);
                }
                rows.push(RawRow { row_number, cells });
            }
            sheets.push(RawSheet {
                name: Some(name),
                rows,
            });
        }
    }

    Ok(sheets)
}

/// Load a CSV file from memory into a single [`RawSheet`].
///
/// Sniffs `;` versus `,` delimiters from the first lines and strips any UTF-8 BOM.
pub fn load_csv(bytes: &[u8]) -> Result<Vec<RawSheet>, ImportError> {
    // Strip UTF-8 byte order mark if present.
    let bytes = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    };

    if bytes.is_empty() {
        return Err(ImportError::Unreadable("file is empty".to_string()));
    }

    // Sniff delimiter: count ';' vs ',' in sample bytes up to first 4KB.
    let sample_len = bytes.len().min(4096);
    let sample = &bytes[..sample_len];
    let semicolons = sample.iter().filter(|&&b| b == b';').count();
    let commas = sample.iter().filter(|&&b| b == b',').count();

    let delimiter = if semicolons >= commas && semicolons > 0 {
        b';'
    } else {
        b','
    };

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(bytes);

    let mut rows = Vec::new();

    for (row_idx, record_res) in reader.records().enumerate() {
        let row_number = (row_idx + 1) as u32;
        let record =
            record_res.map_err(|e| ImportError::Unreadable(format!("failed to parse CSV: {e}")))?;
        let mut cells = Vec::with_capacity(record.len());
        for field in record.iter() {
            let trimmed = field.trim();
            if trimmed.is_empty() {
                cells.push(RawCell::Empty);
            } else {
                cells.push(RawCell::Text(trimmed.to_string()));
            }
        }
        rows.push(RawRow { row_number, cells });
    }

    Ok(vec![RawSheet { name: None, rows }])
}
