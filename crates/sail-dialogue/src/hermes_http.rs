//! Owner-only API for Hermes Agent plugins and Agent Skills (`/api/hermes`), used by the
//! Skills & plugins section of the SAIL Agents workspace.
//!
//! Approving a plugin here is the same owner decision as `sail-hermes approve`: it binds
//! the plugin's current file hash, network and environment grants. Nothing here runs a
//! plugin tool; tools still run only through `hermes:call` with Guardian approval. Listing
//! enabled plugins runs their `register(ctx)` in the sandbox once per approved hash.
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use sail_flow::{
    hermes::{self, Manager, Status},
    proposals::Proposals,
    skills::SkillLibrary,
};
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct HttpState {
    manager: &'static Manager,
    skill_roots: Vec<(String, std::path::PathBuf)>,
    proposals: Proposals,
    authority: Option<ApprovalAuthority>,
}
type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error":message.into()})))
}

fn authorize(s: &HttpState, h: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    let authority = s.authority.as_ref().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "owner authority unavailable",
        )
    })?;
    let token = h
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if token.is_some_and(|t| authority.authenticates(t)) {
        Ok(())
    } else {
        Err(error(StatusCode::UNAUTHORIZED, "owner token required"))
    }
}

pub fn router(authority: Option<ApprovalAuthority>) -> Router {
    router_with(
        hermes::global(),
        SkillLibrary::default_roots(),
        Proposals::default_paths(),
        Some(sail_flow::market::global()),
        authority,
    )
}

/// For tests and demos: a specific manager, skill roots and market (`None` = no
/// `/market` routes).
pub fn router_with(
    manager: &'static Manager,
    skill_roots: Vec<(String, std::path::PathBuf)>,
    proposals: Proposals,
    market: Option<Arc<sail_flow::market::Market>>,
    authority: Option<ApprovalAuthority>,
) -> Router {
    let mut router = Router::new()
        .route("/plugins", get(plugins))
        .route("/plugins/:id/approve", post(approve))
        .route("/plugins/:id/revoke", post(revoke))
        .route("/plugins/:id/hooks", post(set_hook))
        .route("/plugins/:id/environment", post(environment))
        .route("/skills", get(skills))
        .route("/skill", get(skill))
        .route("/proposals", get(proposals_list))
        .route("/proposals/:id/approve", post(proposal_approve))
        .route("/proposals/:id/reject", post(proposal_reject))
        .with_state(Arc::new(HttpState {
            manager,
            skill_roots,
            proposals,
            authority: authority.clone(),
        }));
    if let Some(market) = market {
        router = router.nest("/market", crate::market_http::router(market, authority));
    }
    router
        .layer(axum::extract::DefaultBodyLimit::max(32_000))
        .layer(axum::middleware::map_response(
            |mut r: axum::response::Response| async move {
                r.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                r
            },
        ))
}

async fn plugin_json(manager: &Manager, plugin: hermes::Plugin) -> Value {
    let mut out = json!(plugin);
    let mut registered_hooks = Vec::new();
    if plugin.status == Status::Enabled {
        match manager.describe(&plugin).await {
            Ok(d) => {
                registered_hooks = d.hooks.clone();
                out["registered"] = json!(d);
            }
            Err(e) => out["describe_error"] = json!(e),
        }
    }
    out["hooks"] = json!(manager.hook_views(&plugin, &registered_hooks));
    out["environment"] = json!(manager.env_view(&plugin));
    out
}

fn find_plugin(manager: &Manager, id: &str) -> Result<hermes::Plugin, (StatusCode, Json<Value>)> {
    if !hermes::valid_id(id) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid plugin id"));
    }
    manager
        .paths
        .discover()
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, format!("no plugin {id}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HookBody {
    hook: String,
    enabled: bool,
}

/// Turn one supported hook on or off for an approved plugin (owner action).
async fn set_hook(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<HookBody>,
) -> Reply {
    authorize(&s, &h)?;
    let plugin = find_plugin(s.manager, &id)?;
    if b.enabled && plugin.status != Status::Enabled {
        return Err(error(
            StatusCode::CONFLICT,
            "approve the plugin before turning on its hooks",
        ));
    }
    let paths = s.manager.paths.clone();
    let (hook, enabled) = (b.hook.clone(), b.enabled);
    tokio::task::spawn_blocking(move || paths.set_hook(&id, &hook, enabled))
        .await
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "hook setting failed"))?
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    tracing::info!(plugin = %plugin.id, hook = %b.hook, enabled = b.enabled, "Hermes hook changed by owner");
    Ok(Json(plugin_json(s.manager, plugin).await))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvBody {
    action: String,
}

