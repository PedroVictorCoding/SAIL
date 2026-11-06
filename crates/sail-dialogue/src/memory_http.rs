//! Owner-authenticated HTTP API behind the Memory views in Arena's workspace.
//!
//! Every route needs the owner bearer token, and every read uses the owner's full access.
//! Changes are made as `Actor::User`, so they are logged as the owner's and may teach back
//! knowledge the owner deleted earlier. Store work runs on the blocking pool.
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sail_arena::MemoryStore;
use sail_arena::knowledge::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct MemoryHttpState {
    store: Arc<MemoryStore>,
    authority: Option<sail_guardian::authority::ApprovalAuthority>,
}

pub fn router(
    store: Arc<MemoryStore>,
    authority: Option<sail_guardian::authority::ApprovalAuthority>,
) -> Router {
    Router::new()
        .route("/overview", get(overview))
        .route("/search", get(search))
        .route("/entities/:id", get(entity_detail))
        .route("/graph/:id", get(graph))
        .route("/changes", get(changes))
        .route("/export", get(export))
        .route("/teach", post(teach))
        .route("/claims/:id/confirm", post(confirm_claim))
        .route("/claims/:id/correct", post(correct_claim))
        .route("/claims/:id/invalidate", post(invalidate_claim))
        .route("/claims/:id/forget", post(forget_claim))
        .route("/claims/:id/pin", post(pin_claim))
        .route("/claims/:id/private", post(private_claim))
        .route("/entities/:id/forget", post(forget_entity))
        .route("/entities/:id/merge", post(merge_entity))
        .route("/entities/:id/separate", post(separate_entity))
        .route("/entities/:id/private", post(private_entity))
        .with_state(Arc::new(MemoryHttpState { store, authority }))
}

struct ApiError(StatusCode, Value);

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self(status, json!({"error": message.into()}))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, [("cache-control", "no-store")], Json(self.1)).into_response()
    }
}

impl From<KnowledgeError> for ApiError {
    fn from(e: KnowledgeError) -> Self {
        let status = match &e {
            KnowledgeError::NotFound(..) => StatusCode::NOT_FOUND,
            KnowledgeError::Invalid(_)
            | KnowledgeError::InsufficientEvidence(_)
            | KnowledgeError::Tombstoned => StatusCode::UNPROCESSABLE_ENTITY,
            KnowledgeError::StaleGeneration { .. } => StatusCode::CONFLICT,
            KnowledgeError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(status, e.to_string())
    }
}

type ApiResult = Result<Response, ApiError>;

fn ok(value: Value) -> ApiResult {
    Ok(([("cache-control", "no-store")], Json(value)).into_response())
}

fn authenticate(state: &MemoryHttpState, headers: &HeaderMap) -> Result<(), ApiError> {
    let authority = state.authority.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Owner authentication is not configured",
        )
    })?;
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if authority.authenticates(token) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "Owner token not accepted",
        ))
    }
}

/// Authenticate, then run `f` against the store on the blocking pool.
async fn with_store(
    state: Arc<MemoryHttpState>,
    headers: HeaderMap,
    f: impl FnOnce(&MemoryStore) -> Result<Value, ApiError> + Send + 'static,
) -> ApiResult {
    authenticate(&state, &headers)?;
    let value = tokio::task::spawn_blocking(move || f(&state.store))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    ok(value)
}

fn owner() -> Access {
    Access::owner()
}

fn reason(text: &Option<String>) -> String {
    text.as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("Changed by the owner in Memory")
        .chars()
        .take(500)
        .collect()
}

fn summaries(
    kn: KnowledgeStore<'_>,
    categories: Vec<ClaimCategory>,
    statuses: Vec<EpistemicStatus>,
    limit: usize,
) -> KnowledgeResult<Vec<ClaimSummary>> {
    kn.list_claims(
        &ClaimFilter {
            categories,
            statuses,
            limit,
        },
        &owner(),
    )
}

