//! Schema v2: structured knowledge, in the same SQLite file as the conversation archive.
//!
//! Sharing the file is deliberate: a consolidation job can commit extracted knowledge and
//! release the transcript records it came from in one transaction (spec stage F/G).
//!
//! Relationships are claims whose object is an entity; `kn_relationships` is a view over
//! them rather than a second copy that could drift.

pub(crate) const V2: &str = "
CREATE TABLE kn_entities(
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    name_key TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','merged')),
    merged_into INTEGER REFERENCES kn_entities(id) ON DELETE SET NULL,
    scope TEXT NOT NULL,
    sensitivity INTEGER NOT NULL CHECK(sensitivity BETWEEN 0 AND 2),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK((status = 'merged') = (merged_into IS NOT NULL))
) STRICT;
CREATE INDEX kn_entities_by_name ON kn_entities(name_key, kind);

CREATE TABLE kn_aliases(
    entity_id INTEGER NOT NULL REFERENCES kn_entities(id) ON DELETE CASCADE,
    alias_key TEXT NOT NULL,
    alias TEXT NOT NULL,
    PRIMARY KEY(entity_id, alias_key)
) STRICT, WITHOUT ROWID;
CREATE INDEX kn_aliases_by_key ON kn_aliases(alias_key);

CREATE TABLE kn_evidence(
    id INTEGER PRIMARY KEY,
    source_kind TEXT NOT NULL CHECK(source_kind IN
        ('tool_observation','external_artifact','user_statement','model_inference','prior_memory')),
    source_ref TEXT,
    observed_at INTEGER NOT NULL,
    content_hash BLOB NOT NULL,
    origin_hash BLOB NOT NULL,
    receipt TEXT,
    verification TEXT,
    scope TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(source_kind, content_hash, scope)
) STRICT;
CREATE INDEX kn_evidence_by_origin ON kn_evidence(origin_hash);

CREATE TABLE kn_claims(
    id INTEGER PRIMARY KEY,
    subject_id INTEGER NOT NULL REFERENCES kn_entities(id) ON DELETE CASCADE,
    predicate TEXT NOT NULL,
    object_entity_id INTEGER REFERENCES kn_entities(id) ON DELETE CASCADE,
    object_value TEXT,
    category TEXT NOT NULL CHECK(category IN ('fact','preference','event','capability','question')),
    basis TEXT CHECK(basis IN ('explicit','observed','inferred','situational')),
    status TEXT NOT NULL CHECK(status IN
        ('verified','user_reported','inferred','unverified','disputed','superseded','invalidated')),
    inference_confidence REAL CHECK(inference_confidence IS NULL OR inference_confidence BETWEEN 0 AND 1),
    observed_at INTEGER NOT NULL,
    valid_from INTEGER,
    valid_until INTEGER,
    last_verified_at INTEGER,
    superseded_by INTEGER REFERENCES kn_claims(id) ON DELETE SET NULL,
    status_reason TEXT,
    retention TEXT NOT NULL CHECK(retention IN ('durable','transient')),
    expires_at INTEGER,
    scope TEXT NOT NULL,
    sensitivity INTEGER NOT NULL CHECK(sensitivity BETWEEN 0 AND 2),
    fingerprint BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK((object_entity_id IS NULL) <> (object_value IS NULL)),
    CHECK((category = 'preference') = (basis IS NOT NULL)),
    CHECK(retention <> 'transient' OR expires_at IS NOT NULL),
    CHECK(valid_until IS NULL OR valid_from IS NULL OR valid_until >= valid_from)
) STRICT;
CREATE INDEX kn_claims_by_subject ON kn_claims(subject_id, predicate, observed_at);
CREATE INDEX kn_claims_by_object ON kn_claims(object_entity_id) WHERE object_entity_id IS NOT NULL;
CREATE INDEX kn_claims_by_fingerprint ON kn_claims(fingerprint);

