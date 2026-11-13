//! The owner's grants: units received without buying them, and their ledger rows.

use super::*;

impl CapitalService {
    /// The owner's grants, newest first, retracted ones included.
    pub fn grants(&self, account: &AccountId) -> Result<Vec<GrantView>, ServiceError> {
        Ok(self.store.grants(account)?)
    }

    /// Records units the owner received without buying them.
    pub fn record_grant(
        &self,
        account: &AccountId,
        input: GrantInput,
        now: DateTime<Utc>,
    ) -> Result<GrantView, ServiceError> {
        let symbol = input.symbol.trim().to_ascii_uppercase();
        let symbol_ok = !symbol.is_empty()
            && symbol.len() <= MAX_GRANT_SYMBOL_CHARS
            && symbol
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if !symbol_ok {
            return Err(ServiceError::BadRequest(
                "The symbol must be the broker's ticker, e.g. GALP.PT.".into(),
            ));
        }
        let kind = match input.kind.as_deref().unwrap_or("stock") {
            "stock" => InstrumentKind::Stock,
            "etf" => InstrumentKind::Etf,
            _ => {
                return Err(ServiceError::BadRequest(
                    "The kind must be \"stock\" or \"etf\".".into(),
                ));
            }
        };
        let quantity = parse_decimal_text(&input.quantity)
            .filter(|q| *q > rust_decimal::Decimal::ZERO)
            .ok_or_else(|| ServiceError::BadRequest("The quantity must be positive.".into()))?;
        let received_at = parse_received_at(&input.received_at).ok_or_else(|| {
            ServiceError::BadRequest(
                "The date received must be YYYY-MM-DD or an RFC 3339 time.".into(),
            )
        })?;
        if received_at > now {
            return Err(ServiceError::BadRequest(
                "The date received can't be in the future.".into(),
            ));
        }
        let cost = match input.cost {
            None => None,
            Some(cost) => {
                let amount = parse_decimal_text(&cost.amount)
                    .filter(|a| *a >= rust_decimal::Decimal::ZERO)
                    .ok_or_else(|| {
                        ServiceError::BadRequest("The cost must be zero or more.".into())
                    })?;
                let currency = Currency::parse(&cost.currency).ok_or_else(|| {
                    ServiceError::BadRequest("The cost needs a three-letter currency.".into())
                })?;
                Some(Money::new(amount, currency))
            }
        };
        let note = input
            .note
            .map(|n| clean_cell_text(&n).0)
            .map(|n| n.chars().take(MAX_GRANT_NOTE_CHARS).collect::<String>())
            .filter(|n| !n.is_empty());
        let id = sha256_hex(
            format!(
                "{}\n{symbol}\n{quantity}\n{}\n{}",
                account.as_str(),
                received_at.to_rfc3339(),
                now.to_rfc3339()
            )
            .as_bytes(),
        )[..16]
            .to_string();
        let grant = GrantView {
            id,
            symbol,
            kind,
            quantity,
            received_at,
            cost,
            note,
            recorded_at: now,
            retracted_at: None,
        };
        self.store.record_grant(account, &grant)?;
        Ok(grant)
    }

    /// Retracts a grant. Retracting twice is refused.
    pub fn retract_grant(
        &self,
        account: &AccountId,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<GrantView, ServiceError> {
        match self.store.retract_grant(account, id, now)? {
            RetractOutcome::Retracted(grant) => Ok(grant),
            RetractOutcome::AlreadyRetracted(_) => Err(ServiceError::BadRequest(
                "This grant was already retracted.".into(),
            )),
            RetractOutcome::NotFound => Err(ServiceError::NotFound(format!("No grant {id:?}."))),
        }
    }

}

/// Longest symbol a grant accepts.
const MAX_GRANT_SYMBOL_CHARS: usize = 32;

/// Longest note kept on a grant, in characters.
const MAX_GRANT_NOTE_CHARS: usize = 200;

/// What the owner sends to record a grant (`POST /grants`).
#[derive(Clone, Debug, Deserialize)]
pub struct GrantInput {
    /// Broker symbol, e.g. `"GALP.PT"`.
    pub symbol: String,
    /// `"stock"` (default) or `"etf"`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Units received, as a decimal string.
    pub quantity: String,
    /// `YYYY-MM-DD` (start of that day, UTC) or an RFC 3339 time.
    pub received_at: String,
    /// Cost basis; absent or null means zero.
    #[serde(default)]
    pub cost: Option<GrantCostInput>,
    /// A note, e.g. "promotional campaign".
    #[serde(default)]
    pub note: Option<String>,
}

/// A stated cost, with amounts as strings like everywhere else.
#[derive(Clone, Debug, Deserialize)]
pub struct GrantCostInput {
    /// Decimal amount.
    pub amount: String,
    /// Three-letter currency.
    pub currency: String,
}

/// Reads `YYYY-MM-DD` as the start of that UTC day, or an RFC 3339 time.
fn parse_received_at(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if let Ok(date) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return date.and_hms_opt(0, 0, 0).map(|t| t.and_utc());
    }
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// The ledger row a grant stands for. Its natural key sorts before XTB's on the same
/// instant, so units received at a time are held before a sale at that time.
pub(super) fn grant_record(account: &AccountId, grant: &GrantView) -> LedgerRecord {
    let date_only = grant.received_at.time() == chrono::NaiveTime::MIN;
    LedgerRecord {
        id: format!("grant-{}", grant.id),
        account: account.clone(),
        batch_id: "owner-grants".to_string(),
        ingested_at: grant.recorded_at,
        entry: ParsedEntry {
            natural_key: format!("owner:grant:{}", grant.id),
            source: RowRef {
                sheet: None,
                sheet_kind: SheetKind::Unknown,
                row: 0,
            },
            broker_ref: None,
            occurred: EventTime {
                at: grant.received_at,
                precision: if date_only {
                    TimePrecision::DateOnly
                } else {
                    TimePrecision::Exact
                },
            },
            kind: EntryKind::Grant {
                instrument: Instrument {
                    symbol: grant.symbol.clone(),
                    isin: None,
                    name: None,
                    currency: None,
                    kind: grant.kind,
                },
                quantity: grant.quantity,
                cost: grant.cost.clone(),
            },
            cash_effect: None,
            comment: grant.note.clone(),
        },
    }
}
