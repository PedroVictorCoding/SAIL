//! Owner-only local speaker analysis and explicit enrollment; never an auth factor.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{
    aead, digest,
    rand::{SecureRandom, SystemRandom},
};
use sail_arena::MemoryStore;
use sail_guardian::authority::ApprovalAuthority;
use sail_mind::VoiceModelServices;
use sail_protocol::speaker::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Semaphore;
#[derive(Serialize, Deserialize)]
struct Profile {
    name: String,
    revision: String,
    values: Vec<f32>,
}
struct SpeakerState {
    store: Arc<MemoryStore>,
    analyzer: Arc<dyn SpeakerAnalyzer>,
    authority: Option<ApprovalAuthority>,
    key: Option<aead::LessSafeKey>,
    capacity: Semaphore,
}
pub fn router(
    store: Arc<MemoryStore>,
    services: VoiceModelServices,
    token: Option<&str>,
) -> Router {
    router_with_analyzer(store, Arc::new(services), token)
}
#[async_trait::async_trait]
pub trait SpeakerAnalyzer: Send + Sync {
    async fn analyze(&self, samples: &[i16]) -> Result<SpeakerOutput, String>;
}
#[async_trait::async_trait]
impl SpeakerAnalyzer for VoiceModelServices {
    async fn analyze(&self, samples: &[i16]) -> Result<SpeakerOutput, String> {
        self.analyze_speakers(samples)
            .await
            .map_err(|e| e.to_string())
    }
}
/// Trusted injection point for an analyzer; HTTP clients cannot choose a backend.
pub fn router_with_analyzer(
    store: Arc<MemoryStore>,
    analyzer: Arc<dyn SpeakerAnalyzer>,
    token: Option<&str>,
) -> Router {
    let authority = token.and_then(|t| ApprovalAuthority::new(t).ok());
    let key = token.filter(|_| authority.is_some()).map(|t| {
        let mut data = b"SAIL-speaker-profile-v1\0".to_vec();
        data.extend_from_slice(t.as_bytes());
        let bytes = digest::digest(&digest::SHA256, &data);
        aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, bytes.as_ref()).expect("SHA256 key length"),
        )
    });
    Router::new()
        .route("/request", post(request))
        .layer(axum::extract::DefaultBodyLimit::max(2_700_000))
        .layer(axum::middleware::map_response(
            |mut r: axum::response::Response| async move {
                r.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                r
            },
        ))
        .with_state(Arc::new(SpeakerState {
            store,
            analyzer,
            authority,
            key,
            capacity: Semaphore::new(1),
        }))
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    List,
    Delete {
        id: String,
    },
    Analyze {
        pcm: String,
    },
    Verify {
        pcm: String,
        profile_id: String,
    },
    Enroll {
        pcm: String,
        name: String,
        request_id: String,
        consent: bool,
    },
}
type Failure = (StatusCode, Json<Value>);
fn failure(code: StatusCode, message: &str) -> Failure {
    (code, Json(json!({"error":message})))
}
fn open(key: &aead::LessSafeKey, id: &str, sealed: &[u8]) -> Result<Profile, Failure> {
    if sealed.len() < 12 {
        return Err(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Invalid encrypted voice profile",
        ));
    }
    let nonce = aead::Nonce::try_assume_unique_for_key(&sealed[..12])
        .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "Invalid voice profile"))?;
    let mut data = sealed[12..].to_vec();
    let plain=key.open_in_place(nonce,aead::Aad::from(id.as_bytes()),&mut data).map_err(|_|failure(StatusCode::SERVICE_UNAVAILABLE,"Voice profiles cannot be unlocked; owner token may have changed. Delete and reenroll."))?;
    serde_json::from_slice(plain)
        .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "Invalid voice profile"))
}
fn seal(key: &aead::LessSafeKey, id: &str, profile: &Profile) -> Result<Vec<u8>, Failure> {
    let mut nonce = [0u8; 12];
    SystemRandom::new().fill(&mut nonce).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Secure randomness unavailable",
        )
    })?;
    let mut data = serde_json::to_vec(profile)
        .map_err(|_| failure(StatusCode::INTERNAL_SERVER_ERROR, "Profile encoding failed"))?;
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(id.as_bytes()),
        &mut data,
    )
    .map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Profile encryption failed",
        )
    })?;
    Ok([nonce.to_vec(), data].concat())
}
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() || a.iter().chain(b).any(|v| !v.is_finite()) {
        return None;
    }
    let dot: f64 = a.iter().zip(b).map(|(x, y)| *x as f64 * *y as f64).sum();
    let norm = |v: &[f32]| v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let denom = norm(a) * norm(b);
    (denom > 0.).then(|| (dot / denom).clamp(-1., 1.))
}
fn identify(
    output: &SpeakerOutput,
    profiles: &[(String, Profile)],
    claimed: Option<&str>,
) -> SpeakerAnalysis {
    let matches = output
        .embeddings
        .iter()
        .map(|e| {
            let mut scores: Vec<_> = profiles
                .iter()
                .filter(|(id, p)| {
                    p.revision == output.model_revision
                        && claimed.is_none_or(|wanted| id.as_str() == wanted)
                })
                .filter_map(|(id, p)| cosine(&e.values, &p.values).map(|score| (id, p, score)))
                .collect();
            scores.sort_by(|a, b| b.2.total_cmp(&a.2));
            let best = scores.first();
            let eligible = best.is_some_and(|(_, _, s)| *s >= 0.8)
                && scores.get(1).is_none_or(|s| best.unwrap().2 - s.2 >= 0.08);
            let accepted = eligible
                && claimed
                    .is_none_or(|wanted| best.is_some_and(|(id, _, _)| id.as_str() == wanted));
            SpeakerMatch {
                speaker: e.speaker.clone(),
                profile_id: accepted.then(|| best.unwrap().0.clone()),
                name: accepted.then(|| best.unwrap().1.name.clone()),
                similarity: best.map(|s| s.2),
                decision: if accepted {
                    "matched"
                } else if claimed.is_some() {
                    "not_verified"
                } else {
                    "unknown"
                }
                .into(),
            }
        })
        .collect();
    SpeakerAnalysis {
        version: VERSION,
        segments: output.segments.clone(),
        matches,
        advisory_only: true,
    }
}
async fn request(
    State(s): State<Arc<SpeakerState>>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Result<Json<Value>, Failure> {
    let authority = s.authority.as_ref().ok_or_else(|| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Owner authority unavailable",
        )
    })?;
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(|| failure(StatusCode::UNAUTHORIZED, "Owner authentication required"))?;
    if !authority.authenticates(token) {
        return Err(failure(
            StatusCode::UNAUTHORIZED,
            "Owner authentication required",
        ));
    }
    if let Request::Delete { id } = &request {
        let deleted = s
            .store
            .forget_speaker(id)
            .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "Profile deletion failed"))?;
        return Ok(Json(json!({"deleted":deleted})));
    }
    let (generation, rows) = s
        .store
        .speaker_profiles()
        .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "Profile store unavailable"))?;
    // Listing remains possible after a key rotation so locked ciphertext can be deleted.
    if matches!(request, Request::List) {
        let profiles:Vec<_>=rows.iter().map(|r|json!({"id":r.id,"name":open(s.key.as_ref().unwrap(),&r.id,&r.sealed).map(|p|p.name).unwrap_or_else(|_|"Locked profile — reenroll".into()),"created_at":r.created_at})).collect();
        return Ok(Json(json!({"profiles":profiles})));
    }
    let (pcm, enrollment, claimed) = match request {
        Request::Analyze { pcm } => (pcm, None, None),
        Request::Verify { pcm, profile_id } => (pcm, None, Some(profile_id)),
        Request::Enroll {
            pcm,
            name,
            request_id,
            consent,
        } => {
            if !consent
                || name.trim().is_empty()
                || name.chars().count() > 80
                || name.chars().any(char::is_control)
                || uuid::Uuid::parse_str(&request_id).is_err()
            {
                return Err(failure(
                    StatusCode::BAD_REQUEST,
                    "Explicit enrollment consent, a name and a valid request id are required",
                ));
            }
            if rows.iter().any(|r| r.id == request_id) {
                return Err(failure(
                    StatusCode::CONFLICT,
                    "Enrollment request already used; refresh profiles",
                ));
            }
            (pcm, Some((name.trim().to_owned(), request_id)), None)
        }
        _ => unreachable!(),
    };
    if claimed
        .as_ref()
        .is_some_and(|id| !rows.iter().any(|r| &r.id == id))
    {
        return Err(failure(
            StatusCode::NOT_FOUND,
            "Claimed speaker is not enrolled",
        ));
    }
    let _permit = s.capacity.try_acquire().map_err(|_| {
        failure(
            StatusCode::TOO_MANY_REQUESTS,
            "Another speaker analysis is running",
        )
    })?;
    if pcm.len() > 2_560_000 {
        return Err(failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Audio exceeds 60 seconds",
        ));
    }
    let bytes = STANDARD
        .decode(pcm)
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "Invalid PCM encoding"))?;
    if bytes.len() % 2 != 0 || bytes.len() < 32_000 || bytes.len() > MAX_SAMPLES * 2 {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Provide 1–60 seconds of PCM16 mono at 16 kHz",
        ));
    }
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let output=s.analyzer.analyze(&samples).await.map_err(|_|failure(StatusCode::SERVICE_UNAVAILABLE,"Local speaker worker unavailable, busy or invalid output; check the offline model setup"))?;
    output.validate(samples.len() as u64 / 16).map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Invalid speaker model output",
        )
    })?;
    if let Some((name, id)) = enrollment {
        let mut segments = output.segments.clone();
        segments.sort_by_key(|s| s.start_ms);
        let speech_ms: u64 = segments.iter().map(|s| s.end_ms - s.start_ms).sum();
        if output.embeddings.len() != 1
            || speech_ms < 3000
            || segments.windows(2).any(|w| w[0].end_ms > w[1].start_ms)
        {
            return Err(failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Enroll at least 3 seconds of one speaker without overlapping speech",
            ));
        }
        let profile = Profile {
            name: name.clone(),
            revision: output.model_revision,
            values: output.embeddings[0].values.clone(),
        };
        let sealed = seal(s.key.as_ref().unwrap(), &id, &profile)?;
        s.store
            .enroll_speaker(&id, &sealed, generation)
            .map_err(|_| {
                failure(
                    StatusCode::CONFLICT,
                    "Enrollment did not commit; profiles changed or storage is unavailable",
                )
            })?;
        return Ok(Json(json!({"id":id,"name":name,"enrolled":true})));
    }
    // Reload after inference: a deletion during processing must affect the result.
    let (_, rows) = s
        .store
        .speaker_profiles()
        .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "Profile store unavailable"))?;
    let profiles = rows
        .into_iter()
        .map(|r| open(s.key.as_ref().unwrap(), &r.id, &r.sealed).map(|p| (r.id, p)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(
        serde_json::to_value(identify(&output, &profiles, claimed.as_deref())).unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn output() -> SpeakerOutput {
        SpeakerOutput {
            version: 1,
            model_revision: "a".repeat(64),
            segments: vec![SpeakerSegment {
                speaker: "SPEAKER_00".into(),
                start_ms: 0,
                end_ms: 4000,
            }],
            embeddings: vec![SpeakerVector {
                speaker: "SPEAKER_00".into(),
                values: vec![1., 0.],
            }],
        }
    }
    #[test]
    fn uncertain_ambiguous_and_version_mismatched_matches_stay_unknown() {
        let profile = |name: &str, values: Vec<f32>, revision: String| Profile {
            name: name.into(),
            values,
            revision,
        };
        let a = "a".repeat(64);
        assert_eq!(
            identify(
                &output(),
                &[("1".into(), profile("A", vec![0., 1.], a.clone()))],
                None
            )
            .matches[0]
                .decision,
            "unknown"
        );
        let profiles = vec![
            ("1".into(), profile("A", vec![1., 0.], a.clone())),
            ("2".into(), profile("B", vec![1., 0.], a)),
        ];
        assert_eq!(
            identify(&output(), &profiles, None).matches[0].decision,
            "unknown"
        );
        assert_eq!(
            identify(&output(), &profiles, Some("1")).matches[0].decision,
            "matched"
        );
        assert_eq!(
            identify(
                &output(),
                &[("1".into(), profile("A", vec![1., 0.], "b".repeat(64)))],
                None
            )
            .matches[0]
                .decision,
            "unknown"
        );
        assert!(cosine(&[0., 0.], &[1., 0.]).is_none());
        assert!(cosine(&[f32::NAN], &[1.]).is_none());
    }
}
