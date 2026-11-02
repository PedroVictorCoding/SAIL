use rusqlite::{Connection, OptionalExtension, Row, ToSql};

use super::model::*;
use super::{KnowledgeError, KnowledgeResult, KnowledgeStore};

/// SQL predicate restricting table alias `a` to what `Access` may see. Binds `:all`,
/// `:scopes` and `:maxsens`, so every query using it must pass [`access_params`].
fn visible(a: &str) -> String {
    format!(
        "((:all = 1 OR {a}.scope IN (SELECT value FROM json_each(:scopes))) AND {a}.sensitivity <= :maxsens)"
    )
}

struct AccessParams {
    all: i64,
    scopes: String,
    maxsens: i64,
}

fn access_params(access: &Access) -> AccessParams {
    AccessParams {
        all: access.scopes.is_none() as i64,
        scopes: serde_json::to_string(access.scopes.as_deref().unwrap_or(&[])).unwrap_or_default(),
        maxsens: access.max_sensitivity as i64,
    }
}

/// Named parameters: the access triple plus the query's own.
fn named<'p>(
    a: &'p AccessParams,
    extra: &[(&'p str, &'p dyn ToSql)],
) -> Vec<(&'p str, &'p dyn ToSql)> {
    let mut out: Vec<(&str, &dyn ToSql)> = vec![
        (":all", &a.all),
        (":scopes", &a.scopes),
        (":maxsens", &a.maxsens),
    ];
    out.extend_from_slice(extra);
    out
}

const ENTITY_COLS: &str =
    "e.id, e.kind, e.name, e.merged_into, e.scope, e.sensitivity, e.created_at, e.updated_at";

