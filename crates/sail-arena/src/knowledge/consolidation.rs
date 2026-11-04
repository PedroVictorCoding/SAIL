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

/// Normalize a learned concept or webpage title: decode common HTML entities, unify quotes,
/// strip markdown markers, and remove trailing site suffixes like `" — LessWrong"`.
pub fn normalize_concept_name(raw: &str) -> String {
    let mut s = raw
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace(['’', '‘'], "'")
        .replace(['“', '”'], "\"");
    s = s
        .trim()
        .trim_matches(|c: char| matches!(c, '*' | '#' | '"' | '\'' | ':' | '-' | ' '))
        .to_string();
    if let Some(rest) = s.strip_prefix("Reading ").or_else(|| s.strip_prefix("reading ")) {
        s = rest.trim().to_string();
    }
    for sep in [" — ", " – ", " - ", " | ", " · ", " from "] {
        if let Some((left, right)) = s.rsplit_once(sep) {
            let left = left.trim();
            let right = right.trim();
            if left.chars().count() >= 3 && right.chars().count() <= 40 {
                s = left.to_string();
                break;
            }
        }
    }
    s.trim_matches(|c: char| matches!(c, '*' | '#' | '"' | '\'' | ':' | ' '))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Generate search/lookup aliases for a concept name and optional URL `source_ref` so that
/// queries with or without apostrophes (e.g. `"Roko's Basilisk"` vs `"Rokos Basilisk"`) or
/// using the URL slug (`"rokos-basilisk"`) resolve to the same entity in memory.
pub fn compute_concept_aliases(
    name: &str,
    extra: &[String],
    source_ref: Option<&str>,
) -> Vec<String> {
    let canonical_key = name_key(name);
    let mut out: Vec<String> = Vec::new();
    let mut push = |candidate: String| {
        let trimmed = candidate.trim().to_string();
        let key = name_key(&trimmed);
        if !trimmed.is_empty()
            && trimmed.chars().count() <= 200
            && key != canonical_key
            && !out.iter().any(|existing| name_key(existing) == key)
        {
            out.push(trimmed);
        }
    };

    for item in extra {
        push(item.clone());
    }

    if name.contains('\'') || name.contains('’') {
        let stripped: String = name
            .chars()
            .filter(|&c| c != '\'' && c != '’')
            .collect();
        push(stripped);
    } else {
        // E.g. "Rokos Basilisk" -> "Roko's Basilisk"
        let words: Vec<&str> = name.split_whitespace().collect();
        if words.len() >= 2 && words[0].len() >= 4 && words[0].ends_with('s') && !words[0].ends_with("ss") {
            let stem = &words[0][..words[0].len() - 1];
            let mut possessive = format!("{stem}'s");
            for w in &words[1..] {
                possessive.push(' ');
                possessive.push_str(w);
            }
            push(possessive);
        }
    }

    if let Some(src) = source_ref
        && (src.starts_with("http://") || src.starts_with("https://"))
    {
        let trimmed = src.trim_end_matches('/');
        if let Some(slug) = trimmed.rsplit('/').next() {
            let slug = slug.split(['?', '#']).next().unwrap_or(slug).trim();
            if slug.len() >= 3 && slug.len() <= 120 {
                push(slug.to_string());
                let spaced = slug.replace(['-', '_'], " ");
                push(spaced);
            }
        }
    }

    out
}

fn first_url(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let clean = token.trim_matches(|c: char| {
            matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | ',' | '.' | ';')
        });
        if (clean.starts_with("https://") || clean.starts_with("http://")) && clean.len() > 10 {
            return Some(clean.to_string());
        }
    }
    None
}

