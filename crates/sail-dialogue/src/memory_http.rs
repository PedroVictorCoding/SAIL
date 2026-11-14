//! Owner-only memory administration. Model/tool arguments cannot select the actor.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use sail_arena::{MemoryStore, knowledge::*};
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
struct MemoryState {
    store: Arc<MemoryStore>,
    authority: Option<ApprovalAuthority>,
    arena: Option<Arc<sail_arena::ArenaSubsystem>>,
}
pub fn router(store: Arc<MemoryStore>, authority: Option<ApprovalAuthority>) -> Router {
    build_router(store, authority, None)
}

pub fn router_with_arena(
    arena: Arc<sail_arena::ArenaSubsystem>,
    authority: Option<ApprovalAuthority>,
) -> Router {
    build_router(arena.store().clone(), authority, Some(arena))
}

fn build_router(
    store: Arc<MemoryStore>,
    authority: Option<ApprovalAuthority>,
    arena: Option<Arc<sail_arena::ArenaSubsystem>>,
) -> Router {
    Router::new()
        .route("/learning", post(learning))
        .route("/read", post(read))
        .route("/change", post(change))
        .route("/tool-logs/clear", post(clear_tool_logs))
        .route("/review", axum::routing::get(review))
        .route("/review/enabled", post(review_enabled))
        .route("/review/pin", post(review_pin))
        .route("/review/:id/:action", post(review_decide))
        .layer(axum::extract::DefaultBodyLimit::max(64000))
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(Arc::new(MemoryState {
            store,
            authority,
            arena,
        }))
}
fn authorize(s: &MemoryState, h: &HeaderMap) -> Result<(), StatusCode> {
    let a = s
        .authority
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let token = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if !a.authenticates(token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}
fn error(e: KnowledgeError) -> (StatusCode, Json<Value>) {
    (
        if matches!(e, KnowledgeError::StaleGeneration { .. }) {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
        },
        Json(json!({"error":e.to_string()})),
    )
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Read {
    Overview,
    Search {
        query: sail_protocol::memory::MemoryQuery,
    },
    Entity {
        id: i64,
    },
    Export,
    ToolLogs {
        #[serde(default)]
        tool: Option<String>,
        #[serde(default)]
        query: Option<String>,
        #[serde(default)]
        success: Option<bool>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        before_id: Option<i64>,
    },
}
async fn read(
    State(s): State<Arc<MemoryState>>,
    h: HeaderMap,
    Json(request): Json<Read>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&s, &h)
        .map_err(|code| (code, Json(json!({"error":"Owner authentication required"}))))?;
    let k = s.store.knowledge();
    let access = Access::owner();
    let result = (|| -> KnowledgeResult<Value> {
        Ok(match request {
            Read::Overview => {
                json!({"entities":k.entities(&access,500)?,"history":k.operations(100)?,"jobs":k.job_summaries(&access)?,"generation":k.deletion_generation()?})
            }
            Read::Search { query } => {
                serde_json::to_value(k.search(&query, &access, None, now())?)
                    .map_err(|e| KnowledgeError::Invalid(e.to_string()))?
            }
            Read::Entity { id } => {
                let entity = k
                    .entity(id, &access)?
                    .ok_or(KnowledgeError::NotFound("entity", id))?;
                let claims = k.claims_about(id, &access, true)?;
                let mut why = Vec::new();
                for c in &claims {
                    why.push(json!({"claim_id":c.id,"evidence":k.evidence(c.id,&access)?,"support":k.support(c.id,&access)?}));
                }
                json!({"entity":entity,"claims":claims,"why":why,"graph":k.neighbors(id,&access,true)?,"procedures":k.procedure_versions(id,&access)?,"project_items":k.project_items(id,&access)?,"generation":k.deletion_generation()?})
            }
            Read::Export => k.export(&access)?,
            Read::ToolLogs {
                tool,
                query,
                success,
                limit,
                before_id,
            } => {
                let entries = s
                    .store
                    .list_tool_logs(
                        tool.as_deref(),
                        query.as_deref(),
                        success,
                        limit.unwrap_or(100),
                        before_id,
                    )
                    .map_err(|e| KnowledgeError::Invalid(e.to_string()))?;
                json!({"entries": entries})
            }
        })
    })();
    result.map(Json).map_err(error)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    generation: i64,
    action: Mutation,
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Mutation {
    Correct { claim: i64, value: String },
    Pin { entity: i64, pinned: bool },
    Merge { from: i64, into: i64 },
    Separate { entity: i64 },
    Private { entity: i64 },
    ForgetClaim { claim: i64 },
    ForgetEntity { entity: i64 },
}
async fn change(
    State(s): State<Arc<MemoryState>>,
    h: HeaderMap,
    Json(request): Json<Change>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&s, &h)
        .map_err(|code| (code, Json(json!({"error":"Owner authentication required"}))))?;
    let k = s.store.knowledge();
    let access = Access::owner();
    let correction = if let Mutation::Correct { claim, .. } = &request.action {
        Some(
            k.claim(*claim, &access)
                .map_err(error)?
                .filter(|c| c.status.is_active())
                .ok_or_else(|| error(KnowledgeError::NotFound("active claim", *claim)))?,
        )
    } else {
        None
    };
    k.write_at_generation(request.generation, Actor::User, |tx| {
        match request.action {
            Mutation::Pin { entity, pinned } => tx.pin_entity(entity, pinned)?,
            Mutation::Merge { from, into } => tx.merge_entities(from, into, "owner merge")?,
            Mutation::Separate { entity } => tx.separate_entity(entity, "owner separation")?,
            Mutation::Private { entity } => tx.mark_private(entity)?,
            Mutation::ForgetClaim { claim } => tx.delete_claim(claim, "owner forget")?,
            Mutation::ForgetEntity { entity } => tx.delete_entity(entity, "owner forget")?,
            Mutation::Correct { value, .. } => {
                let c = correction
                    .as_ref()
                    .ok_or_else(|| KnowledgeError::Invalid("missing correction target".into()))?;
                let evidence = tx.add_evidence(&NewEvidence {
                    source_kind: SourceKind::UserStatement,
                    source_ref: Some("owner:memory-view"),
                    observed_at: now(),
                    content: &value,
                    receipt: Some(&value),
                    derived_from: None,
                    verification: None,
                    scope: &c.scope,
                })?;
                let new = tx.assert_claim(
                    &NewClaim {
                        subject: c.subject,
                        predicate: &c.predicate,
                        object: ClaimObject::Value(value),
                        category: c.category,
                        basis: c.basis,
                        status: EpistemicStatus::UserReported,
                        inference_confidence: None,
                        observed_at: now(),
                        valid_from: None,
                        valid_until: None,
                        retention: c.retention,
                        scope: &c.scope,
                        sensitivity: c.sensitivity,
                    },
                    &[(evidence, Stance::Supports)],
                )?;
                if new != c.id {
                    tx.supersede(c.id, new, "owner correction")?;
                }
            }
        }
        Ok(())
    })
    .map_err(error)?;
    Ok(Json(
        json!({"ok":true,"generation":k.deletion_generation().map_err(error)?}),
    ))
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Learning {
    session: String,
    enabled: bool,
}

async fn learning(
    State(s): State<Arc<MemoryState>>,
    h: HeaderMap,
    Json(request): Json<Learning>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&s, &h)
        .map_err(|code| (code, Json(json!({"error":"Owner authentication required"}))))?;
    let arena = s.arena.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"Voice sessions unavailable"})),
        )
    })?;
    arena
        .set_session_learning(&request.session, request.enabled)
        .map_err(error)?;
    Ok(Json(
        json!({"session":request.session,"enabled":request.enabled}),
    ))
}