fn entity_from_row(r: &Row<'_>) -> rusqlite::Result<Entity> {
    Ok(Entity {
        id: r.get(0)?,
        kind: EntityKind::parse(&r.get::<_, String>(1)?).unwrap_or(EntityKind::Other),
        name: r.get(2)?,
        aliases: Vec::new(),
        merged_into: r.get(3)?,
        scope: r.get(4)?,
        sensitivity: Sensitivity::from_i64(r.get(5)?),
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

fn with_aliases(conn: &Connection, mut e: Entity) -> KnowledgeResult<Entity> {
    let mut stmt =
        conn.prepare_cached("SELECT alias FROM kn_aliases WHERE entity_id=?1 ORDER BY alias_key")?;
    e.aliases = stmt
        .query_map([e.id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(e)
}

const CLAIM_COLS: &str = "c.id, c.subject_id, c.predicate, c.object_entity_id, c.object_value,
    c.category, c.basis, c.status, c.inference_confidence, c.observed_at, c.valid_from,
    c.valid_until, c.last_verified_at, c.superseded_by, c.status_reason, c.retention,
    c.expires_at, c.scope, c.sensitivity, c.created_at, c.updated_at";

fn claim_from_row(r: &Row<'_>) -> rusqlite::Result<Claim> {
    let object = match r.get::<_, Option<i64>>(3)? {
        Some(id) => ClaimObject::Entity(id),
        None => ClaimObject::Value(r.get(4)?),
    };
    let retention = match (
        r.get::<_, String>(15)?.as_str(),
        r.get::<_, Option<i64>>(16)?,
    ) {
        ("transient", Some(expires_at)) => Retention::Transient { expires_at },
        _ => Retention::Durable,
    };
    Ok(Claim {
        id: r.get(0)?,
        subject: r.get(1)?,
        predicate: r.get(2)?,
        object,
        category: ClaimCategory::parse(&r.get::<_, String>(5)?).unwrap_or(ClaimCategory::Fact),
        basis: r
            .get::<_, Option<String>>(6)?
            .and_then(|b| PreferenceBasis::parse(&b)),
        status: EpistemicStatus::parse(&r.get::<_, String>(7)?)
            .unwrap_or(EpistemicStatus::Unverified),
        inference_confidence: r.get(8)?,
        observed_at: r.get(9)?,
        valid_from: r.get(10)?,
        valid_until: r.get(11)?,
        last_verified_at: r.get(12)?,
        superseded_by: r.get(13)?,
        status_reason: r.get(14)?,
        retention,
        scope: r.get(17)?,
        sensitivity: Sensitivity::from_i64(r.get(18)?),
        created_at: r.get(19)?,
        updated_at: r.get(20)?,
    })
}

/// Joins and filters that make a claim visible only if the claim, its subject and (for
/// relationships) its object entity are all visible to the caller.
fn claim_from_clause() -> String {
    format!(
        "FROM kn_claims c
         JOIN kn_entities s ON s.id = c.subject_id
         LEFT JOIN kn_entities o ON o.id = c.object_entity_id
         WHERE {} AND {} AND (c.object_entity_id IS NULL OR {})",
        visible("c"),
        visible("s"),
        visible("o")
    )
}

/// SQL rank putting explicit preferences ahead of inferred ones; facts rank equally.
const BASIS_RANK: &str = "CASE c.basis WHEN 'explicit' THEN 0 WHEN 'situational' THEN 1
    WHEN 'observed' THEN 2 WHEN 'inferred' THEN 3 ELSE 0 END";

impl KnowledgeStore<'_> {
    pub fn entity(&self, id: i64, access: &Access) -> KnowledgeResult<Option<Entity>> {
        let a = access_params(access);
        self.read(|conn| {
            let sql = format!(
                "SELECT {ENTITY_COLS} FROM kn_entities e WHERE e.id=:id AND {}",
                visible("e")
            );
            let entity = conn
                .query_row(&sql, &*named(&a, &[(":id", &id)]), entity_from_row)
                .optional()?;
            entity.map(|e| with_aliases(conn, e)).transpose()
        })
    }

    /// Active entities whose name or an alias matches `name` exactly (case- and
    /// whitespace-insensitive). Several results mean several distinct entities: the caller
    /// must not assume they are the same.
    pub fn find_entities(
        &self,
        name: &str,
        kind: Option<EntityKind>,
        access: &Access,
    ) -> KnowledgeResult<Vec<Entity>> {
        let a = access_params(access);
        let key = name_key(name);
        let kind = kind.map(|k| k.as_str());
        self.read(|conn| {
            let sql = format!(
                "SELECT {ENTITY_COLS} FROM kn_entities e
                 WHERE e.status='active' AND (:kind IS NULL OR e.kind=:kind) AND {}
                   AND (e.name_key=:key OR e.id IN (SELECT entity_id FROM kn_aliases WHERE alias_key=:key))
                 ORDER BY e.id",
                visible("e")
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows: Vec<Entity> = stmt
                .query_map(&*named(&a, &[(":key", &key), (":kind", &kind)]), entity_from_row)?
                .collect::<Result<_, _>>()?;
            rows.into_iter().map(|e| with_aliases(conn, e)).collect()
        })
    }

    /// Claims whose subject is `entity`. Inactive (superseded/invalidated) claims are
    /// included only when asked, for history views.
    pub fn claims_about(
        &self,
        entity: i64,
        access: &Access,
        include_inactive: bool,
    ) -> KnowledgeResult<Vec<Claim>> {
        let a = access_params(access);
        let inactive = include_inactive as i64;
        self.read(|conn| {
            let sql = format!(
                "SELECT {CLAIM_COLS} {} AND c.subject_id=:subject
                   AND (:inactive = 1 OR c.status NOT IN ('superseded','invalidated'))
                 ORDER BY c.predicate, c.observed_at, c.id",
                claim_from_clause()
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(
                    &*named(&a, &[(":subject", &entity), (":inactive", &inactive)]),
                    claim_from_row,
                )?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    /// Current-state projection for one subject and predicate at time `now`: the active,
    /// still-valid claim with the best preference basis and, among those, the latest
    /// observation. Ordering by observation time (not insertion) handles out-of-order input.
    pub fn current(
        &self,
        subject: i64,
        predicate: &str,
        access: &Access,
        now: i64,
    ) -> KnowledgeResult<Option<Claim>> {
        let a = access_params(access);
        self.read(|conn| {
            let sql = format!(
                "SELECT {CLAIM_COLS} {} AND c.subject_id=:subject AND c.predicate=:predicate
                   AND c.status NOT IN ('superseded','invalidated')
                   AND (c.valid_until IS NULL OR c.valid_until > :now)
                   AND (c.valid_from IS NULL OR c.valid_from <= :now)
                   AND (c.expires_at IS NULL OR c.expires_at > :now)
                 ORDER BY {BASIS_RANK}, c.observed_at DESC, c.id DESC LIMIT 1",
                claim_from_clause()
            );
            Ok(conn
                .query_row(
                    &sql,
                    &*named(
                        &a,
                        &[
                            (":subject", &subject),
                            (":predicate", &predicate),
                            (":now", &now),
                        ],
                    ),
                    claim_from_row,
                )
                .optional()?)
        })
    }

    /// Every claim for one subject and predicate, oldest observation first.
    pub fn history(
        &self,
        subject: i64,
        predicate: &str,
        access: &Access,
    ) -> KnowledgeResult<Vec<Claim>> {
        let a = access_params(access);
        self.read(|conn| {
            let sql = format!(
                "SELECT {CLAIM_COLS} {} AND c.subject_id=:subject AND c.predicate=:predicate
                 ORDER BY c.observed_at, c.id",
                claim_from_clause()
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(
                    &*named(&a, &[(":subject", &subject), (":predicate", &predicate)]),
                    claim_from_row,
                )?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    fn claim_visible(
        &self,
        conn: &Connection,
        claim: i64,
        a: &AccessParams,
    ) -> KnowledgeResult<()> {
        let sql = format!("SELECT 1 {} AND c.id=:claim", claim_from_clause());
        conn.query_row(&sql, &*named(a, &[(":claim", &claim)]), |_| Ok(()))
            .optional()?
            .ok_or(KnowledgeError::NotFound("claim", claim))
    }

    /// Evidence linked to a visible claim. Evidence in scopes the caller cannot see is
    /// omitted rather than revealed.
    pub fn evidence(&self, claim: i64, access: &Access) -> KnowledgeResult<Vec<EvidenceLink>> {
        let a = access_params(access);
        self.read(|conn| {
            self.claim_visible(conn, claim, &a)?;
            let sql = "SELECT e.id, e.source_kind, e.source_ref, e.observed_at, e.receipt,
                              e.verification, e.scope, ce.stance
                       FROM kn_claim_evidence ce JOIN kn_evidence e ON e.id = ce.evidence_id
                       WHERE ce.claim_id=:claim
                         AND (:all = 1 OR e.scope IN (SELECT value FROM json_each(:scopes)))
                       ORDER BY e.observed_at, e.id";
            let mut stmt = conn.prepare(sql)?;
            let params: &[(&str, &dyn ToSql)] =
                &[(":all", &a.all), (":scopes", &a.scopes), (":claim", &claim)];
            let rows = stmt
                .query_map(params, |r| {
                    Ok(EvidenceLink {
                        evidence: Evidence {
                            id: r.get(0)?,
                            source_kind: SourceKind::parse(&r.get::<_, String>(1)?)
                                .unwrap_or(SourceKind::PriorMemory),
                            source_ref: r.get(2)?,
                            observed_at: r.get(3)?,
                            receipt: r.get(4)?,
                            verification: r
                                .get::<_, Option<String>>(5)?
                                .and_then(|v| serde_json::from_str(&v).ok()),
                            scope: r.get(6)?,
                        },
                        stance: Stance::parse(&r.get::<_, String>(7)?).unwrap_or(Stance::Supports),
                    })
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    /// Support for a visible claim, counted by distinct origin so restatements of one
    /// statement never look like independent confirmation.
    pub fn support(&self, claim: i64, access: &Access) -> KnowledgeResult<Support> {
        let a = access_params(access);
        self.read(|conn| {
            self.claim_visible(conn, claim, &a)?;
            Ok(conn.query_row(
                "SELECT
                   count(DISTINCT CASE WHEN ce.stance='supports' AND e.source_kind IN ('tool_observation','external_artifact') THEN e.origin_hash END),
                   count(DISTINCT CASE WHEN ce.stance='supports' AND e.source_kind='user_statement' THEN e.origin_hash END),
                   count(CASE WHEN ce.stance='supports' AND e.source_kind='model_inference' THEN 1 END),
                   count(DISTINCT CASE WHEN ce.stance='refutes' THEN e.origin_hash END)
                 FROM kn_claim_evidence ce JOIN kn_evidence e ON e.id=ce.evidence_id
                 WHERE ce.claim_id=?1",
                [claim],
                |r| {
                    Ok(Support {
                        direct_origins: r.get::<_, i64>(0)? as usize,
                        user_origins: r.get::<_, i64>(1)? as usize,
                        inferences: r.get::<_, i64>(2)? as usize,
                        refuting_origins: r.get::<_, i64>(3)? as usize,
                    })
                },
            )?)
        })
    }

    /// Relationship edges touching `entity` in either direction. Edges to entities the
    /// caller cannot see are not returned, so traversal cannot cross a scope boundary.
    pub fn neighbors(
        &self,
        entity: i64,
        access: &Access,
        include_inactive: bool,
    ) -> KnowledgeResult<Vec<Edge>> {
        let a = access_params(access);
        let inactive = include_inactive as i64;
        self.read(|conn| {
            let sql = format!(
                "SELECT c.id, c.predicate, c.subject_id = :entity, c.status, {ENTITY_COLS}
                 FROM kn_claims c
                 JOIN kn_entities e ON e.id = CASE WHEN c.subject_id = :entity
                                                   THEN c.object_entity_id ELSE c.subject_id END
                 JOIN kn_entities me ON me.id = :entity
                 WHERE c.object_entity_id IS NOT NULL
                   AND (c.subject_id = :entity OR c.object_entity_id = :entity)
                   AND (:inactive = 1 OR c.status NOT IN ('superseded','invalidated'))
                   AND {} AND {} AND {}
                 ORDER BY c.predicate, e.name_key",
                visible("c"),
                visible("e"),
                visible("me")
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows: Vec<Edge> = stmt
                .query_map(
                    &*named(&a, &[(":entity", &entity), (":inactive", &inactive)]),
                    |r| {
                        let other = Entity {
                            id: r.get(4)?,
                            kind: EntityKind::parse(&r.get::<_, String>(5)?)
                                .unwrap_or(EntityKind::Other),
                            name: r.get(6)?,
                            aliases: Vec::new(),
                            merged_into: r.get(7)?,
                            scope: r.get(8)?,
                            sensitivity: Sensitivity::from_i64(r.get(9)?),
                            created_at: r.get(10)?,
                            updated_at: r.get(11)?,
                        };
                        Ok(Edge {
                            claim_id: r.get(0)?,
                            relation: r.get(1)?,
                            outgoing: r.get(2)?,
                            status: EpistemicStatus::parse(&r.get::<_, String>(3)?)
                                .unwrap_or(EpistemicStatus::Unverified),
                            other,
                        })
                    },
                )?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    /// Every version of the procedure attached to a visible `procedure` entity, newest first.
    pub fn procedure_versions(
        &self,
        entity: i64,
        access: &Access,
    ) -> KnowledgeResult<Vec<Procedure>> {
        let a = access_params(access);
        self.read(|conn| {
            let sql = format!(
                "SELECT p.id, p.entity_id, p.version, p.status, p.spec, p.created_at
                 FROM kn_procedures p JOIN kn_entities e ON e.id = p.entity_id
                 WHERE p.entity_id=:entity AND {} ORDER BY p.version DESC",
                visible("e")
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(&*named(&a, &[(":entity", &entity)]), |r| {
                    Ok(Procedure {
                        id: r.get(0)?,
                        entity_id: r.get(1)?,
                        version: r.get(2)?,
                        status: ProcedureStatus::parse(&r.get::<_, String>(3)?)
                            .unwrap_or(ProcedureStatus::Draft),
                        spec: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                        created_at: r.get(5)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    pub fn procedure_runs(
        &self,
        procedure: i64,
        access: &Access,
    ) -> KnowledgeResult<Vec<ProcedureRun>> {
        let a = access_params(access);
        self.read(|conn| {
            let sql = format!(
                "SELECT r.id, r.procedure_id, r.outcome, r.evidence_id, r.notes, r.at
                 FROM kn_procedure_runs r JOIN kn_procedures p ON p.id = r.procedure_id
                 JOIN kn_entities e ON e.id = p.entity_id
                 WHERE r.procedure_id=:procedure AND {} ORDER BY r.at, r.id",
                visible("e")
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(&*named(&a, &[(":procedure", &procedure)]), |r| {
                    Ok(ProcedureRun {
                        id: r.get(0)?,
                        procedure_id: r.get(1)?,
                        outcome: RunOutcome::parse(&r.get::<_, String>(2)?)
                            .unwrap_or(RunOutcome::Aborted),
                        evidence_id: r.get(3)?,
                        notes: r.get(4)?,
                        at: r.get(5)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    pub fn project_items(
        &self,
        project: i64,
        access: &Access,
    ) -> KnowledgeResult<Vec<ProjectItem>> {
        let a = access_params(access);
        self.read(|conn| {
            let sql = format!(
                "SELECT i.id, i.project_id, i.kind, i.title, i.detail, i.state, i.state_evidence_id,
                        i.superseded_by, i.scope, i.sensitivity, i.updated_at
                 FROM kn_project_items i JOIN kn_entities p ON p.id = i.project_id
                 WHERE i.project_id=:project AND {} AND {} ORDER BY i.kind, i.id",
                visible("i"),
                visible("p")
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(&*named(&a, &[(":project", &project)]), |r| {
                    Ok(ProjectItem {
                        id: r.get(0)?,
                        project: r.get(1)?,
                        kind: ProjectItemKind::parse(&r.get::<_, String>(2)?)
                            .unwrap_or(ProjectItemKind::Task),
                        title: r.get(3)?,
                        detail: r.get(4)?,
                        state: WorkState::parse(&r.get::<_, String>(5)?)
                            .unwrap_or(WorkState::Proposed),
                        state_evidence: r.get(6)?,
                        superseded_by: r.get(7)?,
                        scope: r.get(8)?,
                        sensitivity: Sensitivity::from_i64(r.get(9)?),
                        updated_at: r.get(10)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    /// The change log, newest first. It records ids, kinds and reasons, never deleted
    /// content (deletions keep only a fingerprint).
    pub fn operations(&self, limit: usize) -> KnowledgeResult<Vec<Operation>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, at, actor, op, target_kind, target_id, reason, detail
                 FROM kn_operations ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = stmt
                .query_map([limit as i64], |r| {
                    Ok(Operation {
                        id: r.get(0)?,
                        at: r.get(1)?,
                        actor: r.get(2)?,
                        op: r.get(3)?,
                        target_kind: r.get(4)?,
                        target_id: r.get(5)?,
                        reason: r.get(6)?,
                        detail: r
                            .get::<_, Option<String>>(7)?
                            .and_then(|d| serde_json::from_str(&d).ok()),
                    })
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }
}
