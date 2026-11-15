//! Assembling the one track record for an account.

use super::*;

impl CapitalService {
    /// `GET /scorecard`: every evaluation as one track record ([`crate::scorecard`]).
    ///
    /// Gathers, for `account`: the baseline evaluation's scores ([`Self::evaluation`]), the
    /// signal skill ([`Self::signals`]), the outlook track record at every outlook horizon
    /// ([`Self::track_record`]), the latest trade run's scoreboard and the live record's
    /// totals (only when a card was ever published). An evaluation that can't run yet
    /// (no prices, no trade run) contributes nothing; any other error is returned.
    pub fn scorecard(
        &self,
        account: &AccountId,
    ) -> Result<crate::scorecard::Scorecard, ServiceError> {
        // An evaluation that can't run yet (no prices, no history) contributes nothing.
        // Zero samples is the same signal in every family: nothing to judge.
        let forecasts: Vec<crate::forecast::ModelScore> = match self.evaluation(account) {
            Ok(report) => report
                .scores
                .into_iter()
                .filter(|score| score.forecasts > 0)
                .collect(),
            Err(ServiceError::NotFound(_)) => Vec::new(),
            Err(error) => return Err(error),
        };
        let signals: Vec<crate::signals::SignalSkill> = match self.signals(account) {
            Ok(report) => report
                .skill
                .into_iter()
                .filter(|skill| skill.origins > 0)
                .collect(),
            Err(ServiceError::NotFound(_)) => Vec::new(),
            Err(error) => return Err(error),
        };
        let mut outlook = Vec::new();
        for horizon in crate::outlook::HORIZONS {
            match self.track_record(account, horizon) {
                Ok(record) if record.replays > 0 => outlook.push(record),
                Ok(_) => {}
                Err(ServiceError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let setups = match self.store.latest_trade_run(account)? {
            Some((_, day)) => day.scoreboard,
            None => Vec::new(),
        };
        let record = self.trade_record(account)?;
        let live = record
            .cards
            .iter()
            .any(|c| c.card.tier == crate::trade::Tier::Proven)
            .then_some(&record.totals);
        let calibration = self.store.dividend_calibration()?.map(|(_, r)| r);
        let dividend_record = self.dividend_record(account).ok();
        let inputs = crate::scorecard::ScorecardInputs {
            forecasts: &forecasts,
            signals: &signals,
            outlook: &outlook,
            setups: &setups,
            live,
            dividend_calibration: calibration.as_ref(),
            dividend_record: dividend_record.as_ref(),
        };
        Ok(crate::scorecard::summarize(&inputs, Utc::now()))
    }
}
