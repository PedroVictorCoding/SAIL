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
}
pub fn router(store: Arc<MemoryStore>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/read", post(read))
        .route("/change", post(change))
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
        .with_state(Arc::new(MemoryState { store, authority }))
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
        })
    })();
    result.map(|v| Json(v)).map_err(error)
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
    ApproveInsight { claim: i64, approved: bool },
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
            Mutation::ApproveInsight { claim, approved } => tx.approve_insight(claim, approved)?,
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