async fn clear_tool_logs(
    State(s): State<Arc<MemoryState>>,
    h: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&s, &h)
        .map_err(|code| (code, Json(json!({"error":"Owner authentication required"}))))?;
    let deleted = s
        .store
        .clear_tool_logs()
        .map_err(|e| error(KnowledgeError::Invalid(e.to_string())))?;
    Ok(Json(json!({"ok": true, "deleted": deleted})))
}

/// The Dialogue grant: what the profile block may include (the same access a turn uses).
fn profile_access() -> Option<Access> {
    let grant =
        sail_guardian::memory::memory_grant(&sail_protocol::memory::MemoryPurpose::Dialogue)
            .ok()?;
    Some(Access::from_grant(&grant))
}

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn fail(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": message.into()})))
}

fn owner(s: &MemoryState, h: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    authorize(s, h).map_err(|code| fail(code, "Owner authentication required"))
}

/// Memory › Review: the switch, pending changes and the profile block.
async fn review(State(s): State<Arc<MemoryState>>, h: HeaderMap) -> Reply {
    owner(&s, &h)?;
    let store = s.store.clone();
    let view = tokio::task::spawn_blocking(move || {
        let access = profile_access().ok_or("the memory grant is unavailable")?;
        crate::memory_profile::review_view(&store, &access)
    })
    .await
    .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "review failed"))?
    .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(view))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnabledBody {
    enabled: bool,
}

async fn review_enabled(
    State(s): State<Arc<MemoryState>>,
    h: HeaderMap,
    Json(b): Json<EnabledBody>,
) -> Reply {
    owner(&s, &h)?;
    crate::memory_profile::review_queue()
        .set_enabled(b.enabled)
        .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    tracing::info!(enabled = b.enabled, "Memory review changed by owner");
    Ok(Json(json!({"enabled": b.enabled})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PinBody {
    claim_id: i64,
    pinned: bool,
}

async fn review_pin(
    State(s): State<Arc<MemoryState>>,
    h: HeaderMap,
    Json(b): Json<PinBody>,
) -> Reply {
    owner(&s, &h)?;
    crate::memory_profile::set_pin(b.claim_id, b.pinned)
        .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({"ok": true})))
}

async fn review_decide(
    State(s): State<Arc<MemoryState>>,
    h: HeaderMap,
    axum::extract::Path((id, action)): axum::extract::Path<(String, String)>,
) -> Reply {
    owner(&s, &h)?;
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(fail(StatusCode::BAD_REQUEST, "invalid id"));
    }
    let store = s.store.clone();
    let save = match action.as_str() {
        "save" => true,
        "discard" => false,
        _ => return Err(fail(StatusCode::NOT_FOUND, "unknown action")),
    };
    tokio::task::spawn_blocking(move || {
        let queue = crate::memory_profile::review_queue();
        if !save {
            return queue.reject(&id).map_err(|e| (StatusCode::NOT_FOUND, e));
        }
        let pending = queue.take(&id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
        let report = sail_flow::memory::apply_reviewed(&store, &Access::owner(), &pending, now())
            .map_err(|e| (StatusCode::CONFLICT, e))?;
        match report.rejected {
            Some(reason) => Err((
                StatusCode::CONFLICT,
                format!("Arena's validator rejected it: {reason}"),
            )),
            None => Ok(()),
        }
    })
    .await
    .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "review failed"))?
    .map_err(|(code, e)| fail(code, e))?;
    tracing::info!(save, "Memory change reviewed by owner");
    Ok(Json(json!({"ok": true})))
}
