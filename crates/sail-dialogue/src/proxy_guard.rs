//! Extra rules for requests relayed by a reverse proxy or tunnel (Cloudflare Tunnel, nginx).
//!
//! On the host or the LAN, SAIL hands its owner token to the page that asks
//! (`GET /api/guardian/token`) and opens `/ws/voice` without one, because reaching the port
//! already means being on the owner's network. A tunnel such as `cloudflared` breaks that
//! assumption: the request arrives from the tunnel process on this machine, but it came from
//! anywhere on the internet. Such requests are recognised by the forwarding headers every
//! proxy adds (`cloudflared` always sets `Cf-Connecting-IP`), and then:
//!
//! - the owner token is never handed out — enter it in the client's settings instead;
//! - `/ws/voice` needs `Authorization: Bearer <owner token>`, which the Android app sends.
//!
//! Everything else keeps its own checks. A direct client that adds these headers only
//! restricts itself, so they can't be used to gain anything.
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sail_guardian::authority::ApprovalAuthority;
use serde_json::json;
use std::sync::Arc;

const FORWARDING_HEADERS: [&str; 5] = [
    "cf-connecting-ip",
    "cf-ray",
    "x-forwarded-for",
    "forwarded",
    "x-real-ip",
];

/// The owner authority for this boot, if an owner token is configured.
#[derive(Clone, Default)]
pub struct ProxyGuard {
    authority: Option<Arc<ApprovalAuthority>>,
}

impl ProxyGuard {
    pub fn new(owner_token: Option<&str>) -> Self {
        Self {
            authority: owner_token
                .and_then(|token| ApprovalAuthority::new(token).ok())
                .map(Arc::new),
        }
    }

    fn is_owner(&self, headers: &HeaderMap) -> bool {
        let Some(authority) = &self.authority else {
            return false;
        };
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|token| authority.authenticates(token))
    }
}

/// True when a proxy or tunnel relayed the request.
pub fn is_proxied(headers: &HeaderMap) -> bool {
    FORWARDING_HEADERS
        .iter()
        .any(|name| headers.contains_key(*name))
}

pub async fn guard(State(guard): State<ProxyGuard>, request: Request, next: Next) -> Response {
    if !is_proxied(request.headers()) {
        return next.run(request).await;
    }
    match request.uri().path() {
        "/api/guardian/token" => {
            tracing::warn!("Refused the owner token to a proxied request");
            refuse(
                StatusCode::FORBIDDEN,
                "SAIL only hands out its owner token to direct local connections. Enter the \
                 token in this client's settings.",
            )
        }
        "/ws/voice" if !guard.is_owner(request.headers()) => {
            tracing::warn!("Refused a proxied voice socket without the owner token");
            refuse(
                StatusCode::UNAUTHORIZED,
                "Voice through a proxy needs the owner token (Authorization: Bearer).",
            )
        }
        _ => next.run(request).await,
    }
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (
        status,
        [("cache-control", "no-store")],
        Json(json!({ "error": message })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request as HttpRequest, routing::get};
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789";

    fn app() -> Router {
        Router::new()
            .route("/api/guardian/token", get(|| async { "token" }))
            .route("/ws/voice", get(|| async { "voice" }))
            .route("/api/health", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                ProxyGuard::new(Some(TOKEN)),
                guard,
            ))
    }

    async fn status(path: &str, headers: &[(&str, &str)]) -> StatusCode {
        let mut request = HttpRequest::builder().uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        app()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn direct_requests_are_unchanged() {
        assert_eq!(status("/api/guardian/token", &[]).await, StatusCode::OK);
        assert_eq!(status("/ws/voice", &[]).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn tunnelled_requests_never_get_the_owner_token() {
        for header in FORWARDING_HEADERS {
            assert_eq!(
                status("/api/guardian/token", &[(header, "203.0.113.9")]).await,
                StatusCode::FORBIDDEN,
                "{header}"
            );
        }
        // Not even with the token itself.
        let bearer = format!("Bearer {TOKEN}");
        assert_eq!(
            status(
                "/api/guardian/token",
                &[
                    ("cf-connecting-ip", "203.0.113.9"),
                    ("authorization", &bearer)
                ]
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn tunnelled_voice_needs_the_owner_token() {
        let proxied = ("cf-connecting-ip", "203.0.113.9");
        assert_eq!(
            status("/ws/voice", &[proxied]).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(
                "/ws/voice",
                &[
                    proxied,
                    (
                        "authorization",
                        "Bearer wrong-token-wrong-token-wrong-token"
                    )
                ]
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        let bearer = format!("Bearer {TOKEN}");
        assert_eq!(
            status("/ws/voice", &[proxied, ("authorization", &bearer)]).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn other_routes_keep_their_own_checks() {
        assert_eq!(
            status("/api/health", &[("cf-connecting-ip", "203.0.113.9")]).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn without_an_owner_token_proxied_voice_stays_closed() {
        let app = Router::new()
            .route("/ws/voice", get(|| async { "voice" }))
            .layer(axum::middleware::from_fn_with_state(
                ProxyGuard::new(None),
                guard,
            ));
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/ws/voice")
                    .header("cf-connecting-ip", "203.0.113.9")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