async fn overview(State(state): State<Arc<MemoryHttpState>>, headers: HeaderMap) -> ApiResult {
    with_store(state, headers, |store| {
        let kn = store.knowledge();
        let a = owner();
        Ok(json!({
            "counts": kn.counts(&a)?,
            "projects": kn.list_entities(&[EntityKind::Project], &a, 12)?,
            "skills": kn.list_entities(&[EntityKind::Procedure], &a, 12)?,
            "preferences": summaries(kn, vec![ClaimCategory::Preference], vec![], 20)?,
            "questions": summaries(kn, vec![ClaimCategory::Question], vec![], 20)?,
            "needs_review": summaries(
                kn,
                vec![],
                vec![EpistemicStatus::Inferred, EpistemicStatus::Unverified, EpistemicStatus::Disputed],
                20,
            )?,
            "recent": kn.list_entities(&[], &a, 12)?,
        }))
    })
    .await
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        let q: String = query.q.chars().take(200).collect();
        Ok(json!({"entities": store.knowledge().search_entities(&q, &owner(), 25)?}))
    })
    .await
}

fn object_name(kn: KnowledgeStore<'_>, object: &ClaimObject) -> KnowledgeResult<Option<String>> {
    Ok(match object {
        ClaimObject::Entity(id) => kn.entity(*id, &owner())?.map(|e| e.name),
        ClaimObject::Value(_) => None,
    })
}

async fn entity_detail(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        let kn = store.knowledge();
        let a = owner();
        let entity = kn
            .entity(id, &a)?
            .ok_or(KnowledgeError::NotFound("entity", id))?;
        let mut claims = Vec::new();
        for claim in kn.claims_about(id, &a, true)? {
            claims.push(json!({
                "object_name": object_name(kn, &claim.object)?,
                "evidence": kn.evidence(claim.id, &a)?,
                "support": kn.support(claim.id, &a)?,
                "changes": kn.operations_for("claim", claim.id, 20)?,
                "claim": claim,
            }));
        }
        let mut procedures = Vec::new();
        for procedure in kn.procedure_versions(id, &a)? {
            procedures.push(json!({"runs": kn.procedure_runs(procedure.id, &a)?, "procedure": procedure}));
        }
        let canonical = match entity.merged_into {
            Some(into) => kn.entity(into, &a)?,
            None => None,
        };
        Ok(json!({
            "entity": entity,
            "canonical": canonical,
            "claims": claims,
            "edges": kn.neighbors(id, &a, true)?,
            "merged": kn.merged_into(id, &a)?,
            "project_items": kn.project_items(id, &a)?,
            "procedures": procedures,
            "changes": kn.operations_for("entity", id, 20)?,
        }))
    })
    .await
}

#[derive(Deserialize)]
struct GraphQuery {
    #[serde(default)]
    history: bool,
}

async fn graph(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<GraphQuery>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        let kn = store.knowledge();
        let center = kn
            .entity(id, &owner())?
            .ok_or(KnowledgeError::NotFound("entity", id))?;
        Ok(json!({"center": center, "edges": kn.neighbors(id, &owner(), query.history)?}))
    })
    .await
}

#[derive(Deserialize)]
struct ChangesQuery {
    limit: Option<usize>,
}

async fn changes(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Query(query): Query<ChangesQuery>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        let kn = store.knowledge();
        let a = owner();
        let mut out = Vec::new();
        for op in kn.operations(query.limit.unwrap_or(100).clamp(1, 500))? {
            // Label records that still exist; deleted ones keep only their id.
            let label = match (op.target_kind.as_str(), op.target_id) {
                ("entity", Some(id)) => kn.entity(id, &a)?.map(|e| e.name),
                ("claim", Some(id)) => claim_label(kn, id)?,
                _ => None,
            };
            out.push(json!({"label": label, "operation": op}));
        }
        Ok(json!({"changes": out}))
    })
    .await
}

fn claim_label(kn: KnowledgeStore<'_>, claim: i64) -> KnowledgeResult<Option<String>> {
    Ok(kn.claim(claim, &owner())?.map(|c| {
        format!(
            "{} · {}",
            c.subject_name,
            c.claim.predicate.replace('_', " ")
        )
    }))
}

async fn export(State(state): State<Arc<MemoryHttpState>>, headers: HeaderMap) -> ApiResult {
    authenticate(&state, &headers)?;
    let value = tokio::task::spawn_blocking(move || state.store.knowledge().export(&owner()))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok((
        [
            ("cache-control", "no-store"),
            (
                "content-disposition",
                "attachment; filename=\"sail-memory-export.json\"",
            ),
        ],
        Json(value),
    )
        .into_response())
}