/// Build or remove an approved plugin's Python environment (owner action). A build runs
/// in the background; the plugin list shows `building` until it finishes.
async fn environment(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<EnvBody>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    authorize(&s, &h)?;
    let plugin = find_plugin(s.manager, &id)?;
    match b.action.as_str() {
        "build" => {
            if plugin.status != Status::Enabled {
                return Err(error(
                    StatusCode::CONFLICT,
                    "approve the plugin before building its environment",
                ));
            }
            if !s.manager.begin_build(&plugin.id) {
                return Err(error(StatusCode::CONFLICT, "a build is already running"));
            }
            let manager = s.manager;
            let building = plugin.clone();
            tokio::spawn(async move {
                match manager.build_env(&building).await {
                    Ok(record) => tracing::info!(plugin = %building.id, packages = record.locked.len(), "Hermes plugin environment built"),
                    Err(error) => tracing::warn!(plugin = %building.id, %error, "Hermes plugin environment build failed"),
                }
            });
            tracing::info!(plugin = %plugin.id, "Hermes plugin environment build started by owner");
        }
        "remove" => {
            let paths = s.manager.paths.clone();
            let target = plugin.id.clone();
            tokio::task::spawn_blocking(move || paths.remove_env(&target))
                .await
                .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "remove failed"))?
                .map_err(|e| error(StatusCode::CONFLICT, e))?;
        }
        _ => return Err(error(StatusCode::BAD_REQUEST, "action must be build or remove")),
    }
    Ok((StatusCode::ACCEPTED, Json(plugin_json(s.manager, plugin).await)))
}

async fn plugins(State(s): State<Arc<HttpState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    let paths = s.manager.paths.clone();
    let found = tokio::task::spawn_blocking(move || paths.discover())
        .await
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "discovery failed"))?;
    let mut list = Vec::new();
    for plugin in found {
        list.push(plugin_json(s.manager, plugin).await);
    }
    let roots: Vec<_> = s
        .manager
        .paths
        .roots
        .iter()
        .map(|(source, dir)| json!({"source":source,"path":dir}))
        .collect();
    Ok(Json(json!({
        "version": 1,
        "plugins": list,
        "roots": roots,
        "sandbox": s.manager.sandbox.bwrap.is_file(),
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveBody {
    /// The digest the owner reviewed; approval fails if the files changed since.
    digest: String,
    #[serde(default)]
    network: bool,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    settings: serde_json::Map<String, Value>,
}

async fn approve(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ApproveBody>,
) -> Reply {
    authorize(&s, &h)?;
    if body.env.len() > 32 || body.settings.len() > 64 {
        return Err(error(StatusCode::BAD_REQUEST, "too many grants"));
    }
    let paths = s.manager.paths.clone();
    let plugin = tokio::task::spawn_blocking(move || {
        let current = paths
            .discover()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| (StatusCode::NOT_FOUND, format!("no plugin {id}")))?;
        if current.digest != body.digest {
            return Err((
                StatusCode::CONFLICT,
                "the plugin's files changed since you reviewed it; reload and review again"
                    .to_string(),
            ));
        }
        paths
            .approve(&id, body.network, body.env, body.settings)
            .map_err(|e| (StatusCode::BAD_REQUEST, e))
    })
    .await
    .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "approval failed"))?
    .map_err(|(status, message)| error(status, message))?;
    tracing::info!(plugin = %plugin.id, digest = %plugin.digest, "Hermes plugin approved by owner");
    Ok(Json(
        json!({"plugin": plugin_json(s.manager, plugin).await}),
    ))
}

async fn revoke(State(s): State<Arc<HttpState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    let paths = s.manager.paths.clone();
    let removed = tokio::task::spawn_blocking(move || paths.revoke(&id))
        .await
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "revoke failed"))?
        .map_err(|e| error(StatusCode::SERVICE_UNAVAILABLE, e))?;
    Ok(Json(json!({"revoked": removed})))
}

async fn library(s: &HttpState) -> SkillLibrary {
    let roots = s.skill_roots.clone();
    let mut library = tokio::task::spawn_blocking(move || SkillLibrary::scan(&roots))
        .await
        .unwrap_or_default();
    s.manager.add_skills(&mut library).await;
    library
}

#[derive(Deserialize)]
struct SkillsQuery {
    query: Option<String>,
}

async fn skills(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Query(q): Query<SkillsQuery>,
) -> Reply {
    authorize(&s, &h)?;
    let library = library(&s).await;
    let roots: Vec<_> = s
        .skill_roots
        .iter()
        .map(|(source, dir)| json!({"source":source,"path":dir}))
        .collect();
    Ok(Json(json!({
        "version": 1,
        "skills": library.index(q.query.as_deref(), 512),
        "rejected": library.rejected,
        "roots": roots,
    })))
}

#[derive(Deserialize)]
struct SkillQuery {
    name: String,
    file: Option<String>,
}

async fn skill(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Query(q): Query<SkillQuery>,
) -> Reply {
    authorize(&s, &h)?;
    let library = library(&s).await;
    library
        .view(&q.name, q.file.as_deref())
        .map(|view| Json(json!(view)))
        .map_err(|e| error(StatusCode::NOT_FOUND, e))
}

async fn proposals_list(State(s): State<Arc<HttpState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    let proposals = s.proposals.clone();
    let list = tokio::task::spawn_blocking(move || proposals.list())
        .await
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "listing failed"))?
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({"proposals": list})))
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct ProposalApproveBody {
    acknowledge_caution: bool,
}

