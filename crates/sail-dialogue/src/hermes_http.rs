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
    skills::SkillLibrary,
};
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct HttpState {
    manager: &'static Manager,
    skill_roots: Vec<(String, std::path::PathBuf)>,
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
    router_with(hermes::global(), SkillLibrary::default_roots(), authority)
}

/// For tests and demos: a specific manager and skill roots.
pub fn router_with(
    manager: &'static Manager,
    skill_roots: Vec<(String, std::path::PathBuf)>,
    authority: Option<ApprovalAuthority>,
) -> Router {
    Router::new()
        .route("/plugins", get(plugins))
        .route("/plugins/:id/approve", post(approve))
        .route("/plugins/:id/revoke", post(revoke))
        .route("/skills", get(skills))
        .route("/skill", get(skill))
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
        .with_state(Arc::new(HttpState {
            manager,
            skill_roots,
            authority,
        }))
}

async fn plugin_json(manager: &Manager, plugin: hermes::Plugin) -> Value {
    let mut out = json!(plugin);
    if plugin.status == Status::Enabled {
        match manager.describe(&plugin).await {
            Ok(d) => out["registered"] = json!(d),
            Err(e) => out["describe_error"] = json!(e),
        }
    }
    out
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
