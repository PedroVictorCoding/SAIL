//! Arena Learn's persistent learning graph (#248).
//!
//! A proposition is an atomic claim; a chain records that a set of premise propositions
//! jointly entails (or supports) a conclusion. Both live in Arena, under `learning:<topic>`
//! scopes, with per-source provenance. This is deliberately separate from the lesson plan
//! DAG and from mastery scores.
//!
//! Deletion follows Arena's rules: forgetting bumps the deletion generation and writes a
//! tombstone; deleting an imported source removes only propositions that source alone
//! supports, because provenance rows are separate. A chain whose premise is forgotten is
//! removed by the schema's garbage-collection trigger, so it can never dangle.
use super::model::{Access, Sensitivity};
use super::{KnowledgeError, KnowledgeResult};
use crate::store::{MemoryStore, now_ms, sha16};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const SCOPE_PREFIX: &str = "learning:";
pub const MAX_TOPIC: usize = 80;
pub const MAX_STATEMENT: usize = 2_000;
pub const MAX_QUOTE: usize = 4_000;
pub const MAX_SOURCE_BYTES: usize = 6 * 1024 * 1024;
pub const MAX_CHAIN_PREMISES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainRelation {
    Entails,
    Supports,
}

impl ChainRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Entails => "entails",
            Self::Supports => "supports",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Proposition {
    pub id: i64,
    pub statement: String,
    pub topic: String,
    pub scope: String,
    pub sessions: Vec<String>,
    pub source_ids: Vec<i64>,
    pub quotes: Vec<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Chain {
    pub id: i64,
    pub conclusion: i64,
    pub premises: Vec<i64>,
    pub relation: String,
    pub topic: String,
    pub scope: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Source {
    pub id: i64,
    pub name: String,
    pub topic: String,
    pub scope: String,
    pub created_at: i64,
}

/// One retrieval result: a proposition plus the quote that best grounds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub id: i64,
    pub statement: String,
    pub topic: String,
    pub quote: String,
}

#[derive(Debug, Clone)]
pub struct NewProposition<'a> {
    pub statement: &'a str,
    pub topic: &'a str,
    pub quote: &'a str,
    pub session: Option<&'a str>,
    pub source_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewChain<'a> {
    pub conclusion: i64,
    pub premises: &'a [i64],
    pub relation: ChainRelation,
    pub session: Option<&'a str>,
}

pub fn learning_scope(topic: &str) -> KnowledgeResult<String> {
    let topic = topic.trim().to_lowercase().replace(' ', "-");
    if topic.is_empty()
        || topic.len() > MAX_TOPIC
        || !topic
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(KnowledgeError::Invalid(format!(
            "a learning topic must be 1-{MAX_TOPIC} letters, digits, `-` or `_`"
        )));
    }
    Ok(format!("{SCOPE_PREFIX}{topic}"))
}

pub fn is_learning_scope(scope: &str) -> bool {
    scope.starts_with(SCOPE_PREFIX) && scope.len() > SCOPE_PREFIX.len()
}

fn statement_key(statement: &str) -> String {
    statement
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn bounded(field: &str, text: &str, max: usize) -> KnowledgeResult<()> {
    if text.trim().is_empty() {
        return Err(KnowledgeError::Invalid(format!("`{field}` is required")));
    }
    if text.len() > max {
        return Err(KnowledgeError::Invalid(format!(
            "`{field}` is longer than {max} bytes"
        )));
    }
    Ok(())
}

fn scope_params(access: &Access) -> (i64, String, i64) {
    (
        access.scopes.is_none() as i64,
        serde_json::to_string(access.scopes.as_deref().unwrap_or(&[])).unwrap_or_default(),
        access.max_sensitivity as i64,
    )
}

/// SQL predicate for alias `p`; supports `learning:*` wildcards.
fn visible(alias: &str) -> String {
    format!(
        "(:all = 1 OR EXISTS (SELECT 1 FROM json_each(:scopes) s WHERE \
         {alias}.scope = s.value OR (substr(s.value, -1) = '*' AND {alias}.scope LIKE replace(s.value, '*', '%')))) \
         AND {alias}.sensitivity <= :maxsens"
    )
}

fn visible_scope(access: &Access, scope: &str) -> KnowledgeResult<()> {
    if access.allows(scope, Sensitivity::Private) {
        Ok(())
    } else {
        Err(KnowledgeError::Invalid(
            "the Learn grant does not cover this learning topic".into(),
        ))
    }
}

#[derive(Clone, Copy)]
pub struct LearningStore<'a> {
    store: &'a MemoryStore,
}

