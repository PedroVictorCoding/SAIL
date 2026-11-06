//! Transcript-only adapter for the existing host STT worker. Never starts a dialogue turn.
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use sail_guardian::authority::ApprovalAuthority;
use sail_mind::VoiceModelServices;
use serde_json::{Value, json};
use std::sync::Arc;

// One minute of 16 kHz, signed 16-bit mono PCM, matching capture.js.
const MAX_BYTES: usize = 16000 * 2 * 60;
struct DictationState {
    authority: Option<ApprovalAuthority>,
    stt: crate::LocalStreamingStt,
    busy: tokio::sync::Semaphore,
}
type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

pub fn router(services: VoiceModelServices, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/transcribe", post(transcribe))
        .layer(DefaultBodyLimit::max(MAX_BYTES))
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(Arc::new(DictationState {
            authority,
            stt: crate::LocalStreamingStt::with_services(services),
            busy: tokio::sync::Semaphore::new(1),
        }))
}
fn failure(code: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (code, Json(json!({"error": message})))
}
fn decode_pcm(body: &[u8]) -> Result<Vec<i16>, (StatusCode, Json<Value>)> {
    if body.is_empty() || body.len() % 2 != 0 || body.len() > MAX_BYTES {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Provide up to 60 seconds of 16 kHz mono PCM audio.",
        ));
    }
    Ok(body
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect())
}
async fn transcribe(
    State(s): State<Arc<DictationState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Reply {
    let authority = s.authority.as_ref().ok_or_else(|| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Owner authentication unavailable.",
        )
    })?;
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if !token.is_some_and(|token| authority.authenticates(token)) {
        return Err(failure(
            StatusCode::UNAUTHORIZED,
            "Owner authentication required.",
        ));
    }
    if headers.get("content-type").and_then(|v| v.to_str().ok()) != Some("application/octet-stream")
    {
        return Err(failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Expected 16 kHz signed 16-bit little-endian mono PCM.",
        ));
    }
    let samples = decode_pcm(&body)?;
    let _permit = s.busy.try_acquire().map_err(|_| {
        failure(
            StatusCode::TOO_MANY_REQUESTS,
            "Dictation is busy. Try again shortly.",
        )
    })?;
    let text = tokio::time::timeout(std::time::Duration::from_secs(90), s.stt.finalize(&samples))
        .await
        .map_err(|_| {
            failure(
                StatusCode::GATEWAY_TIMEOUT,
                "Transcription timed out. Try again.",
            )
        })?
        .map_err(|_| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Local speech recognition is unavailable. Try again or type your answer.",
            )
        })?;
    Ok(Json(json!({"text": text})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    #[test]
    fn pcm_is_bounded_aligned_and_little_endian() {
        assert_eq!(
            decode_pcm(&[0, 128, 255, 127]).unwrap(),
            vec![i16::MIN, i16::MAX]
        );
        for bad in [vec![], vec![1], vec![0; MAX_BYTES + 2]] {
            assert!(decode_pcm(&bad).is_err());
        }
    }
    #[tokio::test]
    async fn requires_owner_and_valid_audio_before_contacting_worker() {
        let token = "dictation-test-owner-token-0123456789";
        let app = router(
            VoiceModelServices::new(),
            Some(ApprovalAuthority::new(token).unwrap()),
        );
        for (auth, content, body, expected) in [
            (
                "",
                "application/octet-stream",
                vec![0, 0],
                StatusCode::UNAUTHORIZED,
            ),
            (
                token,
                "audio/webm",
                vec![0, 0],
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ),
            (
                token,
                "application/octet-stream",
                vec![0],
                StatusCode::BAD_REQUEST,
            ),
            (
                token,
                "application/octet-stream",
                vec![0; MAX_BYTES + 2],
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::post("/transcribe")
                        .header("authorization", format!("Bearer {auth}"))
                        .header("content-type", content)
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
    }
}