fn strip_markdown_inline(text: &str) -> String {
    text.replace("**", "")
        .replace("__", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn slug_to_title(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let slug = trimmed.rsplit('/').next()?.split(['?', '#']).next()?.trim();
    if slug.len() < 3 {
        return None;
    }
    let words: Vec<String> = slug
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

fn is_generic_heading(heading: &str) -> bool {
    matches!(
        heading.to_ascii_lowercase().as_str(),
        "key points"
            | "key points from the page"
            | "related concepts"
            | "summary"
            | "overview"
            | "introduction"
            | "conclusion"
            | "background"
            | "details"
            | "notes"
    )
}

/// Extract a learned concept/webpage candidate from a full `User: …\nSAIL: …` exchange when
/// the user asked to read a webpage or explain a concept and the reply provides a structured
/// explanation or summary.
pub(crate) fn extract_learned_turn(body: &str) -> Option<Candidate> {
    if body.matches("\nSAIL: ").count() != 1 {
        return None;
    }
    let (user_raw, assistant_raw) = body.strip_prefix("User: ")?.split_once("\nSAIL: ")?;
    let user = user_raw.trim();
    let assistant = assistant_raw.trim();
    if user.is_empty() || assistant.len() < 40 {
        return None;
    }

    let url = first_url(user).or_else(|| first_url(assistant));
    let user_lower = user.to_ascii_lowercase();
    let asks_to_learn = url.is_some()
        || user_lower.starts_with("read ")
        || user_lower.starts_with("what is ")
        || user_lower.starts_with("who is ")
        || user_lower.starts_with("explain ")
        || user_lower.starts_with("define ")
        || user_lower.starts_with("summarize ")
        || user_lower.contains("read me ")
        || user_lower.contains("read this ");
    if !asks_to_learn {
        return None;
    }

    let mut subject: Option<String> = None;
    let mut paragraphs: Vec<String> = Vec::new();

    // 1. Check for `titled **"Title"**` or `titled "Title"`
    for marker in ["titled **\"", "titled \"", "titled **'", "titled '"] {
        if let Some(idx) = assistant.find(marker) {
            let after = &assistant[idx + marker.len()..];
            if let Some(end) = after.find(['"', '\'']) {
                let candidate = normalize_concept_name(&after[..end]);
                if candidate.chars().count() >= 2 && candidate.chars().count() <= 120 {
                    subject = Some(candidate);
                    break;
                }
            }
        }
    }

    // 2. Check for `Reading <Title>:` intro line from web_speak
    if subject.is_none()
        && let Some(first_line) = assistant.lines().find(|l| !l.trim().is_empty())
    {
        let trimmed = first_line.trim();
        if (trimmed.starts_with("Reading ") || trimmed.starts_with("reading "))
            && trimmed.ends_with(':')
        {
            let candidate = normalize_concept_name(trimmed);
            if candidate.chars().count() >= 2 && candidate.chars().count() <= 120 {
                subject = Some(candidate);
            }
        }
    }

    // 3. Collect headings and substantive paragraphs from the assistant reply
    for block in assistant.split("\n\n") {
        let block = block.trim();
        if block.is_empty() || block == "---" {
            continue;
        }
        if let Some(heading) = block.strip_prefix('#') {
            let heading = normalize_concept_name(heading);
            if subject.is_none()
                && heading.chars().count() >= 2
                && heading.chars().count() <= 120
                && !is_generic_heading(&heading)
            {
                subject = Some(heading);
            }
            continue;
        }
        if subject.is_none()
            && block.starts_with("**")
            && let Some(end) = block[2..].find("**")
        {
            let bold_lead = normalize_concept_name(&block[2..2 + end]);
            let after = block[2 + end + 2..].trim_start();
            if (after.starts_with("is ") || after.starts_with("was ") || after.starts_with("refers to "))
                && bold_lead.chars().count() >= 2
                && bold_lead.chars().count() <= 120
            {
                subject = Some(bold_lead);
            }
        }
        let cleaned = strip_markdown_inline(block);
        let lower = cleaned.to_ascii_lowercase();
        if lower.starts_with("here is the content")
            || lower.starts_with("let me know if you")
            || lower.starts_with("reading ")
            || cleaned.chars().count() < 40
        {
            continue;
        }
        paragraphs.push(cleaned);
    }

    if subject.is_none()
        && let Some(ref u) = url
    {
        subject = slug_to_title(u);
    }

    let subject = normalize_concept_name(&subject?);
    if subject.is_empty() || subject.chars().count() > 160 || paragraphs.is_empty() {
        return None;
    }

    let mut summary = paragraphs[0].clone();
    if summary.chars().count() < 180 && paragraphs.len() > 1 {
        summary.push(' ');
        summary.push_str(&paragraphs[1]);
    }
    let mut summary = crate::compaction::clip(&summary, 420);
    if let Some(ref u) = url
        && !summary.contains(u.as_str())
    {
        summary = crate::compaction::clip(&format!("{summary} (source: {u})"), 500);
    }

    Some(Candidate {
        name: subject,
        kind: EntityKind::Concept,
        entity_id: None,
        predicate: "summary".into(),
        value: summary,
        category: ClaimCategory::Fact,
        correction: false,
        direct: false,
        source_ref: url,
    })
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
    let explicit = if let Some(value) = user.strip_prefix("I prefer ") {
        Some(("Owner", "prefers", value, ClaimCategory::Preference, false))
    } else if let Some(value) = user.strip_prefix("I now prefer ") {
        Some(("Owner", "prefers", value, ClaimCategory::Preference, true))
    } else {
        let parsed = if let Some(text) = user.strip_prefix("Remember: ") {
            Some((text, false, ClaimCategory::Fact))
        } else if let Some(text) = user.strip_prefix("Correction: ") {
            Some((text, true, ClaimCategory::Fact))
        } else {
            user.strip_prefix("Decision: ")
                .map(|text| (text, false, ClaimCategory::Event))
        };
        parsed.and_then(|(text, correction, category)| {
            let (name, value) = text.split_once(" is ")?;
            Some((
                name,
                if category == ClaimCategory::Event {
                    "decision"
                } else {
                    "is"
                },
                value,
                category,
                correction,
            ))
        })
    };
    if let Some((name, predicate, value, category, correction)) = explicit {
        if name.trim().is_empty() || value.trim().is_empty() {
            return None;
        }
        return Some(Candidate {
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
        });
    }
    extract_learned_turn(body)
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
    /// Check whether an active concept/entity or URL `source_ref` has already been learned in `scope`.
    pub fn has_learned_knowledge(
        &self,
        scope: &str,
        name: &str,
        source_ref: Option<&str>,
    ) -> KnowledgeResult<bool> {
        Ok(self
            .find_learned_claim(scope, name, &[], "summary", source_ref)?
            .is_some())
    }

    /// Find an existing active claim ID in `scope` if this concept/name/alias or URL `source_ref`
    /// has already been learned.
    pub fn find_learned_claim(
        &self,
        scope: &str,
        name: &str,
        extra_aliases: &[String],
        predicate: &str,
        source_ref: Option<&str>,
    ) -> KnowledgeResult<Option<i64>> {
        let clean = normalize_concept_name(name);
        let aliases = compute_concept_aliases(&clean, extra_aliases, source_ref);
        self.read(|conn| find_existing_learned_claim_in_conn(conn, scope, &clean, &aliases, predicate, source_ref))
    }

    /// Add learned knowledge to memory at most once: checks if an active claim for the concept
    /// (by canonical name, alias, or URL `source_ref`) already exists in `scope`. Returns
    /// `(claim_id, true)` when newly added, or `(existing_claim_id, false)` when already present.
    #[allow(clippy::too_many_arguments)]
    pub fn learn_once(
        &self,
        scope: &str,
        name: &str,
        extra_aliases: &[String],
        kind: EntityKind,
        predicate: &str,
        value: &str,
        source_ref: Option<&str>,
        direct: bool,
    ) -> KnowledgeResult<(i64, bool)> {
        let clean_name = normalize_concept_name(name);
        let clean_value = value.trim();
        if clean_name.is_empty() || clean_value.is_empty() {
            return Err(KnowledgeError::Invalid(
                "learned knowledge requires non-empty name and value".into(),
            ));
        }
        let aliases = compute_concept_aliases(&clean_name, extra_aliases, source_ref);
        self.write(Actor::subsystem("learner"), |tx| {
            if let Some(existing_claim) = find_existing_learned_claim_in_conn(
                tx.conn,
                scope,
                &clean_name,
                &aliases,
                predicate,
                source_ref,
            )? {
                if let Ok(subject_id) = tx.conn.query_row(
                    "SELECT subject_id FROM kn_claims WHERE id=?1",
                    [existing_claim],
                    |r| r.get::<_, i64>(0),
                ) {
                    for alias in &aliases {
                        let _ = tx.add_alias(subject_id, alias);
                    }
                }
                return Ok((existing_claim, false));
            }
            let observed_at = crate::store::now_ms();
            let src_ref = source_ref.unwrap_or("conversation:learned");
            let evidence = tx.add_evidence(&NewEvidence {
                source_kind: if direct {
                    SourceKind::ToolObservation
                } else {
                    SourceKind::UserStatement
                },
                source_ref: Some(src_ref),
                observed_at,
                content: clean_value,
                receipt: Some(clean_value),
                derived_from: None,
                verification: None,
                scope,
            })?;
            let candidate = Candidate {
                name: clean_name,
                kind,
                entity_id: None,
                predicate: predicate.into(),
                value: clean_value.into(),
                category: ClaimCategory::Fact,
                correction: false,
                direct,
                source_ref: source_ref.map(str::to_string),
            };
            let claim = apply_candidate(
                tx,
                &Access::owner(),
                scope,
                &candidate,
                evidence,
                observed_at,
                if direct {
                    EpistemicStatus::Verified
                } else {
                    EpistemicStatus::UserReported
                },
            )?;
            Ok((claim, true))
        })
    }
}

fn find_existing_learned_claim_in_conn(
    conn: &rusqlite::Connection,
    scope: &str,
    clean_name: &str,
    aliases: &[String],
    predicate: &str,
    source_ref: Option<&str>,
) -> KnowledgeResult<Option<i64>> {
    if let Some(src) = source_ref
        && (src.starts_with("http://") || src.starts_with("https://"))
    {
        let by_url: Option<i64> = conn
            .query_row(
                "SELECT c.id FROM kn_claims c
                 JOIN kn_claim_evidence ce ON ce.claim_id=c.id
                 JOIN kn_evidence ev ON ev.id=ce.evidence_id
                 WHERE c.scope=?1 AND c.status NOT IN ('superseded','invalidated')
                   AND ev.source_ref=?2
                 ORDER BY c.id LIMIT 1",
                params![scope, src],
                |r| r.get(0),
            )
            .optional()?;
        if by_url.is_some() {
            return Ok(by_url);
        }
    }
    let mut keys = vec![name_key(clean_name)];
    for alias in aliases {
        let k = name_key(alias);
        if !k.is_empty() && !keys.contains(&k) {
            keys.push(k);
        }
    }
    for key in &keys {
        let claim: Option<i64> = conn
            .query_row(
                "SELECT c.id FROM kn_claims c
                 JOIN kn_entities e ON e.id=c.subject_id
                 LEFT JOIN kn_aliases a ON a.entity_id=e.id
                 WHERE e.status='active' AND e.scope=?1 AND c.scope=?1
                   AND (e.name_key=?2 OR a.alias_key=?2)
                   AND (c.predicate=?3 OR (?3='summary' AND c.predicate IN ('summary','is')))
                   AND c.status NOT IN ('superseded','invalidated')
                 ORDER BY c.id LIMIT 1",
                params![scope, key, predicate],
                |r| r.get(0),
            )
            .optional()?;
        if claim.is_some() {
            return Ok(claim);
        }
    }
    Ok(None)
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
    let aliases = if c.predicate == "summary" {
        compute_concept_aliases(&c.name, &[], c.source_ref.as_deref())
    } else {
        Vec::new()
    };
    if c.predicate == "summary"
        && let Some(existing_claim) = find_existing_learned_claim_in_conn(
            tx.conn,
            scope,
            &c.name,
            &aliases,
            &c.predicate,
            c.source_ref.as_deref(),
        )?
    {
        if let Ok(subject_id) = tx.conn.query_row(
            "SELECT subject_id FROM kn_claims WHERE id=?1",
            [existing_claim],
            |r| r.get::<_, i64>(0),
        ) {
            for alias in &aliases {
                let _ = tx.add_alias(subject_id, alias);
            }
        }
        return Ok(existing_claim);
    }
    let mut lookup_keys = vec![name_key(&c.name)];
    for alias in &aliases {
        let k = name_key(alias);
        if !k.is_empty() && !lookup_keys.contains(&k) {
            lookup_keys.push(k);
        }
    }
    let mut matches: Vec<i64> = Vec::new();
    {
        let mut stmt = tx.conn.prepare("SELECT DISTINCT e.id FROM kn_entities e LEFT JOIN kn_aliases a ON a.entity_id=e.id WHERE e.status='active' AND e.scope=?1 AND e.kind=?2 AND e.sensitivity<=?4 AND (e.name_key=?3 OR a.alias_key=?3) ORDER BY e.id")?;
        for key in &lookup_keys {
            let rows = stmt
                .query_map(
                    params![scope, c.kind.as_str(), key, access.max_sensitivity as i64],
                    |r| r.get::<_, i64>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            for id in rows {
                if !matches.contains(&id) {
                    matches.push(id);
                }
            }
        }
    }
    let subject = if let Some(id) = c.entity_id {
        let valid = tx.conn.query_row("SELECT id FROM kn_entities WHERE id=?1 AND scope=?2 AND sensitivity<=?3 AND status='active'",params![id,scope,access.max_sensitivity as i64],|r|r.get::<_,i64>(0)).optional()?;
        valid.ok_or(KnowledgeError::NotFound("entity", id))?
    } else if matches.len() == 1 {
        let existing_id = matches[0];
        for alias in &aliases {
            let _ = tx.add_alias(existing_id, alias);
        }
        existing_id
    } else {
        let alias_refs: Vec<&str> = aliases.iter().map(String::as_str).collect();
        let entity = tx.create_entity(&NewEntity {
            kind: c.kind,
            name: &c.name,
            aliases: &alias_refs,
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
