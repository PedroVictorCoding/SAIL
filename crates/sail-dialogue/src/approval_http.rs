//! Owner-authenticated approval transport; the event bus cannot resolve approvals.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use sail_guardian::SecurityRuntime;
use serde_json::json;
use std::sync::Arc;
struct ApprovalHttpState {
    approval_authority: Option<sail_guardian::authority::ApprovalAuthority>,
    broker: Option<Arc<sail_guardian::GuardianExecutionBroker>>,
    active_token: Option<String>,
}
pub fn router(
    authority: Option<sail_guardian::authority::ApprovalAuthority>,
    broker: Option<Arc<sail_guardian::GuardianExecutionBroker>>,
) -> Router {
    router_with_token(authority, broker, None)
}

pub fn router_with_token(
    authority: Option<sail_guardian::authority::ApprovalAuthority>,
    broker: Option<Arc<sail_guardian::GuardianExecutionBroker>>,
    active_token: Option<String>,
) -> Router {
    Router::new()
        .route("/approvals", get(owner_pending))
        .route("/resolve", post(owner_resolve))
        .route("/revoke", post(owner_revoke))
        .route("/token", get(owner_token_bootstrap))
        .route("/status", get(guardian_security_status))
        .route("/preset", post(guardian_set_preset))
        .route("/audit", get(guardian_recent_audit))
        .route("/rules", get(guardian_list_rules))
        .route("/rules/revoke", post(guardian_revoke_rule))
        .with_state(Arc::new(ApprovalHttpState {
            approval_authority: authority,
            broker,
            active_token,
        }))
}

async fn guardian_security_status(
    State(state): State<Arc<ApprovalHttpState>>,
) -> impl IntoResponse {
    let runtime = sail_guardian::runtime::NonoSecurityRuntime::new();
    let caps = runtime.inspect_capabilities();
    let preset = state
        .broker
        .as_ref()
        .map(|b| b.policy().autonomy_preset())
        .unwrap_or_default();

    (
        [("cache-control", "no-store")],
        Json(json!({
            "status": caps.status,
            "platform": caps.platform,
            "nono_version": caps.nono_version,
            "landlock_supported": caps.landlock_supported,
            "detected_abi": caps.detected_abi,
            "max_enforced_abi": caps.max_enforced_abi,
            "bubblewrap_available": caps.bubblewrap_available,
            "cgroup_limits_available": caps.cgroup_limits_available,
            "features": caps.features,
            "autonomy_preset": preset,
            "status_detail": caps.status_detail,
        })),
    )
}

#[derive(serde::Deserialize)]
struct SetPresetPayload {
    preset: String,
}

