//! Conservative deterministic consolidation. Assistant prose is never treated as evidence.
//! Unsupported utterances remain in the evidence buffer for later processing/retention.
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::*;
use crate::store::decode_body;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub entity_id: Option<i64>,
    pub name: String,
    pub kind: EntityKind,
    pub predicate: String,
    pub value: String,
    pub source_ref: String,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ConsolidationReport {
    pub released: usize,
    pub retained: usize,
    pub claims: Vec<i64>,
}

pub(crate) struct Candidate {
    pub(crate) name: String,
    pub(crate) kind: EntityKind,
    pub(crate) entity_id: Option<i64>,
    pub(crate) predicate: String,
    pub(crate) value: String,
    pub(crate) category: ClaimCategory,
    pub(crate) correction: bool,
    pub(crate) direct: bool,
    pub(crate) source_ref: Option<String>,
}

/// The user side of an archived turn in our own `User: …\nSAIL: …` format. Assistant prose
/// is never returned, so it can never become evidence.
pub(crate) fn user_side(body: &str) -> Option<&str> {
    if body.matches("\nSAIL: ").count() != 1 {
        return None;
    }
    Some(body.strip_prefix("User: ")?.split_once("\nSAIL: ")?.0)
}

fn extract(kind: &str, body: &str) -> Option<Candidate> {
    if kind == "tool_observation" {
        let o: Observation = serde_json::from_str(body).ok()?;
        if o.source_ref.trim().is_empty() {
            return None;
        }
        return Some(Candidate {
            name: o.name,
            kind: o.kind,
            entity_id: o.entity_id,
            predicate: o.predicate,
            value: o.value,
            category: ClaimCategory::Fact,
            correction: true,
            direct: true,
            source_ref: Some(o.source_ref),
        });
    }
    if kind != "turn" {
        return None;
    }
    let user = user_side(body)?.trim();
    // A deliberately narrow whole-utterance grammar avoids silently dropping unparsed facts.
    if user.contains(['\n', '\r']) || user.len() > 2000 {
        return None;
    }
    let (name, predicate, value, category, correction) =
        if let Some(value) = user.strip_prefix("I prefer ") {
            ("Owner", "prefers", value, ClaimCategory::Preference, false)
        } else if let Some(value) = user.strip_prefix("I now prefer ") {
            ("Owner", "prefers", value, ClaimCategory::Preference, true)
        } else {
            let (text, correction, category) = if let Some(text) = user.strip_prefix("Remember: ") {
                (text, false, ClaimCategory::Fact)
            } else if let Some(text) = user.strip_prefix("Correction: ") {
                (text, true, ClaimCategory::Fact)
            } else if let Some(text) = user.strip_prefix("Decision: ") {
                (text, false, ClaimCategory::Event)
            } else {
                return None;
            };
            let (name, value) = text.split_once(" is ")?;
            (
                name,
                if category == ClaimCategory::Event {
                    "decision"
                } else {
                    "is"
                },
                value,
                category,
                correction,
            )
        };
    if name.trim().is_empty() || value.trim().is_empty() {
        return None;
    }
    Some(Candidate {
        name: name.trim().into(),
        kind: if name == "Owner" {
            EntityKind::Person
        } else {
            EntityKind::Concept
        },
        entity_id: None,
        predicate: predicate.into(),
        value: value.trim().trim_end_matches('.').into(),
        category,
        correction,
        direct: false,
        source_ref: None,
    })
}

impl KnowledgeStore<'_> {
    /// Consolidate at most `limit` records from one authorized source scope, atomically.
    /// The generation belongs to the job that requested this work, not the current time.
    pub fn consolidate(
        &self,
        scope: &str,
        access: &Access,
        generation: i64,
        limit: usize,
    ) -> KnowledgeResult<ConsolidationReport> {
        self.consolidate_sources(scope, access, generation, limit, None)
    }

    pub(crate) fn consolidate_sources(
        &self,
        scope: &str,
        access: &Access,
        generation: i64,
        limit: usize,
        sources: Option<&str>,
    ) -> KnowledgeResult<ConsolidationReport> {
        if !access.allows(scope, Sensitivity::Private) {
            return Err(KnowledgeError::Invalid(
                "consolidation requires private access to the source scope".into(),
            ));
        }
        self.write_at_generation(generation, Actor::subsystem("consolidation"), |tx| {
            let rows = {
                let mut stmt = tx.conn.prepare("SELECT id,kind,codec,body,created_at FROM records WHERE scope=?1 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM json_each(?3) WHERE json_extract(value,'$.id')=records.id AND json_extract(value,'$.hash')=hex(records.hash) AND json_extract(value,'$.created_at')=records.created_at)) ORDER BY id LIMIT ?2")?;
                stmt.query_map(params![scope, limit.min(256) as i64, sources], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,Vec<u8>>(3)?,r.get::<_,i64>(4)?)))?.collect::<Result<Vec<_>,_>>()?
            };
            let mut report = ConsolidationReport::default();
            for (id, kind, codec, blob, observed_at) in rows {
                let body = decode_body(codec, &blob);
                let Some(c) = extract(&kind, &body) else { report.retained += 1; continue; };
                let source_ref = c.source_ref.clone().unwrap_or_else(||format!("archive:{id}"));
                let evidence = tx.add_evidence(&NewEvidence { source_kind:if c.direct {SourceKind::ToolObservation} else {SourceKind::UserStatement}, source_ref:Some(&source_ref), observed_at,
                    content:if c.direct {&body} else {body.strip_prefix("User: ").and_then(|b| b.split_once("\nSAIL: ")).map(|p|p.0).unwrap_or("")}, receipt:Some(&c.value), derived_from:None, verification:None, scope })?;
                let claim = apply_candidate(tx, access, scope, &c, evidence, observed_at, if c.direct {EpistemicStatus::Verified} else {EpistemicStatus::UserReported})?;
                tx.conn.execute("DELETE FROM records_fts WHERE rowid=?1",[id])?;
                tx.conn.execute("DELETE FROM records WHERE id=?1",[id])?;
                report.claims.push(claim); report.released += 1;
            }
            Ok(report)
        })
    }
}