CREATE TABLE kn_claim_evidence(
    claim_id INTEGER NOT NULL REFERENCES kn_claims(id) ON DELETE CASCADE,
    evidence_id INTEGER NOT NULL REFERENCES kn_evidence(id) ON DELETE CASCADE,
    stance TEXT NOT NULL CHECK(stance IN ('supports','refutes')),
    PRIMARY KEY(claim_id, evidence_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX kn_claim_evidence_by_evidence ON kn_claim_evidence(evidence_id);

CREATE VIEW kn_relationships AS
    SELECT id, subject_id AS source_id, object_entity_id AS target_id, predicate AS relation,
           status, valid_from, valid_until, scope, sensitivity
    FROM kn_claims WHERE object_entity_id IS NOT NULL;

CREATE TABLE kn_procedures(
    id INTEGER PRIMARY KEY,
    entity_id INTEGER NOT NULL REFERENCES kn_entities(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK(version >= 1),
    spec TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('draft','active','deprecated')),
    created_at INTEGER NOT NULL,
    UNIQUE(entity_id, version)
) STRICT;

CREATE TABLE kn_procedure_runs(
    id INTEGER PRIMARY KEY,
    procedure_id INTEGER NOT NULL REFERENCES kn_procedures(id) ON DELETE CASCADE,
    outcome TEXT NOT NULL CHECK(outcome IN ('succeeded','failed','aborted')),
    evidence_id INTEGER NOT NULL REFERENCES kn_evidence(id) ON DELETE RESTRICT,
    notes TEXT,
    at INTEGER NOT NULL
) STRICT;

CREATE TABLE kn_project_items(
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES kn_entities(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN
        ('objective','constraint','decision','milestone','task','dependency','risk')),
    title TEXT NOT NULL,
    detail TEXT,
    state TEXT NOT NULL CHECK(state IN
        ('proposed','approved','in_progress','implemented','tested','deployed','failed','superseded')),
    state_evidence_id INTEGER REFERENCES kn_evidence(id) ON DELETE RESTRICT,
    superseded_by INTEGER REFERENCES kn_project_items(id) ON DELETE SET NULL,
    scope TEXT NOT NULL,
    sensitivity INTEGER NOT NULL CHECK(sensitivity BETWEEN 0 AND 2),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK(state NOT IN ('implemented','tested','deployed') OR state_evidence_id IS NOT NULL)
) STRICT;
CREATE INDEX kn_project_items_by_project ON kn_project_items(project_id, kind);

CREATE TABLE kn_operations(
    id INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    actor TEXT NOT NULL,
    op TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id INTEGER,
    reason TEXT,
    detail TEXT
) STRICT;

CREATE TABLE kn_tombstones(
    fingerprint BLOB PRIMARY KEY,
    deleted_at INTEGER NOT NULL,
    generation INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE kn_meta(
    key TEXT PRIMARY KEY,
    value INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
INSERT INTO kn_meta(key, value) VALUES('deletion_generation', 0);
";

pub(crate) const V3: &str = "
CREATE VIEW kn_search_documents AS
SELECT c.id, s.name || ' ' || COALESCE((SELECT group_concat(alias,' ') FROM kn_aliases WHERE entity_id=s.id),'') || ' ' || c.predicate || ' ' || COALESCE(c.object_value,o.name,'') AS text
FROM kn_claims c JOIN kn_entities s ON s.id=c.subject_id LEFT JOIN kn_entities o ON o.id=c.object_entity_id;
CREATE VIRTUAL TABLE kn_search USING fts5(text,content='kn_search_documents',content_rowid='id');
INSERT INTO kn_search(kn_search) VALUES('rebuild');
CREATE TABLE kn_vectors(
 claim_id INTEGER NOT NULL REFERENCES kn_claims(id) ON DELETE CASCADE,
 model TEXT NOT NULL, revision TEXT NOT NULL, text TEXT NOT NULL,
 vector TEXT NOT NULL, PRIMARY KEY(claim_id,model,revision)
) STRICT;
CREATE TRIGGER kn_search_insert AFTER INSERT ON kn_claims BEGIN
 INSERT INTO kn_search(rowid,text) SELECT id,text FROM kn_search_documents WHERE id=new.id;
END;
CREATE TRIGGER kn_search_delete BEFORE DELETE ON kn_claims BEGIN
 INSERT INTO kn_search(kn_search,rowid,text) SELECT 'delete',id,text FROM kn_search_documents WHERE id=old.id;
END;
CREATE TRIGGER kn_search_before_update BEFORE UPDATE ON kn_claims BEGIN
 INSERT INTO kn_search(kn_search,rowid,text) SELECT 'delete',id,text FROM kn_search_documents WHERE id=old.id;
 DELETE FROM kn_vectors WHERE claim_id=old.id;
END;
CREATE TRIGGER kn_search_update AFTER UPDATE ON kn_claims BEGIN
 INSERT INTO kn_search(rowid,text) SELECT id,text FROM kn_search_documents WHERE id=new.id;
END;
CREATE TRIGGER kn_search_entity_update AFTER UPDATE ON kn_entities BEGIN
 INSERT INTO kn_search(kn_search) VALUES('rebuild');
 DELETE FROM kn_vectors WHERE claim_id IN (SELECT id FROM kn_claims WHERE subject_id=new.id OR object_entity_id=new.id);
END;
CREATE TRIGGER kn_search_alias_insert AFTER INSERT ON kn_aliases BEGIN
 INSERT INTO kn_search(kn_search) VALUES('rebuild');
 DELETE FROM kn_vectors WHERE claim_id IN (SELECT id FROM kn_claims WHERE subject_id=new.entity_id);
END;
CREATE TRIGGER kn_search_alias_delete AFTER DELETE ON kn_aliases BEGIN
 INSERT INTO kn_search(kn_search) VALUES('rebuild');
 DELETE FROM kn_vectors WHERE claim_id IN (SELECT id FROM kn_claims WHERE subject_id=old.entity_id);
END;
";
