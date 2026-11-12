//! The initiative loop: observations → agenda + signals → Pulse attention.
//!
//! One [`Initiative::tick`] is cheap and deterministic: no model is called, and a tick with
//! nothing new does no writes beyond dedup lookups. The caller runs it on a timer (minutes,
//! not seconds) and on events, gathers observations from stores it already owns, and
//! builds the [`AttentionContext`] from Pulse settings and the resource scheduler.
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sail_pulse::attention::{AttentionContext, Delivery, Provenance, store::AttentionStore};
use serde::{Deserialize, Serialize};

use crate::agenda::{AgendaKind, AgendaStore, NewAgendaItem};
use crate::detect::{self, Observation, excerpt};

/// The brief added to a conversation prompt is at most this many characters.
pub const MAX_BRIEF_CHARS: usize = 700;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TickReport {
    pub observations: usize,
    pub signals: usize,
    pub interrupt: usize,
    pub suggest: usize,
    pub digest: usize,
    pub silent: usize,
    pub dropped: usize,
    pub agenda_updates: usize,
    pub revisits: usize,
    pub errors: Vec<String>,
}

pub struct Initiative {
    attention: Arc<AttentionStore>,
    agenda: Arc<AgendaStore>,
}

impl Initiative {
    pub fn new(attention: Arc<AttentionStore>, agenda: Arc<AgendaStore>) -> Self {
        Self { attention, agenda }
    }

    pub fn attention(&self) -> &Arc<AttentionStore> {
        &self.attention
    }

    pub fn agenda(&self) -> &Arc<AgendaStore> {
        &self.agenda
    }

    pub fn tick(&self, observations: &[Observation], context: AttentionContext) -> TickReport {
        let now = context.now;
        let mut report = TickReport {
            observations: observations.len(),
            ..TickReport::default()
        };
        let mut all = observations.to_vec();
        for observation in observations {
            if let Err(error) = self.record_agenda(observation, now, &mut report) {
                report.errors.push(error);
            }
            // New evidence may answer an open question.
            if let Some(evidence) = evidence_text(observation) {
                match self.agenda.match_evidence(&evidence, now) {
                    Ok(items) => {
                        for item in items {
                            report.revisits += 1;
                            all.push(Observation::AgendaRevisit {
                                item_id: item.id,
                                title: item.title,
                                evidence: evidence.clone(),
                            });
                        }
                    }
                    Err(error) => report.errors.push(error.to_string()),
                }
            }
        }
        for observation in &all {
            for signal in detect::detect(observation, now) {
                report.signals += 1;
                match self.attention.ingest(signal, context.clone()) {
                    Ok(item) => match item.assessment.delivery {
                        Delivery::Interrupt => report.interrupt += 1,
                        Delivery::Suggest => report.suggest += 1,
                        Delivery::Digest => report.digest += 1,
                        Delivery::Silent => report.silent += 1,
                        Delivery::Drop(_) => report.dropped += 1,
                    },
                    Err(error) => report.errors.push(error.to_string()),
                }
            }
        }
        if report.signals > 0 || !report.errors.is_empty() {
            tracing::info!(?report, "initiative tick");
        }
        report
    }

    /// A short block for the conversation prompt: commitments due within three days and
    /// the top suggestions SAIL is holding. Empty when there is nothing worth raising.
    /// The model is told these are optional and must not be forced into the reply.
    pub fn conversation_brief(&self, now: DateTime<Utc>) -> String {
        let mut lines = Vec::new();
        if let Ok(items) = self.agenda.open_items(20) {
            for item in items.iter().filter(|i| {
                i.kind == AgendaKind::Commitment
                    && i.due
                        .is_some_and(|d| d > now && d - now <= Duration::days(3))
            }) {
                lines.push(format!("- due soon: {}", excerpt(&item.title, 100)));
                if lines.len() == 2 {
                    break;
                }
            }
        }
        if let Ok(items) = self.attention.suggestions(3) {
            for item in items {
                let tag = match item.signal.provenance {
                    Provenance::Hypothesis => "idea (unverified)",
                    Provenance::Suggestion => "suggestion",
                    Provenance::UserCommitment => "commitment",
                    Provenance::Observation => "noticed",
                };
                lines.push(format!("- {tag}: {}", excerpt(&item.signal.title, 110)));
            }
        }
        if lines.is_empty() {
            return String::new();
        }
        let mut brief = String::from(
            "Things SAIL could raise if it fits the conversation naturally (optional; never force them):\n",
        );
        for line in lines {
            if brief.chars().count() + line.chars().count() + 1 > MAX_BRIEF_CHARS {
                break;
            }
            brief.push_str(&line);
            brief.push('\n');
        }
        brief
    }

    fn record_agenda(
        &self,
        observation: &Observation,
        now: DateTime<Utc>,
        report: &mut TickReport,
    ) -> Result<(), String> {
        if let Observation::SchoolItem {
            id,
            course,
            title,
            due,
            ..
        } = observation
            && *due > now
            && *due - now <= Duration::days(detect::SCHOOL_HORIZON_DAYS)
        {
            self.agenda
                .upsert(
                    NewAgendaItem {
                        key: format!("moodle:item:{id}"),
                        kind: AgendaKind::Commitment,
                        title: format!("{course}: {title}"),
                        detail: String::new(),
                        provenance: Provenance::UserCommitment,
                        match_terms: vec![],
                        due: Some(*due),
                    },
                    now,
                )
                .map_err(|e| e.to_string())?;
            report.agenda_updates += 1;
        }
        Ok(())
    }
}

fn evidence_text(observation: &Observation) -> Option<String> {
    match observation {
        Observation::SchoolChange { course, title, .. } => Some(format!("{course} {title}")),
        Observation::EscalationAnswered { question, .. } => Some(question.clone()),
        _ => None,
    }
}