async fn proposal_approve(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<ProposalApproveBody>>,
) -> Reply {
    authorize(&s, &h)?;
    let acknowledge = body.map(|b| b.acknowledge_caution).unwrap_or(false);
    let proposals = s.proposals.clone();
    let name = tokio::task::spawn_blocking(move || proposals.approve(&id, acknowledge))
        .await
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "approval failed"))?
        .map_err(|e| error(StatusCode::CONFLICT, e))?;
    tracing::info!(skill = %name, "Proposed skill approved by owner");
    Ok(Json(json!({"skill": name})))
}

async fn proposal_reject(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&s, &h)?;
    let proposals = s.proposals.clone();
    tokio::task::spawn_blocking(move || proposals.reject(&id))
        .await
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "reject failed"))?
        .map_err(|e| error(StatusCode::NOT_FOUND, e))?;
    Ok(Json(json!({"ok": true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use sail_flow::hermes::{Paths, Sandbox};
    use tower::ServiceExt;

    const TOKEN: &str = "owner-token-owner-token-owner-token-0001";

    fn app(tmp: &std::path::Path) -> Router {
        let plugins = tmp.join("plugins");
        std::fs::create_dir_all(plugins.join("calc")).unwrap();
        std::fs::write(
            plugins.join("calc/plugin.yaml"),
            "name: calc\nrequires_env: [CALC_KEY]\n",
        )
        .unwrap();
        std::fs::write(
            plugins.join("calc/__init__.py"),
            "def register(ctx):\n    pass\n",
        )
        .unwrap();
        let skills = tmp.join("skills/arxiv");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("SKILL.md"),
            "---\nname: arxiv\ndescription: Find papers\n---\nSearch arXiv.\n",
        )
        .unwrap();
        let manager: &'static Manager = Box::leak(Box::new(Manager::new(
            Paths {
                roots: vec![("hermes".into(), plugins)],
                approvals: tmp.join("approvals.json"),
                data: tmp.join("data"),
                env: tmp.join("env"),
            },
            Sandbox {
                bwrap: tmp.join("no-bwrap"),
                python: "/usr/bin/python3".into(),
                runtime: tmp.join("runtime"),
                timeout: std::time::Duration::from_secs(5),
            },
        )));
        router_with(
            manager,
            vec![("hermes".into(), tmp.join("skills"))],
            Proposals {
                dir: tmp.join("proposed"),
                skills: tmp.join("skills"),
            },
            None,
            Some(ApprovalAuthority::new(TOKEN).unwrap()),
        )
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
        auth: bool,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(uri);
        if auth {
            req = req.header("authorization", format!("Bearer {TOKEN}"));
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn owner_reviews_approves_and_revokes_the_exact_files() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        assert_eq!(
            call(&app, "GET", "/plugins", None, false).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, list) = call(&app, "GET", "/plugins", None, true).await;
        assert_eq!(status, StatusCode::OK);
        let plugin = &list["plugins"][0];
        assert_eq!(plugin["status"]["state"], "quarantined");
        assert_eq!(plugin["manifest"]["requires_env"][0], "CALC_KEY");
        assert_eq!(list["sandbox"], false);
        let digest = plugin["digest"].as_str().unwrap().to_string();

        let stale = json!({"digest":"0".repeat(64)});
        assert_eq!(
            call(&app, "POST", "/plugins/calc/approve", Some(stale), true)
                .await
                .0,
            StatusCode::CONFLICT
        );
        let bad_env = json!({"digest":digest,"env":["LD_PRELOAD"]});
        assert_eq!(
            call(&app, "POST", "/plugins/calc/approve", Some(bad_env), true)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let body = json!({"digest":digest,"network":true,"env":["CALC_KEY"]});
        assert_eq!(
            call(
                &app,
                "POST",
                "/plugins/calc/approve",
                Some(body.clone()),
                false
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        let (status, approved) =
            call(&app, "POST", "/plugins/calc/approve", Some(body), true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(approved["plugin"]["status"]["state"], "enabled");
        assert_eq!(approved["plugin"]["approval"]["network"], true);
        // Without bubblewrap the plugin cannot be described, and says why.
        assert!(
            approved["plugin"]["describe_error"]
                .as_str()
                .unwrap()
                .contains("bubblewrap")
        );

        let (_, revoked) = call(&app, "POST", "/plugins/calc/revoke", None, true).await;
        assert_eq!(revoked["revoked"], true);
        let (_, list) = call(&app, "GET", "/plugins", None, true).await;
        assert_eq!(list["plugins"][0]["status"]["state"], "quarantined");
    }

    #[tokio::test]
    async fn skills_are_listed_and_read() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let (status, list) = call(&app, "GET", "/skills?query=papers", None, true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["skills"][0]["name"], "arxiv");
        let (_, view) = call(&app, "GET", "/skill?name=arxiv", None, true).await;
        assert_eq!(view["content"], "Search arXiv.\n");
        assert_eq!(
            call(&app, "GET", "/skill?name=arxiv&file=../x", None, true)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&app, "GET", "/skills", None, false).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
}
