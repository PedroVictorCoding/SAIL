//! Default reasoning model, nested at `/api/mind/models` (owner token required).
//!
//! `GET /` lists the catalog (`sail_common::models::CATALOG`) with what is installed, what
//! is running and the saved default. `PUT /default` with `{"model": "<id>"}` saves the
//! default for the next start. SAIL keeps one resident model, so the change applies when
//! SAIL restarts; nothing is reloaded here and no other model is started.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, put},
};
use sail_common::models;
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

struct ModelsState {
    authority: Option<ApprovalAuthority>,
    /// The weights Mind started with. The saved default may differ until a restart.
    running: PathBuf,
}

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

pub fn router(authority: Option<ApprovalAuthority>, running: PathBuf) -> Router {
    Router::new()
        .route("/", get(list))
        .route("/default", put(set_default))
        .with_state(Arc::new(ModelsState { authority, running }))
}

fn authorize(s: &ModelsState, h: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    let deny = |code| (code, Json(json!({"error":"Owner authentication required"})));
    let a = s
        .authority
        .as_ref()
        .ok_or_else(|| deny(StatusCode::SERVICE_UNAVAILABLE))?;
    let token = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| deny(StatusCode::UNAUTHORIZED))?;
    if !a.authenticates(token) {
        return Err(deny(StatusCode::UNAUTHORIZED));
    }
    Ok(())
}

fn view(s: &ModelsState) -> Value {
    let running = models::running_entry(&s.running);
    let default = models::default_model();
    let list: Vec<Value> = models::CATALOG
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "name": m.name,
                "summary": m.summary,
                "size_gb": m.size_gb,
                "active_params_b": m.active_params_b,
                "total_params_b": m.total_params_b,
                "vision": m.projector.is_some(),
                "source": m.source,
                "installed": models::installed(m),
                "running": running.is_some_and(|r| r.id == m.id),
                "default": default.id == m.id,
            })
        })
        .collect();
    json!({
        "models": list,
        "running": running.map(|m| m.id),
        // A path outside the catalog (SAIL_MODEL_PATH) is shown by file name only.
        "running_file": s.running.file_name().map(|n| n.to_string_lossy().into_owned()),
        "default": default.id,
        "restart_required": running.is_none_or(|r| r.id != default.id),
        "env_override": std::env::var_os("SAIL_MODEL_PATH").is_some_and(|p| !p.is_empty()),
    })
}

async fn list(State(s): State<Arc<ModelsState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    Ok(Json(view(&s)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetDefault {
    model: String,
}

async fn set_default(
    State(s): State<Arc<ModelsState>>,
    h: HeaderMap,
    Json(body): Json<SetDefault>,
) -> Reply {
    authorize(&s, &h)?;
    let Some(model) = models::entry(&body.model) else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("Unknown model `{}`.", body.model)})),
        ));
    };
    if !models::installed(model) {
        return Err((
            StatusCode::CONFLICT,
            Json(
                json!({"error": format!("{} isn't installed yet: {} is missing.", model.name, model.file)}),
            ),
        ));
    }
    models::save_default(model.id).map_err(|error| {
        tracing::warn!(%error, "Could not save the default model");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "The default model could not be saved."})),
        )
    })?;
    tracing::info!(
        model = model.id,
        "Default reasoning model saved; applies on the next start"
    );
    Ok(Json(view(&s)))
}