/// Resolve the subject, assert the claim and reconcile it with existing values. Shared by
/// deterministic consolidation and validated model change sets so both follow one rule set.
pub(crate) fn apply_candidate(
    tx: &mut KnowledgeTx<'_>,
    access: &Access,
    scope: &str,
    c: &Candidate,
    evidence: i64,
    observed_at: i64,
    status: EpistemicStatus,
) -> KnowledgeResult<i64> {
    let matches: Vec<i64> = {
        let mut stmt = tx.conn.prepare("SELECT DISTINCT e.id FROM kn_entities e LEFT JOIN kn_aliases a ON a.entity_id=e.id WHERE e.status='active' AND e.scope=?1 AND e.kind=?2 AND e.sensitivity<=?4 AND (e.name_key=?3 OR a.alias_key=?3) ORDER BY e.id")?;
        stmt.query_map(
            params![
                scope,
                c.kind.as_str(),
                name_key(&c.name),
                access.max_sensitivity as i64
            ],
            |r| r.get(0),
        )?
        .collect::<Result<_, _>>()?
    };
    let subject = if let Some(id) = c.entity_id {
        let valid = tx.conn.query_row("SELECT id FROM kn_entities WHERE id=?1 AND scope=?2 AND sensitivity<=?3 AND status='active'",params![id,scope,access.max_sensitivity as i64],|r|r.get::<_,i64>(0)).optional()?;
        valid.ok_or(KnowledgeError::NotFound("entity", id))?
    } else if matches.len() == 1 {
        matches[0]
    } else {
        let entity = tx.create_entity(&NewEntity {
            kind: c.kind,
            name: &c.name,
            aliases: &[],
            scope,
            sensitivity: Sensitivity::Private,
        })?;
        for duplicate in matches {
            tx.assert_claim(
                &NewClaim {
                    subject: entity,
                    predicate: "possible_duplicate_of",
                    object: ClaimObject::Entity(duplicate),
                    category: ClaimCategory::Fact,
                    basis: None,
                    status: EpistemicStatus::Unverified,
                    inference_confidence: None,
                    observed_at,
                    valid_from: None,
                    valid_until: None,
                    retention: Retention::Durable,
                    scope,
                    sensitivity: Sensitivity::Private,
                },
                &[(evidence, Stance::Supports)],
            )?;
        }
        entity
    };
    let previous: Vec<(i64, String, i64)> = {
        let mut stmt = tx.conn.prepare("SELECT id,object_value,observed_at FROM kn_claims WHERE subject_id=?1 AND predicate=?2 AND scope=?3 AND object_value IS NOT NULL AND status NOT IN ('superseded','invalidated')")?;
        stmt.query_map(params![subject, c.predicate, scope], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?
    };
    let claim = tx.assert_claim(
        &NewClaim {
            subject,
            predicate: &c.predicate,
            object: ClaimObject::Value(c.value.clone()),
            category: c.category,
            basis: (c.category == ClaimCategory::Preference).then_some(PreferenceBasis::Explicit),
            status,
            inference_confidence: None,
            observed_at,
            valid_from: None,
            valid_until: None,
            retention: Retention::Durable,
            scope,
            sensitivity: Sensitivity::Private,
        },
        &[(evidence, Stance::Supports)],
    )?;
    for (old, value, time) in previous {
        if old == claim || value == c.value {
            continue;
        }
        if c.correction && time <= observed_at {
            tx.supersede(old, claim, "explicit correction or newer tool observation")?;
        } else if c.category != ClaimCategory::Preference && c.category != ClaimCategory::Event {
            tx.link_evidence(old, evidence, Stance::Refutes)?;
            tx.set_status(
                old,
                EpistemicStatus::Disputed,
                Some("conflicting observation"),
            )?;
            // Both alternatives remain visible; neither is silently promoted.
            let old_evidence: i64 = tx.conn.query_row("SELECT evidence_id FROM kn_claim_evidence WHERE claim_id=?1 AND stance='supports' LIMIT 1",[old],|r|r.get(0))?;
            tx.link_evidence(claim, old_evidence, Stance::Refutes)?;
            tx.set_status(
                claim,
                EpistemicStatus::Disputed,
                Some("conflicting observation"),
            )?;
        }
    }
    Ok(claim)
}
