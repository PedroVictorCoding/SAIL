//! Broker report importers. Each turns an uploaded file into a [`ParsedReport`]; none of
//! them touches storage, the network or the clock.
use crate::contracts::{Broker, ImportError, ImportOptions, ParsedReport};

pub mod xtb;

/// Read one report file for `broker`. `file_name` is used only to tell XLSX from CSV.
pub fn parse(
    broker: Broker,
    bytes: &[u8],
    file_name: &str,
    options: &ImportOptions,
) -> Result<ParsedReport, ImportError> {
    match broker {
        Broker::Xtb => xtb::parse(bytes, file_name, options),
        Broker::Manual => Err(ImportError::Unreadable(
            "manual entries are not imported from files".to_string(),
        )),
    }
}