#[derive(Deserialize)]
struct TeachInput {
    subject: String,
    #[serde(default)]
    subject_id: Option<i64>,
    #[serde(default)]
    kind: Option<String>,
    predicate: String,
    value: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    private: bool,
}

/// "favourite colour" → "favourite_colour"; `None` if nothing usable remains.
fn predicate_from(text: &str) -> Option<String> {
    let mut out = String::new();
    for c in text.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_string();
    let out = if out.starts_with(|c: char| c.is_ascii_digit()) {
        format!("p_{out}")
    } else {
        out
    };
    (!out.is_empty() && valid_predicate(&out)).then_some(out)
}

fn valid_scope(scope: &str) -> bool {
    (1..=64).contains(&scope.len())
        && scope.starts_with(|c: char| c.is_ascii_lowercase())
        && scope
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ":_-".contains(c))
}

async fn teach(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Json(input): Json<TeachInput>,
) -> ApiResult {
    let unprocessable = |m: &str| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, m);
    let predicate =
        predicate_from(&input.predicate).ok_or_else(|| unprocessable("Describe the relation in a few words"))?;
    let kind = match input.kind.as_deref() {
        None | Some("") => EntityKind::Other,
        Some(k) => EntityKind::parse(k).ok_or_else(|| unprocessable("Unknown kind"))?,
    };
    let category = match input.category.as_deref() {
        None | Some("") | Some("fact") => ClaimCategory::Fact,
        Some("preference") => ClaimCategory::Preference,
        Some("question") => ClaimCategory::Question,
        Some(_) => return Err(unprocessable("Unknown category")),
    };
    let scope = input.scope.clone().unwrap_or_else(|| "user".into());
    if !valid_scope(&scope) {
        return Err(unprocessable("Unknown scope"));
    }
    let sensitivity = if input.private {
        Sensitivity::Private
    } else {
        Sensitivity::Normal
    };
    with_store(state, headers, move |store| {
        let kn = store.knowledge();
        let subject_name = input.subject.trim().to_string();
        let subject = match input.subject_id {
            Some(id) => Some(id),
            None => {
                let found = kn.find_entities(&subject_name, Some(kind), &owner())?;
                if found.len() > 1 {
                    return Err(ApiError(
                        StatusCode::CONFLICT,
                        json!({"error": "Several entities have this name; choose one", "candidates": found}),
                    ));
                }
                found.first().map(|e| e.id)
            }
        };
        let value = input.value.trim().to_string();
        let (entity_id, claim_id) = kn.write(Actor::User, |tx| {
            let entity_id = match subject {
                Some(id) => id,
                None => tx.create_entity(&NewEntity {
                    kind,
                    name: &subject_name,
                    aliases: &[],
                    scope: &scope,
                    sensitivity,
                })?,
            };
            let statement = format!("{subject_name} {} {value}", predicate.replace('_', " "));
            let ev = tx.add_evidence(&NewEvidence {
                source_kind: SourceKind::UserStatement,
                source_ref: Some("memory-ui"),
                observed_at: now_ms(),
                content: &statement,
                receipt: Some(&statement),
                derived_from: None,
                verification: None,
                scope: &scope,
            })?;
            let claim = tx.assert_claim(
                &NewClaim {
                    subject: entity_id,
                    predicate: &predicate,
                    object: ClaimObject::Value(value.clone()),
                    category,
                    basis: (category == ClaimCategory::Preference)
                        .then_some(PreferenceBasis::Explicit),
                    status: if category == ClaimCategory::Question {
                        EpistemicStatus::Unverified
                    } else {
                        EpistemicStatus::UserReported
                    },
                    inference_confidence: None,
                    observed_at: now_ms(),
                    valid_from: None,
                    valid_until: None,
                    retention: Retention::Durable,
                    scope: &scope,
                    sensitivity,
                },
                &[(ev, Stance::Supports)],
            )?;
            Ok((entity_id, claim))
        })?;
        Ok(json!({"entity_id": entity_id, "claim_id": claim_id}))
    })
    .await
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Deserialize, Default)]
struct ReasonInput {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct CorrectInput {
    value: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct FlagInput {
    value: bool,
}

#[derive(Deserialize)]
struct MergeInput {
    into: i64,
    #[serde(default)]
    reason: Option<String>,
}

fn user_write(
    store: &MemoryStore,
    f: impl FnOnce(&mut KnowledgeTx<'_>) -> KnowledgeResult<Value>,
) -> Result<Value, ApiError> {
    Ok(store.knowledge().write(Actor::User, f)?)
}

async fn confirm_claim(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        user_write(store, |tx| tx.confirm_claim(id).map(|_| json!({"ok": true})))
    })
    .await
}

async fn correct_claim(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<CorrectInput>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        user_write(store, |tx| {
            let new = tx.correct_claim(id, &input.value, &reason(&input.reason))?;
            Ok(json!({"claim_id": new}))
        })
    })
    .await
}