impl MemoryStore {
    pub fn learning(&self) -> LearningStore<'_> {
        LearningStore { store: self }
    }
}

impl<'a> LearningStore<'a> {
    fn read<T>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> KnowledgeResult<T>,
    ) -> KnowledgeResult<T> {
        self.store
            .with_reader(|conn| Ok(f(conn)))
            .map_err(KnowledgeError::from)?
    }

    /// Adds a proposition, or unions its provenance when an equivalent statement already
    /// exists. Exact/normalised statement dedupe; no model calls.
    pub fn add_proposition(
        &self,
        access: &Access,
        new: NewProposition<'_>,
    ) -> KnowledgeResult<Proposition> {
        bounded("statement", new.statement, MAX_STATEMENT)?;
        bounded("quote", new.quote, MAX_QUOTE)?;
        let scope = learning_scope(new.topic)?;
        visible_scope(access, &scope)?;
        let topic = scope.trim_start_matches(SCOPE_PREFIX).to_string();
        let statement = new.statement.trim().to_string();
        let key = statement_key(&statement);
        if key.is_empty() {
            return Err(KnowledgeError::Invalid(
                "`statement` has no words to index".into(),
            ));
        }
        let now = now_ms();
        let id = self.store.with_writer(|conn| {
            let tx = conn.transaction()?;
            if let Some(source_id) = new.source_id {
                let source_scope: Option<String> = tx
                    .query_row(
                        "SELECT scope FROM kn_learning_sources WHERE id=?1 AND status='active'",
                        [source_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if source_scope.as_deref() != Some(scope.as_str()) {
                    return Err(crate::store::StoreError(
                        "the import source does not belong to this learning topic".into(),
                    ));
                }
            }
            tx.execute(
                "INSERT INTO kn_learning_propositions
                 (statement, statement_key, topic, scope, sensitivity, status, created_at, updated_at)
                 VALUES(?1,?2,?3,?4,?5,'active',?6,?6)
                 ON CONFLICT(scope, statement_key) DO UPDATE SET updated_at=excluded.updated_at",
                params![
                    statement,
                    key,
                    topic,
                    scope,
                    Sensitivity::Private as i64,
                    now
                ],
            )?;
            let id: i64 = tx.query_row(
                "SELECT id FROM kn_learning_propositions WHERE scope=?1 AND statement_key=?2",
                params![scope, key],
                |r| r.get(0),
            )?;
            let provenance_key = match (new.session, new.source_id) {
                (Some(session), _) => format!("session:{session}"),
                (None, Some(source)) => format!("note:{source}"),
                (None, None) => "manual".to_string(),
            };
            tx.execute(
                "INSERT INTO kn_learning_provenance
                 (prop_id, provenance_key, source_kind, session, source_id, quote, created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(prop_id, provenance_key) DO UPDATE SET quote=excluded.quote",
                params![
                    id,
                    provenance_key,
                    if new.source_id.is_some() { "note" } else { "session" },
                    new.session,
                    new.source_id,
                    new.quote.trim(),
                    now
                ],
            )?;
            tx.execute(
                "INSERT INTO kn_operations(at,actor,op,target_kind,target_id,detail)
                 VALUES(?1,'subsystem:arena-learn','add_proposition','learning_proposition',?2,?3)",
                params![now, id, format!("{{\"scope\":\"{scope}\"}}")],
            )?;
            tx.commit()?;
            Ok(id)
        })?;
        self.proposition(access, id)
    }

    /// Adds or extends a chain. Premises are unioned, so a repeated extraction is idempotent.
    pub fn add_chain(&self, access: &Access, new: NewChain<'_>) -> KnowledgeResult<Chain> {
        if new.premises.is_empty() || new.premises.len() > MAX_CHAIN_PREMISES {
            return Err(KnowledgeError::Invalid(format!(
                "a chain needs 1 to {MAX_CHAIN_PREMISES} premises"
            )));
        }
        let (scope, topic) = self.proposition_scope(access, new.conclusion)?;
        let now = now_ms();
        let id = self.store.with_writer(|conn| {
            let tx = conn.transaction()?;
            let mut seen = BTreeSet::new();
            for premise in new.premises {
                if *premise == new.conclusion {
                    return Err(crate::store::StoreError(
                        "a proposition cannot entail itself".into(),
                    ));
                }
                if !seen.insert(*premise) {
                    return Err(crate::store::StoreError(
                        "a chain lists the same premise twice".into(),
                    ));
                }
                let premise_scope: Option<String> = tx
                    .query_row(
                        "SELECT scope FROM kn_learning_propositions WHERE id=?1 AND status='active'",
                        [*premise],
                        |r| r.get(0),
                    )
                    .optional()?;
                if premise_scope.as_deref() != Some(scope.as_str()) {
                    return Err(crate::store::StoreError(
                        "every premise must be an active proposition in the same topic".into(),
                    ));
                }
            }
            tx.execute(
                "INSERT INTO kn_learning_chains(conclusion_id,relation,topic,scope,sensitivity,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(conclusion_id, relation) DO NOTHING",
                params![
                    new.conclusion,
                    new.relation.as_str(),
                    topic,
                    scope,
                    Sensitivity::Private as i64,
                    now
                ],
            )?;
            let id: i64 = tx.query_row(
                "SELECT id FROM kn_learning_chains WHERE conclusion_id=?1 AND relation=?2",
                params![new.conclusion, new.relation.as_str()],
                |r| r.get(0),
            )?;
            for premise in new.premises {
                tx.execute(
                    "INSERT OR IGNORE INTO kn_learning_chain_premises(chain_id,premise_id) VALUES(?1,?2)",
                    params![id, premise],
                )?;
            }
            tx.execute(
                "INSERT INTO kn_operations(at,actor,op,target_kind,target_id,detail)
                 VALUES(?1,'subsystem:arena-learn','add_chain','learning_chain',?2,NULL)",
                params![now, id],
            )?;
            tx.commit()?;
            Ok(id)
        })?;
        self.chain(access, id)
    }

    pub fn propositions(
        &self,
        access: &Access,
        topic: Option<&str>,
    ) -> KnowledgeResult<Vec<Proposition>> {
        let (all, scopes, maxsens) = scope_params(access);
        let scope = topic.map(learning_scope).transpose()?;
        self.read(|conn| {
            let sql = format!(
                "SELECT p.id FROM kn_learning_propositions p
                 WHERE {} AND p.status='active' AND (:scope IS NULL OR p.scope=:scope)
                 ORDER BY p.created_at DESC, p.id DESC",
                visible("p")
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::named_params! {
                    ":all": all,
                    ":scopes": scopes,
                    ":maxsens": maxsens,
                    ":scope": scope,
                },
                |r| r.get::<_, i64>(0),
            )?;
            let mut out = Vec::new();
            for id in rows {
                out.push(load_proposition(conn, id?)?);
            }
            Ok(out)
        })
    }

    pub fn chains(&self, access: &Access, topic: Option<&str>) -> KnowledgeResult<Vec<Chain>> {
        let (all, scopes, maxsens) = scope_params(access);
        let scope = topic.map(learning_scope).transpose()?;
        self.read(|conn| {
            let sql = format!(
                "SELECT c.id FROM kn_learning_chains c
                 WHERE {} AND (:scope IS NULL OR c.scope=:scope)
                 ORDER BY c.created_at DESC, c.id DESC",
                visible("c")
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::named_params! {
                    ":all": all,
                    ":scopes": scopes,
                    ":maxsens": maxsens,
                    ":scope": scope,
                },
                |r| r.get::<_, i64>(0),
            )?;
            let mut out = Vec::new();
            for id in rows {
                out.push(load_chain(conn, id?)?);
            }
            Ok(out)
        })
    }

    /// Bounded retrieval for the learning agent's gated read tool (#251). Word overlap is
    /// enough here: the graph is small and the result is labelled untrusted data.
    pub fn search(
        &self,
        access: &Access,
        query: &str,
        topic: Option<&str>,
        limit: usize,
        budget_bytes: usize,
    ) -> KnowledgeResult<Vec<Hit>> {
        let terms: BTreeSet<String> = query
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() > 2)
            .map(str::to_string)
            .collect();
        if terms.is_empty() || limit == 0 || budget_bytes == 0 {
            return Ok(Vec::new());
        }
        let mut scored = Vec::new();
        for proposition in self.propositions(access, topic)? {
            let text = format!(
                "{} {}",
                proposition.statement.to_lowercase(),
                proposition.quotes.join(" ").to_lowercase()
            );
            let score = terms.iter().filter(|t| text.contains(t.as_str())).count();
            if score > 0 {
                scored.push((score, proposition));
            }
        }
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(b.1.created_at.cmp(&a.1.created_at))
                .then(b.1.id.cmp(&a.1.id))
        });
        let mut out = Vec::new();
        let mut used = 0usize;
        for (_, proposition) in scored.into_iter().take(limit) {
            let quote = proposition
                .quotes
                .first()
                .cloned()
                .unwrap_or_else(|| proposition.statement.clone());
            let cost = proposition.statement.len() + quote.len() + 32;
            if used + cost > budget_bytes {
                break;
            }
            used += cost;
            out.push(Hit {
                id: proposition.id,
                statement: proposition.statement,
                topic: proposition.topic,
                quote,
            });
        }
        Ok(out)
    }

    /// Deletes one proposition and everything that depended on it (#248).
    pub fn forget(&self, access: &Access, id: i64) -> KnowledgeResult<()> {
        let (scope, _) = self.proposition_scope(access, id)?;
        let _key: String = self.store.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let statement_key: String = tx.query_row(
                "SELECT statement_key FROM kn_learning_propositions WHERE id=?1",
                [id],
                |r| r.get(0),
            )?;
            let generation: i64 = tx.query_row(
                "SELECT value FROM kn_meta WHERE key='deletion_generation'",
                [],
                |r| r.get::<_, i64>(0),
            )? + 1;
            tx.execute(
                "UPDATE kn_meta SET value=?1 WHERE key='deletion_generation'",
                [generation],
            )?;
            let fingerprint = sha16(&[b"learning", scope.as_bytes(), statement_key.as_bytes()]);
            tx.execute(
                "INSERT INTO kn_tombstones(fingerprint,deleted_at,generation) VALUES(?1,?2,?3)
                 ON CONFLICT(fingerprint) DO UPDATE SET deleted_at=excluded.deleted_at, generation=excluded.generation",
                params![&fingerprint[..], now_ms(), generation],
            )?;
            tx.execute("DELETE FROM kn_learning_propositions WHERE id=?1", [id])?;
            tx.execute(
                "INSERT INTO kn_operations(at,actor,op,target_kind,target_id,reason)
                 VALUES(?1,'user','forget','learning_proposition',?2,'owner forget')",
                params![now_ms(), id],
            )?;
            tx.commit()?;
            Ok(statement_key)
        })?;
        Ok(())
    }

    /// Stores an imported note as a source with a content hash (#250).
    pub fn add_source(
        &self,
        access: &Access,
        name: &str,
        content: &str,
        topic: &str,
    ) -> KnowledgeResult<Source> {
        bounded("name", name, 300)?;
        if content.len() > MAX_SOURCE_BYTES {
            return Err(KnowledgeError::Invalid(format!(
                "a source may be at most {MAX_SOURCE_BYTES} bytes"
            )));
        }
        let scope = learning_scope(topic)?;
        visible_scope(access, &scope)?;
        let hash = sha16(&[b"learning-source", scope.as_bytes(), content.as_bytes()]);
        let now = now_ms();
        let id = self.store.with_writer(|conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO kn_learning_sources
                 (kind,name,content_hash,topic,scope,sensitivity,status,created_at)
                 VALUES('note',?1,?2,?3,?4,?5,'active',?6)
                 ON CONFLICT(scope, content_hash) DO NOTHING",
                params![
                    name.trim(),
                    &hash[..],
                    scope.trim_start_matches(SCOPE_PREFIX),
                    scope,
                    Sensitivity::Private as i64,
                    now
                ],
            )?;
            let id: i64 = tx.query_row(
                "SELECT id FROM kn_learning_sources WHERE scope=?1 AND content_hash=?2",
                params![scope, &hash[..]],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE kn_learning_sources SET status='active' WHERE id=?1",
                [id],
            )?;
            tx.commit()?;
            Ok(id)
        })?;
        self.source(access, id)
    }

    pub fn sources(&self, access: &Access, topic: Option<&str>) -> KnowledgeResult<Vec<Source>> {
        let (all, scopes, maxsens) = scope_params(access);
        let scope = topic.map(learning_scope).transpose()?;
        self.read(|conn| {
            let sql = format!(
                "SELECT s.id,s.name,s.topic,s.scope,s.created_at FROM kn_learning_sources s
                 WHERE s.status='active' AND {} AND (:scope IS NULL OR s.scope=:scope)
                 ORDER BY s.created_at DESC, s.id DESC",
                visible("s")
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::named_params! {
                    ":all": all, ":scopes": scopes, ":maxsens": maxsens, ":scope": scope,
                },
                |r| {
                    Ok(Source {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        topic: r.get(2)?,
                        scope: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                },
            )?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(KnowledgeError::from)
        })
    }

    pub fn source(&self, access: &Access, id: i64) -> KnowledgeResult<Source> {
        let (all, scopes, maxsens) = scope_params(access);
        self.read(|conn| {
            let sql = format!(
                "SELECT s.id,s.name,s.topic,s.scope,s.created_at FROM kn_learning_sources s
                 WHERE s.id=:id AND s.status='active' AND {}",
                visible("s")
            );
            conn.query_row(
                &sql,
                rusqlite::named_params! {
                    ":all": all, ":scopes": scopes, ":maxsens": maxsens, ":id": id,
                },
                |r| {
                    Ok(Source {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        topic: r.get(2)?,
                        scope: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(KnowledgeError::from)?
            .ok_or(KnowledgeError::NotFound("learning source", id))
        })
    }

    /// Removes a source; propositions that other provenance still supports survive.
    pub fn delete_source(&self, access: &Access, id: i64) -> KnowledgeResult<()> {
        let source = self.source(access, id)?;
        visible_scope(access, &source.scope)?;
        self.store
            .with_writer(|conn| {
                let tx = conn.transaction()?;
                tx.execute("DELETE FROM kn_learning_sources WHERE id=?1", [id])?;
                tx.execute(
                    "INSERT INTO kn_operations(at,actor,op,target_kind,target_id,reason)
                     VALUES(?1,'user','delete_learning_source','learning_source',?2,'owner delete')",
                    params![now_ms(), id],
                )?;
                tx.commit()?;
                Ok(())
            })
            .map_err(KnowledgeError::from)?;
        Ok(())
    }

    pub fn covered_through(&self, session: &str) -> KnowledgeResult<i64> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT covered_seq FROM kn_learning_coverage
                     WHERE session=?1 AND extractor_version=1",
                    [session],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0))
        })
    }

    pub fn set_covered_through(&self, session: &str, seq: i64) -> KnowledgeResult<()> {
        self.store
            .with_writer(|conn| {
                conn.execute(
                    "INSERT INTO kn_learning_coverage(session,extractor_version,covered_seq,updated_at)
                     VALUES(?1,1,?2,?3)
                     ON CONFLICT(session,extractor_version) DO UPDATE SET
                       covered_seq=MAX(covered_seq,excluded.covered_seq), updated_at=excluded.updated_at",
                    params![session, seq, now_ms()],
                )?;
                Ok(())
            })
            .map_err(KnowledgeError::from)?;
        Ok(())
    }

    fn proposition(&self, access: &Access, id: i64) -> KnowledgeResult<Proposition> {
        let (all, scopes, maxsens) = scope_params(access);
        self.read(|conn| {
            let sql = format!(
                "SELECT p.id FROM kn_learning_propositions p WHERE p.id=:id AND p.status='active' AND {}",
                visible("p")
            );
            let found: Option<i64> = conn
                .query_row(
                    &sql,
                    rusqlite::named_params! {
                        ":all": all, ":scopes": scopes, ":maxsens": maxsens, ":id": id,
                    },
                    |r| r.get(0),
                )
                .optional()?;
            match found {
                Some(id) => load_proposition(conn, id),
                None => Err(KnowledgeError::NotFound("learning proposition", id)),
            }
        })
    }

    fn chain(&self, access: &Access, id: i64) -> KnowledgeResult<Chain> {
        let (all, scopes, maxsens) = scope_params(access);
        self.read(|conn| {
            let sql = format!(
                "SELECT c.id FROM kn_learning_chains c WHERE c.id=:id AND {}",
                visible("c")
            );
            let found: Option<i64> = conn
                .query_row(
                    &sql,
                    rusqlite::named_params! {
                        ":all": all, ":scopes": scopes, ":maxsens": maxsens, ":id": id,
                    },
                    |r| r.get(0),
                )
                .optional()?;
            match found {
                Some(id) => load_chain(conn, id),
                None => Err(KnowledgeError::NotFound("learning chain", id)),
            }
        })
    }

    fn proposition_scope(&self, access: &Access, id: i64) -> KnowledgeResult<(String, String)> {
        let (all, scopes, maxsens) = scope_params(access);
        let row = self.store.with_reader(|conn| {
            let sql = format!(
                "SELECT p.scope,p.topic FROM kn_learning_propositions p
                 WHERE p.id=:id AND p.status='active' AND {}",
                visible("p")
            );
            Ok(conn
                .query_row(
                    &sql,
                    rusqlite::named_params! {
                        ":all": all, ":scopes": scopes, ":maxsens": maxsens, ":id": id,
                    },
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?)
        })?;
        row.ok_or(KnowledgeError::NotFound("learning proposition", id))
    }
}

fn load_proposition(conn: &rusqlite::Connection, id: i64) -> KnowledgeResult<Proposition> {
    let (statement, topic, scope, created_at) = conn.query_row(
        "SELECT statement,topic,scope,created_at FROM kn_learning_propositions
         WHERE id=?1 AND status='active'",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let mut stmt = conn.prepare_cached(
        "SELECT source_kind,session,source_id,quote FROM kn_learning_provenance
         WHERE prop_id=?1 ORDER BY created_at, provenance_key",
    )?;
    let rows = stmt.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut sessions = Vec::new();
    let mut source_ids = Vec::new();
    let mut quotes = Vec::new();
    for row in rows {
        let (kind, session, source_id, quote) = row?;
        if kind == "session" {
            if let Some(session) = session {
                sessions.push(session);
            }
        } else if let Some(source_id) = source_id {
            source_ids.push(source_id);
        }
        if !quotes.contains(&quote) {
            quotes.push(quote);
        }
    }
    Ok(Proposition {
        id,
        statement,
        topic,
        scope,
        sessions,
        source_ids,
        quotes,
        created_at,
    })
}

fn load_chain(conn: &rusqlite::Connection, id: i64) -> KnowledgeResult<Chain> {
    let (conclusion, relation, topic, scope, created_at) = conn.query_row(
        "SELECT conclusion_id,relation,topic,scope,created_at FROM kn_learning_chains WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let mut stmt = conn.prepare_cached(
        "SELECT premise_id FROM kn_learning_chain_premises WHERE chain_id=?1 ORDER BY premise_id",
    )?;
    let premises = stmt
        .query_map([id], |r| r.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(Chain {
        id,
        conclusion,
        premises,
        relation,
        topic,
        scope,
        created_at,
    })
}
