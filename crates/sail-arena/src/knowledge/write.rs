use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;

use super::model::*;
use super::{KnowledgeError, KnowledgeResult};
use crate::store::{now_ms, sha16};

const MAX_NAME_CHARS: usize = 200;
const MAX_VALUE_CHARS: usize = 2_000;

/// Mutations inside one knowledge transaction. Obtained from
/// [`KnowledgeStore::write`](super::KnowledgeStore::write).
pub struct KnowledgeTx<'t> {
    pub(crate) conn: &'t Connection,
    actor: Actor,
    now: i64,
}

pub(crate) fn generation(conn: &Connection) -> KnowledgeResult<i64> {
    Ok(conn.query_row(
        "SELECT value FROM kn_meta WHERE key='deletion_generation'",
        [],
        |r| r.get(0),
    )?)
}

fn invalid(msg: impl Into<String>) -> KnowledgeError {
    KnowledgeError::Invalid(msg.into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fingerprint(kind: EntityKind, subject_key: &str, predicate: &str, object_key: &str) -> [u8; 16] {
    sha16(&[
        b"claim",
        kind.as_str().as_bytes(),
        subject_key.as_bytes(),
        predicate.as_bytes(),
        object_key.as_bytes(),
    ])
}

struct EntityRow {
    kind: EntityKind,
    name_key: String,
    active: bool,
}

struct ClaimRow {
    subject: i64,
    predicate: String,
    status: EpistemicStatus,
    observed_at: i64,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    fingerprint: Vec<u8>,
}

impl<'t> KnowledgeTx<'t> {
    /// Atomically erase pending source text and invalidate proposals already reading it.
    /// Previously consolidated claims remain until explicitly forgotten by the owner.
    pub fn discard_session_sources(&self, scope: &str, session: &str) -> KnowledgeResult<()> {
        if !matches!(self.actor, Actor::User) {
            return Err(invalid("owner action required"));
        }
        self.bump_generation()?;
        // A generation change obsoletes shared-scope jobs too. Re-admit their surviving
        // sources, otherwise their processed markers would silently strand other sessions.
        self.conn.execute(
            "DELETE FROM kn_source_progress WHERE extractor_version=1 AND record_id IN (
                SELECT r.id FROM kn_jobs j, json_each(j.sources) s JOIN records r
                  ON r.id=json_extract(s.value,'$.id')
                 AND hex(r.hash)=json_extract(s.value,'$.hash')
                 AND r.created_at=json_extract(s.value,'$.created_at')
                WHERE j.state IN ('pending','running'))",
            [],
        )?;
        self.conn.execute("DELETE FROM records_fts WHERE rowid IN (SELECT id FROM records WHERE scope=?1 AND session=?2)", params![scope, session])?;
        self.conn.execute(
            "DELETE FROM records WHERE scope=?1 AND session=?2",
            params![scope, session],
        )?;
        self.log(
            "discard_sources",
            "session",
            None,
            Some("owner disabled conversation learning"),
            json!({"scope":scope, "session":session}),
        )
    }

    pub(crate) fn new(conn: &'t Connection, actor: Actor) -> Self {
        Self {
            conn,
            actor,
            now: now_ms(),
        }
    }

    fn log(
        &self,
        op: &str,
        target_kind: &str,
        target_id: Option<i64>,
        reason: Option<&str>,
        detail: serde_json::Value,
    ) -> KnowledgeResult<()> {
        self.conn.execute(
            "INSERT INTO kn_operations(at,actor,op,target_kind,target_id,reason,detail)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                self.now,
                self.actor.label(),
                op,
                target_kind,
                target_id,
                reason,
                (!detail.is_null()).then(|| detail.to_string()),
            ],
        )?;
        Ok(())
    }

    fn entity_row(&self, id: i64) -> KnowledgeResult<EntityRow> {
        self.conn
            .query_row(
                "SELECT kind,name_key,status FROM kn_entities WHERE id=?1",
                [id],
                |r| {
                    Ok(EntityRow {
                        kind: EntityKind::parse(&r.get::<_, String>(0)?)
                            .unwrap_or(EntityKind::Other),
                        name_key: r.get(1)?,
                        active: r.get::<_, String>(2)? == "active",
                    })
                },
            )
            .optional()?
            .ok_or(KnowledgeError::NotFound("entity", id))
    }

    fn active_entity(&self, id: i64) -> KnowledgeResult<EntityRow> {
        let row = self.entity_row(id)?;
        if !row.active {
            return Err(invalid(format!(
                "entity {id} was merged; use its canonical entity"
            )));
        }
        Ok(row)
    }

    fn claim_row(&self, id: i64) -> KnowledgeResult<ClaimRow> {
        self.conn
            .query_row(
                "SELECT subject_id,predicate,status,observed_at,valid_from,valid_until,fingerprint
                 FROM kn_claims WHERE id=?1",
                [id],
                |r| {
                    Ok(ClaimRow {
                        subject: r.get(0)?,
                        predicate: r.get(1)?,
                        status: EpistemicStatus::parse(&r.get::<_, String>(2)?)
                            .unwrap_or(EpistemicStatus::Unverified),
                        observed_at: r.get(3)?,
                        valid_from: r.get(4)?,
                        valid_until: r.get(5)?,
                        fingerprint: r.get(6)?,
                    })
                },
            )
            .optional()?
            .ok_or(KnowledgeError::NotFound("claim", id))
    }

    fn evidence_kind(&self, id: i64) -> KnowledgeResult<SourceKind> {
        let kind: Option<String> = self
            .conn
            .query_row(
                "SELECT source_kind FROM kn_evidence WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        kind.and_then(|k| SourceKind::parse(&k))
            .ok_or(KnowledgeError::NotFound("evidence", id))
    }

    /// Fingerprint of a claim's content, independent of row ids, so a deleted claim stays
    /// recognisable if a stale job tries to re-insert it under new ids.
    fn claim_fingerprint(
        &self,
        subject: i64,
        predicate: &str,
        object: &ClaimObject,
    ) -> KnowledgeResult<[u8; 16]> {
        let s = self.entity_row(subject)?;
        let object_key = match object {
            ClaimObject::Entity(id) => {
                let o = self.entity_row(*id)?;
                format!("e:{}:{}", o.kind.as_str(), o.name_key)
            }
            ClaimObject::Value(v) => format!("v:{}", name_key(v)),
        };
        Ok(fingerprint(s.kind, &s.name_key, predicate, &object_key))
    }

    /// Every name (canonical and alias) an entity is known by, as `(kind, keys)`.
    fn entity_keys(&self, id: i64) -> KnowledgeResult<(EntityKind, Vec<String>)> {
        let row = self.entity_row(id)?;
        let mut stmt = self
            .conn
            .prepare("SELECT alias_key FROM kn_aliases WHERE entity_id=?1")?;
        let mut keys: Vec<String> = stmt
            .query_map([id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        keys.push(row.name_key);
        keys.sort_unstable();
        keys.dedup();
        Ok((row.kind, keys))
    }

    /// Tombstone a claim's content under every name its subject and object are known by,
    /// so it cannot return through an alias or a pre-merge name either.
    fn tombstone_claim(&self, claim: i64, generation: i64) -> KnowledgeResult<()> {
        let (subject, predicate, object, value): (i64, String, Option<i64>, Option<String>) =
            self.conn.query_row(
                "SELECT subject_id,predicate,object_entity_id,object_value FROM kn_claims WHERE id=?1",
                [claim],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        let (s_kind, s_keys) = self.entity_keys(subject)?;
        let object_keys = match (object, value) {
            (Some(o), _) => {
                let (kind, keys) = self.entity_keys(o)?;
                keys.into_iter()
                    .map(|k| format!("e:{}:{k}", kind.as_str()))
                    .collect()
            }
            (None, v) => vec![format!("v:{}", name_key(&v.unwrap_or_default()))],
        };
        for s_key in &s_keys {
            for o_key in &object_keys {
                self.tombstone(&fingerprint(s_kind, s_key, &predicate, o_key), generation)?;
            }
        }
        Ok(())
    }

    /// Fail on a tombstoned fingerprint, unless the user is explicitly re-teaching it, in
    /// which case the tombstone is lifted.
    fn check_tombstone(&self, fingerprint: &[u8]) -> KnowledgeResult<()> {
        let dead: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM kn_tombstones WHERE fingerprint=?1)",
            [fingerprint],
            |r| r.get(0),
        )?;
        if !dead {
            return Ok(());
        }
        if self.actor != Actor::User {
            return Err(KnowledgeError::Tombstoned);
        }
        self.conn.execute(
            "DELETE FROM kn_tombstones WHERE fingerprint=?1",
            [fingerprint],
        )?;
        self.log(
            "lift_tombstone",
            "tombstone",
            None,
            None,
            json!({"fingerprint": hex(fingerprint)}),
        )
    }

    fn tombstone(&self, fingerprint: &[u8], generation: i64) -> KnowledgeResult<()> {
        self.conn.execute(
            "INSERT INTO kn_tombstones(fingerprint,deleted_at,generation) VALUES(?1,?2,?3)
             ON CONFLICT(fingerprint) DO UPDATE SET deleted_at=excluded.deleted_at, generation=excluded.generation",
            params![fingerprint, self.now, generation],
        )?;
        Ok(())
    }

    fn bump_generation(&self) -> KnowledgeResult<i64> {
        Ok(self.conn.query_row(
            "UPDATE kn_meta SET value=value+1 WHERE key='deletion_generation' RETURNING value",
            [],
            |r| r.get(0),
        )?)
    }

    /// Delete those of `candidates` no longer referenced by any claim, run or project item.
    /// Receipts can hold excerpts, so they must not outlive the knowledge they supported.
    /// Only previously linked evidence is considered, never evidence a caller added earlier
    /// in this transaction and has not linked yet.
    fn purge_orphan_evidence(&self, candidates: &[i64]) -> KnowledgeResult<usize> {
        let mut purged = 0;
        for id in candidates {
            purged += self.conn.execute(
                "DELETE FROM kn_evidence WHERE id=?1
                 AND NOT EXISTS(SELECT 1 FROM kn_claim_evidence WHERE evidence_id=?1)
                 AND NOT EXISTS(SELECT 1 FROM kn_procedure_runs WHERE evidence_id=?1)
                 AND NOT EXISTS(SELECT 1 FROM kn_project_items WHERE state_evidence_id=?1)",
                [id],
            )?;
        }
        Ok(purged)
    }

    fn ids(&self, sql: &str, param: i64) -> KnowledgeResult<Vec<i64>> {
        let mut stmt = self.conn.prepare(sql)?;
        Ok(stmt
            .query_map([param], |r| r.get(0))?
            .collect::<Result<_, _>>()?)
    }

    // ---- entities -------------------------------------------------------------------

    /// Create a new entity. Never merges with an existing one: resolution is the caller's
    /// decision, made with [`find_entities`](super::KnowledgeStore::find_entities) first.
    pub fn create_entity(&mut self, e: &NewEntity<'_>) -> KnowledgeResult<i64> {
        let name = e.name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
            return Err(invalid("entity name must be 1–200 characters"));
        }
        if e.scope.trim().is_empty() {
            return Err(invalid("entity scope is required"));
        }
        // Names are not tombstoned: a deleted "Alex" must not block a different Alex.
        // Forgotten claims are, and stale jobs are stopped by the deletion generation.
        let key = name_key(name);
        self.conn.execute(
            "INSERT INTO kn_entities(kind,name,name_key,scope,sensitivity,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?6)",
            params![
                e.kind.as_str(),
                name,
                key,
                e.scope,
                e.sensitivity as i64,
                self.now
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        for alias in e.aliases {
            self.add_alias(id, alias)?;
        }
        self.log("create", "entity", Some(id), None, json!({"kind": e.kind}))?;
        Ok(id)
    }

    pub fn add_alias(&mut self, entity: i64, alias: &str) -> KnowledgeResult<()> {
        let alias = alias.trim();
        if alias.is_empty() || alias.chars().count() > MAX_NAME_CHARS {
            return Err(invalid("alias must be 1–200 characters"));
        }
        self.active_entity(entity)?;
        self.conn.execute(
            "INSERT OR IGNORE INTO kn_aliases(entity_id,alias_key,alias) VALUES(?1,?2,?3)",
            params![entity, name_key(alias), alias],
        )?;
        self.log(
            "alias",
            "entity",
            Some(entity),
            None,
            json!({"changed":true}),
        )
    }

    /// Merge `from` into `into` (same kind). `from` stays as a `merged` stub pointing at
    /// `into`, its names become aliases, and every claim, project item and procedure moves.
    pub fn merge_entities(&mut self, from: i64, into: i64, reason: &str) -> KnowledgeResult<()> {
        if from == into {
            return Err(invalid("cannot merge an entity into itself"));
        }
        let (a, b) = (self.active_entity(from)?, self.active_entity(into)?);
        if a.kind != b.kind {
            return Err(invalid("only entities of the same kind can be merged"));
        }
        let compatible:bool=self.conn.query_row("SELECT a.scope=b.scope AND a.sensitivity=b.sensitivity FROM kn_entities a,kn_entities b WHERE a.id=?1 AND b.id=?2",[from,into],|r|r.get(0))?;
        if !compatible {
            return Err(invalid("merge requires the same scope and privacy level"));
        }
        let both_have_procedures: bool = self.conn.query_row(
            "SELECT (SELECT count(*) FROM kn_procedures WHERE entity_id=?1) > 0
                AND (SELECT count(*) FROM kn_procedures WHERE entity_id=?2) > 0",
            [from, into],
            |r| r.get(0),
        )?;
        if both_have_procedures {
            return Err(invalid(
                "both entities have procedure versions; reconcile them first",
            ));
        }
        let ids = |sql: &str| -> KnowledgeResult<Vec<i64>> {
            let mut stmt = self.conn.prepare(sql)?;
            Ok(stmt
                .query_map([from], |r| r.get(0))?
                .collect::<Result<_, _>>()?)
        };
        let aliases: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT alias FROM kn_aliases WHERE entity_id=?1")?;
            stmt.query_map([from], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        let target_aliases: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT alias FROM kn_aliases WHERE entity_id=?1")?;
            stmt.query_map([into], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        let snapshot = json!({"subjects":ids("SELECT id FROM kn_claims WHERE subject_id=?1")?,
            "objects":ids("SELECT id FROM kn_claims WHERE object_entity_id=?1")?,
            "projects":ids("SELECT id FROM kn_project_items WHERE project_id=?1")?,
            "procedures":ids("SELECT id FROM kn_procedures WHERE entity_id=?1")?,
            "aliases":aliases,"target_aliases":target_aliases});
        self.conn.execute(
            "INSERT OR IGNORE INTO kn_aliases(entity_id,alias_key,alias)
             SELECT ?2, name_key, name FROM kn_entities WHERE id=?1
             UNION ALL SELECT ?2, alias_key, alias FROM kn_aliases WHERE entity_id=?1",
            [from, into],
        )?;
        self.conn
            .execute("DELETE FROM kn_aliases WHERE entity_id=?1", [from])?;
        self.conn.execute(
            "UPDATE kn_claims SET subject_id=?2 WHERE subject_id=?1",
            [from, into],
        )?;
        self.conn.execute(
            "UPDATE kn_claims SET object_entity_id=?2 WHERE object_entity_id=?1",
            [from, into],
        )?;
        self.conn.execute(
            "UPDATE kn_project_items SET project_id=?2 WHERE project_id=?1",
            [from, into],
        )?;
        self.conn.execute(
            "UPDATE kn_procedures SET entity_id=?2 WHERE entity_id=?1",
            [from, into],
        )?;
        self.conn.execute(
            "UPDATE kn_entities SET status='merged', merged_into=?2, updated_at=?3 WHERE id=?1",
            params![from, into, self.now],
        )?;
        // Moved claims now describe `into`; refresh their content fingerprints.
        let moved: Vec<(i64, i64, String, ClaimObject)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id,subject_id,predicate,object_entity_id,object_value FROM kn_claims
                 WHERE subject_id=?1 OR object_entity_id=?1",
            )?;
            stmt.query_map([into], |r| {
                let object = match r.get::<_, Option<i64>>(3)? {
                    Some(o) => ClaimObject::Entity(o),
                    None => ClaimObject::Value(r.get(4)?),
                };
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, object))
            })?
            .collect::<Result<_, _>>()?
        };
        for (id, subject, predicate, object) in moved {
            let fp = self.claim_fingerprint(subject, &predicate, &object)?;
            self.conn.execute(
                "UPDATE kn_claims SET fingerprint=?2 WHERE id=?1",
                params![id, &fp[..]],
            )?;
        }
        self.log(
            "merge",
            "entity",
            Some(into),
            Some(reason),
            json!({"from":from}),
        )?;
        let operation_id: i64 =
            self.conn
                .query_row("SELECT max(id) FROM kn_operations", [], |r| r.get(0))?;
        self.conn.execute("INSERT OR REPLACE INTO kn_merge_snapshots(from_id,into_id,operation_id,snapshot) VALUES(?1,?2,?3,?4)",params![from,into,operation_id,snapshot.to_string()])?;
        Ok(())
    }

    /// Forget an entity, every claim about or pointing at it, its project items and
    /// procedures, and evidence left unreferenced. Merged stubs pointing at it go too.
    pub fn delete_entity(&mut self, id: i64, reason: &str) -> KnowledgeResult<()> {
        self.entity_row(id)?;
        let mut doomed = vec![id];
        let mut i = 0;
        while i < doomed.len() {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM kn_entities WHERE merged_into=?1")?;
            let more: Vec<i64> = stmt
                .query_map([doomed[i]], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            doomed.extend(more);
            i += 1;
        }
        let mut evidence = Vec::new();
        for &e in &doomed {
            evidence.extend(self.ids(
                "SELECT ce.evidence_id FROM kn_claim_evidence ce JOIN kn_claims c ON c.id=ce.claim_id
                 WHERE c.subject_id=?1 OR c.object_entity_id=?1",
                e,
            )?);
            evidence.extend(self.ids(
                "SELECT state_evidence_id FROM kn_project_items WHERE project_id=?1 AND state_evidence_id IS NOT NULL",
                e,
            )?);
            evidence.extend(self.ids(
                "SELECT r.evidence_id FROM kn_procedure_runs r JOIN kn_procedures p ON p.id=r.procedure_id WHERE p.entity_id=?1",
                e,
            )?);
        }
        evidence.sort_unstable();
        evidence.dedup();
        let generation = self.bump_generation()?;
        for &e in &doomed {
            for claim in self.ids(
                "SELECT id FROM kn_claims WHERE subject_id=?1 OR object_entity_id=?1",
                e,
            )? {
                self.tombstone_claim(claim, generation)?;
            }
        }
        // Stubs first: deleting a merge target would otherwise null `merged_into` on them.
        for &e in doomed.iter().rev() {
            self.conn
                .execute("DELETE FROM kn_entities WHERE id=?1", [e])?;
        }
        let purged = self.purge_orphan_evidence(&evidence)?;
        self.log(
            "delete",
            "entity",
            Some(id),
            Some(reason),
            json!({"entities": doomed.len(), "evidence_purged": purged, "generation": generation}),
        )
    }

    // ---- evidence -------------------------------------------------------------------

    /// Record evidence and return its id. Only a hash of `content` and a bounded receipt are
    /// kept. The same content from the same source kind in the same scope is stored once.
    pub fn add_evidence(&mut self, e: &NewEvidence<'_>) -> KnowledgeResult<i64> {
        if e.scope.trim().is_empty() {
            return Err(invalid("evidence scope is required"));
        }
        let content_hash = sha16(&[e.content.as_bytes()]);
        let origin: Vec<u8> = match e.derived_from {
            Some(parent) => self
                .conn
                .query_row(
                    "SELECT origin_hash FROM kn_evidence WHERE id=?1",
                    [parent],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or(KnowledgeError::NotFound("evidence", parent))?,
            None if e.source_kind == SourceKind::PriorMemory => {
                return Err(invalid(
                    "prior_memory evidence must name the evidence it restates",
                ));
            }
            None => content_hash.to_vec(),
        };
        let receipt = e
            .receipt
            .map(|r| crate::compaction::clip(r, MAX_RECEIPT_CHARS));
        self.conn.execute(
            "INSERT INTO kn_evidence(source_kind,source_ref,observed_at,content_hash,origin_hash,
                                     receipt,verification,scope,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(source_kind,content_hash,scope) DO NOTHING",
            params![
                e.source_kind.as_str(),
                e.source_ref,
                e.observed_at,
                &content_hash[..],
                origin,
                receipt,
                e.verification.as_ref().map(|v| v.to_string()),
                e.scope,
                self.now,
            ],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM kn_evidence WHERE source_kind=?1 AND content_hash=?2 AND scope=?3",
            params![e.source_kind.as_str(), &content_hash[..], e.scope],
            |r| r.get(0),
        )?)
    }

    // ---- claims ---------------------------------------------------------------------

    /// Check that the supporting/refuting evidence justifies `status`.
    fn check_status_supported(&self, claim: i64, status: EpistemicStatus) -> KnowledgeResult<()> {
        let mut stmt = self
            .conn
            .prepare("SELECT e.source_kind, ce.stance FROM kn_claim_evidence ce JOIN kn_evidence e ON e.id=ce.evidence_id WHERE ce.claim_id=?1")?;
        let links: Vec<(SourceKind, Stance)> = stmt
            .query_map([claim], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(k, s)| Some((SourceKind::parse(&k)?, Stance::parse(&s)?)))
            .collect();
        let supports = |pred: fn(SourceKind) -> bool| {
            links
                .iter()
                .any(|(k, s)| *s == Stance::Supports && pred(*k))
        };
        let ok = match status {
            EpistemicStatus::Verified => supports(SourceKind::is_direct),
            EpistemicStatus::UserReported => {
                supports(|k| k.is_direct() || k == SourceKind::UserStatement)
            }
            EpistemicStatus::Inferred | EpistemicStatus::Unverified => supports(|_| true),
            EpistemicStatus::Disputed => links.iter().any(|(_, s)| *s == Stance::Refutes),
            EpistemicStatus::Superseded | EpistemicStatus::Invalidated => true,
        };
        if ok {
            Ok(())
        } else {
            Err(KnowledgeError::InsufficientEvidence(format!(
                "status '{status}' is not supported by the linked evidence"
            )))
        }
    }

    /// Assert a claim backed by `evidence`. If an active claim with the same content
    /// already exists in the scope, the evidence is attached to it and its id returned:
    /// equivalent claims are merged without dropping their independent evidence.
    pub fn assert_claim(
        &mut self,
        c: &NewClaim<'_>,
        evidence: &[(i64, Stance)],
    ) -> KnowledgeResult<i64> {
        if !valid_predicate(c.predicate) {
            return Err(invalid(format!(
                "predicate '{}' must be snake_case",
                c.predicate
            )));
        }
        if c.scope.trim().is_empty() {
            return Err(invalid("claim scope is required"));
        }
        if !c.status.is_active() {
            return Err(invalid("new claims cannot start superseded or invalidated"));
        }
        if (c.category == ClaimCategory::Preference) != c.basis.is_some() {
            return Err(invalid(
                "a basis is required for preferences and only for preferences",
            ));
        }
        if c.inference_confidence.is_some() && c.status != EpistemicStatus::Inferred {
            return Err(invalid(
                "inference_confidence applies only to inferred claims",
            ));
        }
        if c.inference_confidence
            .is_some_and(|v| !(0.0..=1.0).contains(&v))
        {
            return Err(invalid("inference_confidence must be within 0..=1"));
        }
        if let (Some(from), Some(until)) = (c.valid_from, c.valid_until)
            && until < from
        {
            return Err(invalid("valid_until precedes valid_from"));
        }
        if evidence.is_empty() {
            return Err(KnowledgeError::InsufficientEvidence(
                "a claim needs evidence".into(),
            ));
        }
        self.active_entity(c.subject)?;
        match &c.object {
            ClaimObject::Entity(o) => {
                self.active_entity(*o)?;
            }
            ClaimObject::Value(v) => {
                if v.trim().is_empty() || v.chars().count() > MAX_VALUE_CHARS {
                    return Err(invalid("claim value must be 1–2000 characters"));
                }
            }
        }
        for (id, _) in evidence {
            self.evidence_kind(*id)?;
        }
        let fp = self.claim_fingerprint(c.subject, c.predicate, &c.object)?;
        self.check_tombstone(&fp)?;

        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM kn_claims WHERE fingerprint=?1 AND scope=?2
                 AND status NOT IN ('superseded','invalidated') ORDER BY id LIMIT 1",
                params![&fp[..], c.scope],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            for (ev, stance) in evidence {
                self.link_evidence(id, *ev, *stance)?;
            }
            self.log(
                "reinforce",
                "claim",
                Some(id),
                None,
                json!({"evidence": evidence.len()}),
            )?;
            return Ok(id);
        }

        let (object_entity, object_value) = match &c.object {
            ClaimObject::Entity(o) => (Some(*o), None),
            ClaimObject::Value(v) => (None, Some(v.trim())),
        };
        let (retention, expires_at) = match c.retention {
            Retention::Durable => ("durable", None),
            Retention::Transient { expires_at } => ("transient", Some(expires_at)),
        };
        self.conn.execute(
            "INSERT INTO kn_claims(subject_id,predicate,object_entity_id,object_value,category,basis,
                 status,inference_confidence,observed_at,valid_from,valid_until,last_verified_at,
                 retention,expires_at,scope,sensitivity,fingerprint,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?18)",
            params![
                c.subject,
                c.predicate,
                object_entity,
                object_value,
                c.category.as_str(),
                c.basis.map(|b| b.as_str()),
                c.status.as_str(),
                c.inference_confidence,
                c.observed_at,
                c.valid_from,
                c.valid_until,
                (c.status == EpistemicStatus::Verified).then_some(self.now),
                retention,
                expires_at,
                c.scope,
                c.sensitivity as i64,
                &fp[..],
                self.now,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        for (ev, stance) in evidence {
            self.link_evidence(id, *ev, *stance)?;
        }
        self.check_status_supported(id, c.status)?;
        self.log(
            "assert",
            "claim",
            Some(id),
            None,
            json!({"status": c.status, "predicate": c.predicate}),
        )?;
        Ok(id)
    }

    pub fn link_evidence(
        &mut self,
        claim: i64,
        evidence: i64,
        stance: Stance,
    ) -> KnowledgeResult<()> {
        self.claim_row(claim)?;
        self.evidence_kind(evidence)?;
        self.conn.execute(
            "INSERT INTO kn_claim_evidence(claim_id,evidence_id,stance) VALUES(?1,?2,?3)
             ON CONFLICT(claim_id,evidence_id) DO UPDATE SET stance=excluded.stance",
            params![claim, evidence, stance.as_str()],
        )?;
        Ok(())
    }

    /// Change a claim's epistemic status. Use [`supersede`](Self::supersede) for
    /// supersession; invalidation requires a reason.
    pub fn set_status(
        &mut self,
        claim: i64,
        status: EpistemicStatus,
        reason: Option<&str>,
    ) -> KnowledgeResult<()> {
        let row = self.claim_row(claim)?;
        if status == EpistemicStatus::Superseded {
            return Err(invalid(
                "use supersede() so the replacing claim is recorded",
            ));
        }
        if status == EpistemicStatus::Invalidated && reason.is_none_or(|r| r.trim().is_empty()) {
            return Err(invalid("invalidating a claim requires a reason"));
        }
        self.check_status_supported(claim, status)?;
        self.conn.execute(
            "UPDATE kn_claims SET status=?2, status_reason=?3, updated_at=?4,
                 last_verified_at=CASE WHEN ?2='verified' THEN ?4 ELSE last_verified_at END
             WHERE id=?1",
            params![claim, status.as_str(), reason, self.now],
        )?;
        self.log(
            "set_status",
            "claim",
            Some(claim),
            reason,
            json!({"from": row.status, "to": status}),
        )
    }

    /// `new` replaces `old` as the current value of the same subject and predicate. History
    /// is kept: `old` becomes `superseded` with its validity closed at the new value's start.
    /// An observation older than `old` cannot supersede it (observations arrive out of order).
    pub fn supersede(&mut self, old: i64, new: i64, reason: &str) -> KnowledgeResult<()> {
        let (o, n) = (self.claim_row(old)?, self.claim_row(new)?);
        if old == new || o.subject != n.subject || o.predicate != n.predicate {
            return Err(invalid(
                "supersession needs two claims with the same subject and predicate",
            ));
        }
        if !o.status.is_active() || !n.status.is_active() {
            return Err(invalid("both claims must be active"));
        }
        if n.observed_at < o.observed_at {
            return Err(invalid("an older observation cannot supersede a newer one"));
        }
        let mut until = o.valid_until.or(n.valid_from).unwrap_or(n.observed_at);
        if let Some(from) = o.valid_from {
            until = until.max(from);
        }
        self.conn.execute(
            "UPDATE kn_claims SET status='superseded', superseded_by=?2, valid_until=?3,
                 status_reason=?4, updated_at=?5 WHERE id=?1",
            params![old, new, until, reason, self.now],
        )?;
        self.log(
            "supersede",
            "claim",
            Some(old),
            Some(reason),
            json!({"by": new}),
        )
    }

    /// Forget a claim: remove it and its evidence links, purge evidence nothing else uses,
    /// and tombstone its content so it cannot be silently re-learned.
    pub fn delete_claim(&mut self, claim: i64, reason: &str) -> KnowledgeResult<()> {
        let row = self.claim_row(claim)?;
        let evidence = self.ids(
            "SELECT evidence_id FROM kn_claim_evidence WHERE claim_id=?1",
            claim,
        )?;
        let generation = self.bump_generation()?;
        self.tombstone_claim(claim, generation)?;
        self.conn
            .execute("DELETE FROM kn_claims WHERE id=?1", [claim])?;
        let purged = self.purge_orphan_evidence(&evidence)?;
        self.log(
            "delete",
            "claim",
            Some(claim),
            Some(reason),
            json!({"fingerprint": hex(&row.fingerprint), "evidence_purged": purged, "generation": generation}),
        )
    }

    // ---- procedures -----------------------------------------------------------------

    /// Store a new version of a procedure attached to a `procedure` entity. New versions
    /// start as `draft`; nothing here executes anything.
    pub fn put_procedure(
        &mut self,
        entity: i64,
        spec: &ProcedureSpec,
    ) -> KnowledgeResult<(i64, i64)> {
        if self.active_entity(entity)?.kind != EntityKind::Procedure {
            return Err(invalid("procedures attach to entities of kind 'procedure'"));
        }
        if spec.summary.trim().is_empty() || spec.steps.is_empty() {
            return Err(invalid("a procedure needs a summary and at least one step"));
        }
        for step in &spec.steps {
            if let Some(r) = &step.workflow_ref
                && !r
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_.:-/".contains(c))
            {
                return Err(invalid(format!(
                    "workflow_ref '{r}' must name a typed workflow, not a command"
                )));
            }
        }
        let version: i64 = self.conn.query_row(
            "SELECT coalesce(max(version),0)+1 FROM kn_procedures WHERE entity_id=?1",
            [entity],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO kn_procedures(entity_id,version,spec,status,created_at) VALUES(?1,?2,?3,'draft',?4)",
            params![
                entity,
                version,
                serde_json::to_string(spec).map_err(|e| invalid(e.to_string()))?,
                self.now
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.log(
            "put",
            "procedure",
            Some(id),
            None,
            json!({"entity": entity, "version": version}),
        )?;
        Ok((id, version))
    }

    pub fn set_procedure_status(
        &mut self,
        procedure: i64,
        status: ProcedureStatus,
    ) -> KnowledgeResult<()> {
        let n = self.conn.execute(
            "UPDATE kn_procedures SET status=?2 WHERE id=?1",
            params![procedure, status.as_str()],
        )?;
        if n == 0 {
            return Err(KnowledgeError::NotFound("procedure", procedure));
        }
        self.log(
            "set_status",
            "procedure",
            Some(procedure),
            None,
            json!({"to": status}),
        )
    }

    /// Record an execution outcome. Needs a tool observation or artifact as evidence.
    pub fn record_procedure_run(
        &mut self,
        procedure: i64,
        outcome: RunOutcome,
        evidence: i64,
        notes: Option<&str>,
    ) -> KnowledgeResult<i64> {
        if !self.evidence_kind(evidence)?.is_direct() {
            return Err(KnowledgeError::InsufficientEvidence(
                "procedure runs need a tool observation or artifact".into(),
            ));
        }
        self.conn.execute(
            "INSERT INTO kn_procedure_runs(procedure_id,outcome,evidence_id,notes,at) VALUES(?1,?2,?3,?4,?5)",
            params![procedure, outcome.as_str(), evidence, notes, self.now],
        )?;
        let id = self.conn.last_insert_rowid();
        self.log(
            "run",
            "procedure",
            Some(procedure),
            None,
            json!({"outcome": outcome}),
        )?;
        Ok(id)
    }

    // ---- projects -------------------------------------------------------------------

    pub fn add_project_item(&mut self, item: &NewProjectItem<'_>) -> KnowledgeResult<i64> {
        if self.active_entity(item.project)?.kind != EntityKind::Project {
            return Err(invalid(
                "project items attach to entities of kind 'project'",
            ));
        }
        if item.state.requires_direct_evidence() || item.state == WorkState::Superseded {
            return Err(invalid(format!(
                "new items start proposed, approved, in_progress or failed; use set_project_state for '{}'",
                item.state
            )));
        }
        if item.title.trim().is_empty() || item.scope.trim().is_empty() {
            return Err(invalid("project items need a title and scope"));
        }
        self.conn.execute(
            "INSERT INTO kn_project_items(project_id,kind,title,detail,state,scope,sensitivity,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8)",
            params![
                item.project,
                item.kind.as_str(),
                item.title.trim(),
                item.detail,
                item.state.as_str(),
                item.scope,
                item.sensitivity as i64,
                self.now
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.log(
            "create",
            "project_item",
            Some(id),
            None,
            json!({"kind": item.kind, "state": item.state}),
        )?;
        Ok(id)
    }

    /// Move a project item to `state`. `implemented`, `tested` and `deployed` require
    /// `evidence` from a tool observation or artifact; a model's description of the work is
    /// not enough.
    pub fn set_project_state(
        &mut self,
        item: i64,
        state: WorkState,
        evidence: Option<i64>,
        reason: Option<&str>,
    ) -> KnowledgeResult<()> {
        let (from, previous): (String, Option<i64>) = self
            .conn
            .query_row(
                "SELECT state, state_evidence_id FROM kn_project_items WHERE id=?1",
                [item],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(KnowledgeError::NotFound("project item", item))?;
        if state == WorkState::Superseded {
            return Err(invalid(
                "use supersede_project_item() so the replacement is recorded",
            ));
        }
        if state.requires_direct_evidence() {
            let Some(ev) = evidence else {
                return Err(KnowledgeError::InsufficientEvidence(format!(
                    "'{state}' needs evidence"
                )));
            };
            if !self.evidence_kind(ev)?.is_direct() {
                return Err(KnowledgeError::InsufficientEvidence(format!(
                    "'{state}' needs a tool observation or artifact, not a description"
                )));
            }
        } else if let Some(ev) = evidence {
            self.evidence_kind(ev)?;
        }
        self.conn.execute(
            "UPDATE kn_project_items SET state=?2, state_evidence_id=?3, updated_at=?4 WHERE id=?1",
            params![item, state.as_str(), evidence, self.now],
        )?;
        self.purge_orphan_evidence(&previous.into_iter().collect::<Vec<_>>())?;
        self.log(
            "set_state",
            "project_item",
            Some(item),
            reason,
            json!({"from": from, "to": state}),
        )
    }

    pub fn supersede_project_item(
        &mut self,
        old: i64,
        new: i64,
        reason: &str,
    ) -> KnowledgeResult<()> {
        let projects: Vec<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT project_id FROM kn_project_items WHERE id IN (?1,?2)")?;
            stmt.query_map([old, new], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        if old == new || projects.len() != 2 || projects[0] != projects[1] {
            return Err(invalid("supersession needs two items of the same project"));
        }
        // The superseded item keeps its state evidence as history.
        self.conn.execute(
            "UPDATE kn_project_items SET state='superseded', superseded_by=?2, updated_at=?3 WHERE id=?1",
            params![old, new, self.now],
        )?;
        self.log(
            "supersede",
            "project_item",
            Some(old),
            Some(reason),
            json!({"by": new}),
        )
    }
}

impl KnowledgeTx<'_> {
    pub fn pin_entity(&mut self, entity: i64, pinned: bool) -> KnowledgeResult<()> {
        self.active_entity(entity)?;
        self.conn.execute("INSERT INTO kn_pins(entity_id,pinned) VALUES(?1,?2) ON CONFLICT(entity_id) DO UPDATE SET pinned=excluded.pinned",params![entity,pinned])?;
        self.log(
            "pin",
            "entity",
            Some(entity),
            None,
            json!({"pinned":pinned}),
        )
    }
    /// Privacy changes invalidate in-flight retrieval/consolidation work too.
    pub fn mark_private(&mut self, entity: i64) -> KnowledgeResult<()> {
        self.active_entity(entity)?;
        self.conn.execute(
            "UPDATE kn_entities SET sensitivity=max(sensitivity,1) WHERE id=?1",
            [entity],
        )?;
        self.conn.execute("UPDATE kn_claims SET sensitivity=max(sensitivity,1) WHERE subject_id=?1 OR object_entity_id=?1",[entity])?;
        self.conn.execute(
            "UPDATE kn_meta SET value=value+1 WHERE key='deletion_generation'",
            [],
        )?;
        self.log(
            "privacy",
            "entity",
            Some(entity),
            None,
            json!({"sensitivity":"private"}),
        )
    }
    /// Undo the most recent merge only while no subsequent knowledge mutation exists.
    /// A stale snapshot must never overwrite edits, deletions or later merges.
    pub fn separate_entity(&mut self, from: i64, reason: &str) -> KnowledgeResult<()> {
        let (into, operation, snapshot): (i64, i64, String) = self
            .conn
            .query_row(
                "SELECT into_id,operation_id,snapshot FROM kn_merge_snapshots WHERE from_id=?1",
                [from],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or(KnowledgeError::NotFound("merge snapshot", from))?;
        let latest: i64 = self
            .conn
            .query_row("SELECT max(id) FROM kn_operations", [], |r| r.get(0))?;
        if latest != operation {
            return Err(invalid(
                "separation requires an unchanged merge; later edits must be reconciled explicitly",
            ));
        }
        let data: serde_json::Value =
            serde_json::from_str(&snapshot).map_err(|e| invalid(e.to_string()))?;
        self.conn.execute(
            "UPDATE kn_entities SET status='active',merged_into=NULL WHERE id=?1",
            [from],
        )?;
        for (key, table, column) in [
            ("subjects", "kn_claims", "subject_id"),
            ("objects", "kn_claims", "object_entity_id"),
            ("projects", "kn_project_items", "project_id"),
            ("procedures", "kn_procedures", "entity_id"),
        ] {
            let ids = data[key]
                .as_array()
                .ok_or_else(|| invalid("corrupt merge snapshot"))?;
            for id in ids {
                let id = id.as_i64().ok_or_else(|| invalid("corrupt merge id"))?;
                self.conn.execute(
                    &format!("UPDATE {table} SET {column}=?1 WHERE id=?2"),
                    [from, id],
                )?;
            }
        }
        self.conn.execute(
            "DELETE FROM kn_aliases WHERE entity_id IN (?1,?2)",
            [from, into],
        )?;
        for (key, entity) in [("aliases", from), ("target_aliases", into)] {
            for alias in data[key]
                .as_array()
                .ok_or_else(|| invalid("corrupt alias snapshot"))?
            {
                self.add_alias(
                    entity,
                    alias.as_str().ok_or_else(|| invalid("corrupt alias"))?,
                )?;
            }
        }
        // Recompute fingerprints under the restored identity assignments.
        let rows: Vec<ClaimIdentityRow> = {
            let mut stmt = self.conn.prepare(
                "SELECT id,subject_id,predicate,object_entity_id,object_value FROM kn_claims",
            )?;
            stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<_, _>>()?
        };
        for (id, subject, predicate, object, value) in rows {
            let object = object
                .map(ClaimObject::Entity)
                .unwrap_or_else(|| ClaimObject::Value(value.unwrap_or_default()));
            let fp = self.claim_fingerprint(subject, &predicate, &object)?;
            self.conn.execute(
                "UPDATE kn_claims SET fingerprint=?2 WHERE id=?1",
                params![id, &fp[..]],
            )?;
        }
        self.conn
            .execute("DELETE FROM kn_merge_snapshots WHERE from_id=?1", [from])?;
        self.conn.execute(
            "UPDATE kn_meta SET value=value+1 WHERE key='deletion_generation'",
            [],
        )?;
        self.log(
            "separate",
            "entity",
            Some(from),
            Some(reason),
            json!({"from":into}),
        )
    }
}

/// `(id, subject_id, predicate, object_entity_id, object_value)` of a claim.
type ClaimIdentityRow = (i64, i64, String, Option<i64>, Option<String>);