async fn invalidate_claim(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<ReasonInput>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        user_write(store, |tx| {
            tx.set_status(id, EpistemicStatus::Invalidated, Some(&reason(&input.reason)))
                .map(|_| json!({"ok": true}))
        })
    })
    .await
}

async fn forget_claim(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<ReasonInput>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        user_write(store, |tx| {
            tx.delete_claim(id, &reason(&input.reason))
                .map(|_| json!({"ok": true}))
        })
    })
    .await
}

async fn pin_claim(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<FlagInput>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        user_write(store, |tx| tx.set_pinned(id, input.value).map(|_| json!({"ok": true})))
    })
    .await
}

fn sensitivity(private: bool) -> Sensitivity {
    if private {
        Sensitivity::Private
    } else {
        Sensitivity::Normal
    }
}

async fn private_claim(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<FlagInput>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        user_write(store, |tx| {
            tx.set_claim_sensitivity(id, sensitivity(input.value))
                .map(|_| json!({"ok": true}))
        })
    })
    .await
}

async fn forget_entity(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<ReasonInput>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        user_write(store, |tx| {
            tx.delete_entity(id, &reason(&input.reason))
                .map(|_| json!({"ok": true}))
        })
    })
    .await
}

async fn merge_entity(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<MergeInput>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        user_write(store, |tx| {
            tx.merge_entities(id, input.into, &reason(&input.reason))
                .map(|_| json!({"entity_id": input.into}))
        })
    })
    .await
}

async fn separate_entity(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<ReasonInput>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        user_write(store, |tx| {
            let from = tx.separate_entity(id, &reason(&input.reason))?;
            Ok(json!({"entity_id": id, "separated_from": from}))
        })
    })
    .await
}