async fn guardian_set_preset(
    State(state): State<Arc<ApprovalHttpState>>,
    headers: HeaderMap,
    Json(payload): Json<SetPresetPayload>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    authenticate_owner(&state, &headers)?;
    let broker = state
        .broker
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let preset = match payload.preset.to_lowercase().as_str() {
        "careful" => sail_guardian::runtime::AutonomyPreset::Careful,
        "balanced" => sail_guardian::runtime::AutonomyPreset::Balanced,
        "autonomous" => sail_guardian::runtime::AutonomyPreset::Autonomous,
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    broker.policy().set_autonomy_preset(preset);
    Ok((
        [("cache-control", "no-store")],
        Json(json!({"preset": preset, "status": "ok"})),
    ))
}

async fn guardian_recent_audit(
    State(state): State<Arc<ApprovalHttpState>>,
    headers: HeaderMap,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    authenticate_owner(&state, &headers)?;
    let broker = state
        .broker
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let entries = broker.audit().list(50);
    Ok((
        [("cache-control", "no-store")],
        Json(json!({"audit": entries})),
    ))
}

async fn guardian_list_rules(
    State(state): State<Arc<ApprovalHttpState>>,
    headers: HeaderMap,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    authenticate_owner(&state, &headers)?;
    let broker = state
        .broker
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let rules = broker.policy().list_learned_rules();
    Ok((
        [("cache-control", "no-store")],
        Json(json!({"rules": rules})),
    ))
}

#[derive(serde::Deserialize)]
struct RevokeRulePayload {
    rule_id: uuid::Uuid,
}

async fn guardian_revoke_rule(
    State(state): State<Arc<ApprovalHttpState>>,
    headers: HeaderMap,
    Json(payload): Json<RevokeRulePayload>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    authenticate_owner(&state, &headers)?;
    let broker = state
        .broker
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let revoked = broker.policy().revoke_learned_rule(payload.rule_id);
    if revoked {
        Ok((
            [("cache-control", "no-store")],
            Json(json!({"status": "ok", "revoked": true})),
        ))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn owner_token_bootstrap(
    State(state): State<Arc<ApprovalHttpState>>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    if let Some(token) = &state.active_token {
        Ok((
            [("cache-control", "no-store")],
            Json(json!({"token": token, "configured": true})),
        ))
    } else {
        Err(StatusCode::SERVICE_UNAVAILABLE)
    }
}
fn authenticate_owner(
    state: &ApprovalHttpState,
    headers: &HeaderMap,
) -> std::result::Result<(), StatusCode> {
    let authority = state
        .approval_authority
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if !authority.authenticates(token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}
async fn owner_pending(
    State(state): State<Arc<ApprovalHttpState>>,
    headers: HeaderMap,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    authenticate_owner(&state, &headers)?;
    let broker = state
        .broker
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    Ok((
        [("cache-control", "no-store")],
        Json(json!({"pending":broker.approval().list_pending()})),
    ))
}
#[derive(serde::Deserialize)]
struct ApprovalResolution {
    session_id: uuid::Uuid,
    approved: bool,
    #[serde(default)]
    remember: bool,
    #[serde(default)]
    scope: Option<String>,
}
async fn owner_resolve(
    State(state): State<Arc<ApprovalHttpState>>,
    headers: HeaderMap,
    Json(input): Json<ApprovalResolution>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    authenticate_owner(&state, &headers)?;
    let broker = state
        .broker
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let token = match broker
        .approval()
        .resolve_session(input.session_id, input.approved)
    {
        Ok(token) => {
            if input.approved && (input.remember || input.scope.as_deref() == Some("always")) {
                let rule = broker.policy().learn_approval(
                    token.bound_actor,
                    &token.bound_capability,
                    format!(
                        "Always allowed by owner on {}",
                        chrono::Utc::now().format("%Y-%m-%d %H:%M")
                    ),
                );
                tracing::info!(rule_id = %rule.id, pattern = ?rule.pattern, "Guardian learned new persistent approval rule");
            } else if input.approved && input.scope.as_deref() == Some("session") {
                broker.policy().grant_scoped(
                    token.bound_actor,
                    token.bound_capability.clone(),
                    token.correlation_id,
                    chrono::Duration::hours(24),
                );
            }
            Some(token)
        }
        Err(sail_guardian::ApprovalError::Denied) if !input.approved => None,
        Err(_) => return Err(StatusCode::CONFLICT),
    };
    Ok((
        [("cache-control", "no-store")],
        Json(json!({"token":token})),
    ))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalRevocation {
    session_id: uuid::Uuid,
}
async fn owner_revoke(
    State(state): State<Arc<ApprovalHttpState>>,
    headers: HeaderMap,
    Json(input): Json<ApprovalRevocation>,
) -> std::result::Result<StatusCode, StatusCode> {
    authenticate_owner(&state, &headers)?;
    state
        .broker
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .approval()
        .revoke_session(input.session_id);
    Ok(StatusCode::NO_CONTENT)
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use sail_common::types::SubsystemId;
    use sail_guardian::{
        ApprovalBroker, AuditLedger, Capability, GuardianExecutionBroker, PolicyEngine,
        SandboxPolicy, SystemCapability,
    };
    use tower::ServiceExt;
    const OWNER: &str = "owner-test-token-with-at-least-32-random-bytes";
    async fn request(
        router: &Router,
        path: &str,
        body: Option<serde_json::Value>,
        token: Option<&str>,
    ) -> axum::response::Response {
        let mut request =
            Request::builder()
                .uri(path)
                .method(if body.is_some() { "POST" } else { "GET" });
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        request = request.header("content-type", "application/json");
        router
            .clone()
            .oneshot(
                request
                    .body(Body::from(
                        body.map(|value| value.to_string()).unwrap_or_default(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn only_owner_can_resolve_and_tokens_cannot_cross_tasks_or_replay() {
        let approvals = Arc::new(ApprovalBroker::new());
        let broker = Arc::new(GuardianExecutionBroker::new(
            Arc::new(PolicyEngine::new()),
            Arc::new(AuditLedger::new()),
            approvals.clone(),
            Arc::new(SandboxPolicy::new()),
        ));
        let app = router(
            Some(sail_guardian::authority::ApprovalAuthority::new(OWNER).unwrap()),
            Some(broker),
        );
        let cap = Capability::System(SystemCapability::ProcessInspect);
        let context = uuid::Uuid::new_v4();
        let session = approvals.create_session_for(
            Some(context),
            SubsystemId::Dialogue,
            cap.clone(),
            "list processes",
        );
        let resolve = json!({"session_id":session.session_id,"approved":true});
        assert_eq!(
            request(&app, "/resolve", Some(resolve.clone()), None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(&app, "/resolve", Some(resolve.clone()), Some("wrong"))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let pending = request(&app, "/approvals", None, Some(OWNER)).await;
        assert_eq!(pending.status(), StatusCode::OK);
        assert_eq!(pending.headers()["cache-control"], "no-store");
        let response = request(&app, "/resolve", Some(resolve.clone()), Some(OWNER)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let token = uuid::Uuid::parse_str(result["token"]["token_id"].as_str().unwrap()).unwrap();
        assert!(
            approvals
                .consume_token_for(
                    token,
                    SubsystemId::Dialogue,
                    &cap,
                    Some(uuid::Uuid::new_v4())
                )
                .is_err()
        );
        assert!(
            approvals
                .consume_token_for(token, SubsystemId::Dialogue, &cap, Some(context))
                .is_ok()
        );
        assert!(
            approvals
                .consume_token_for(token, SubsystemId::Dialogue, &cap, Some(context))
                .is_err()
        );
        assert_eq!(
            request(&app, "/resolve", Some(resolve), Some(OWNER))
                .await
                .status(),
            StatusCode::CONFLICT
        );
    }
    #[tokio::test]
    async fn unconfigured_approval_authority_fails_closed() {
        let app = router(None, None);
        assert_eq!(
            request(&app, "/approvals", None, Some(OWNER))
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    #[tokio::test]
    async fn bootstrap_token_endpoint_works() {
        let app_with_token = router_with_token(None, None, Some(OWNER.to_string()));
        let res = request(&app_with_token, "/token", None, None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = to_bytes(res.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["token"], OWNER);
        assert_eq!(json["configured"], true);

        let app_without_token = router_with_token(None, None, None);
        let res_unavail = request(&app_without_token, "/token", None, None).await;
        assert_eq!(res_unavail.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    #[tokio::test]
    async fn status_and_preset_endpoints_work() {
        let approvals = Arc::new(ApprovalBroker::new());
        let broker = Arc::new(GuardianExecutionBroker::new(
            Arc::new(PolicyEngine::new()),
            Arc::new(AuditLedger::new()),
            approvals.clone(),
            Arc::new(SandboxPolicy::new()),
        ));
        let app = router_with_token(
            Some(sail_guardian::authority::ApprovalAuthority::new(OWNER).unwrap()),
            Some(broker.clone()),
            Some(OWNER.to_string()),
        );

        let status_res = request(&app, "/status", None, None).await;
        assert_eq!(status_res.status(), StatusCode::OK);
        let bytes = to_bytes(status_res.into_body(), 65536).await.unwrap();
        let status_json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status_json["nono_version"], "0.81.0");
        assert_eq!(status_json["autonomy_preset"], "balanced");

        // Set preset to autonomous
        let set_res = request(
            &app,
            "/preset",
            Some(json!({"preset": "autonomous"})),
            Some(OWNER),
        )
        .await;
        assert_eq!(set_res.status(), StatusCode::OK);
        assert_eq!(
            broker.policy().autonomy_preset(),
            sail_guardian::runtime::AutonomyPreset::Autonomous
        );

        // Audit endpoint
        let audit_res = request(&app, "/audit", None, Some(OWNER)).await;
        assert_eq!(audit_res.status(), StatusCode::OK);
    }
}