async fn private_entity(
    State(state): State<Arc<MemoryHttpState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<FlagInput>,
) -> ApiResult {
    with_store(state, headers, move |store| {
        user_write(store, |tx| {
            tx.set_entity_sensitivity(id, sensitivity(input.value))
                .map(|_| json!({"ok": true}))
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    const OWNER: &str = "owner-test-token-with-at-least-32-random-bytes";

    fn app() -> (Router, Arc<MemoryStore>) {
        let store = Arc::new(MemoryStore::open_in_memory().unwrap());
        let authority = sail_guardian::authority::ApprovalAuthority::new(OWNER).ok();
        (router(store.clone(), authority), store)
    }

    async fn call(app: &Router, path: &str, body: Option<Value>, token: Option<&str>) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .uri(path)
            .method(if body.is_some() { "POST" } else { "GET" })
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn every_route_requires_the_owner_token() {
        let (app, _) = app();
        for path in ["/overview", "/search?q=x", "/entities/1", "/graph/1", "/changes", "/export"] {
            assert_eq!(call(&app, path, None, None).await.0, StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(call(&app, path, None, Some("wrong-token-but-long-enough-to-check-0000")).await.0, StatusCode::UNAUTHORIZED);
        }
        let (status, _) = call(&app, "/claims/1/forget", Some(json!({})), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let unconfigured = router(Arc::new(MemoryStore::open_in_memory().unwrap()), None);
        assert_eq!(call(&unconfigured, "/overview", None, Some(OWNER)).await.0, StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Phase 4 gate over HTTP: what the owner teaches and corrects persists and changes
    /// what later reads return.
    #[tokio::test]
    async fn teach_correct_and_forget_round_trip() {
        let (app, _) = app();
        let (status, taught) = call(
            &app,
            "/teach",
            Some(json!({"subject": "SAIL host", "kind": "environment", "predicate": "Root disk", "value": "512 GB SSD", "scope": "system"})),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{taught}");
        let entity = taught["entity_id"].as_i64().unwrap();
        let claim = taught["claim_id"].as_i64().unwrap();

        let (_, found) = call(&app, "/search?q=host", None, Some(OWNER)).await;
        assert_eq!(found["entities"][0]["id"], entity);
        let (_, overview) = call(&app, "/overview", None, Some(OWNER)).await;
        assert_eq!(overview["counts"]["active_claims"], 1);

        let (status, corrected) = call(&app, &format!("/claims/{claim}/correct"), Some(json!({"value": "1 TB NVMe", "reason": "upgraded"})), Some(OWNER)).await;
        assert_eq!(status, StatusCode::OK, "{corrected}");
        let (_, detail) = call(&app, &format!("/entities/{entity}"), None, Some(OWNER)).await;
        let claims = detail["claims"].as_array().unwrap();
        let current: Vec<_> = claims.iter().filter(|c| c["claim"]["status"] != "superseded").collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0]["claim"]["object"]["value"], "1 TB NVMe");
        assert_eq!(current[0]["claim"]["predicate"], "root_disk");
        assert_eq!(current[0]["evidence"][0]["evidence"]["source_kind"], "user_statement");

        let new = corrected["claim_id"].as_i64().unwrap();
        let (status, _) = call(&app, &format!("/claims/{new}/forget"), Some(json!({"reason": "test"})), Some(OWNER)).await;
        assert_eq!(status, StatusCode::OK);
        let (_, changes) = call(&app, "/changes", None, Some(OWNER)).await;
        let ops: Vec<&str> = changes["changes"].as_array().unwrap().iter().map(|c| c["operation"]["op"].as_str().unwrap()).collect();
        assert_eq!(&ops[..3], &["delete", "supersede", "assert"]);
        assert!(!changes.to_string().contains("1 TB NVMe"), "the log never shows forgotten values");
        let (_, exported) = call(&app, "/export", None, Some(OWNER)).await;
        assert!(!exported.to_string().contains("1 TB NVMe"));
    }

    #[tokio::test]
    async fn homonyms_ask_the_owner_to_choose_and_errors_are_typed() {
        let (app, store) = app();
        store
            .knowledge()
            .write(Actor::subsystem("test"), |tx| {
                for _ in 0..2 {
                    tx.create_entity(&NewEntity { kind: EntityKind::Person, name: "Alex", aliases: &[], scope: "user", sensitivity: Sensitivity::Normal })?;
                }
                Ok(())
            })
            .unwrap();
        let teach = json!({"subject": "Alex", "kind": "person", "predicate": "plays", "value": "cello"});
        let (status, body) = call(&app, "/teach", Some(teach.clone()), Some(OWNER)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["candidates"].as_array().unwrap().len(), 2);
        let chosen = body["candidates"][1]["id"].as_i64().unwrap();
        let mut teach = teach;
        teach["subject_id"] = json!(chosen);
        assert_eq!(call(&app, "/teach", Some(teach), Some(OWNER)).await.0, StatusCode::OK);

        assert_eq!(call(&app, "/entities/999", None, Some(OWNER)).await.0, StatusCode::NOT_FOUND);
        let (status, body) = call(&app, "/teach", Some(json!({"subject": "x", "predicate": "!!", "value": "y"})), Some(OWNER)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        let (status, _) = call(&app, "/teach", Some(json!({"subject": "x", "predicate": "is", "value": "y", "scope": "Not A Scope"})), Some(OWNER)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn predicates_are_normalised_from_plain_words() {
        assert_eq!(predicate_from("Favourite colour").as_deref(), Some("favourite_colour"));
        assert_eq!(predicate_from("  uses -- Rust  ").as_deref(), Some("uses_rust"));
        assert_eq!(predicate_from("3d printer").as_deref(), Some("p_3d_printer"));
        assert_eq!(predicate_from("!!"), None);
    }
}
