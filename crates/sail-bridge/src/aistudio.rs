//! Assisted use of Google AI Studio through the user's own signed-in browser profile.
//!
//! This is the user's tool, driven on their behalf: the user signs in by hand once
//! (`sail_aistudio login`), and each `ask` is something the user asked for. It is not
//! registered as an agent tool, so no model can call it on its own.
//!
//! What it may do is fixed in code: a [`SiteGrant`] limited to Google's own hosts with
//! account pages (Gmail, Drive, account settings…) refused as pages, a daily request
//! quota that fails closed, and a usage log holding a digest of each prompt, not its text.
//!
//! The page selectors are AI Studio's markup as SAIL understands it, kept in one table.
//! They have not been verified against a signed-in session in CI. `probe` reports which
//! ones match, and every action fails with [`AiStudioError::Ui`] rather than guessing
//! when they don't.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::browser::{BrowserError, BrowserSession, HostRule, LaunchOptions, Profile, SiteGrant};

pub const HOME: &str = "https://aistudio.google.com/prompts/new_chat";

/// Bumped whenever the selector table changes, and recorded in the usage log.
pub const SELECTORS_VERSION: &str = "2026-11-03.1";

/// Candidates in order; the first that matches a visible element wins.
pub const PROMPT_INPUT: &[&str] = &[
    "ms-prompt-input-wrapper textarea",
    "ms-chunk-input textarea",
    "textarea[aria-label*='prompt' i]",
    "textarea[placeholder*='prompt' i]",
];
pub const RUN_BUTTON: &[&str] = &[
    "ms-run-button button",
    "run-button button",
    "button[aria-label='Run']",
];
/// Model turns only: a selector that also matches the user's turn would read the prompt
/// back as the answer.
pub const MODEL_TURN: &[&str] = &[
    "ms-chat-turn .chat-turn-container.model",
    "ms-chat-turn [data-turn-role='Model']",
];

#[derive(Debug, thiserror::Error)]
pub enum AiStudioError {
    #[error(transparent)]
    Browser(#[from] BrowserError),
    #[error("not signed in to AI Studio in the SAIL profile; run `sail_aistudio login`")]
    NotSignedIn,
    #[error("AI Studio's page did not match SAIL's selectors ({0}); run `sail_aistudio probe`")]
    Ui(String),
    #[error("daily AI Studio quota reached ({used}/{limit}); it resets at local midnight")]
    QuotaExhausted { used: u32, limit: u32 },
    #[error("usage records: {0}")]
    Records(String),
}

/// The grant for the AI Studio profile.
pub fn grant() -> SiteGrant {
    let suffix = |h: &str| HostRule::Suffix(h.to_owned());
    let exact = |h: &str| HostRule::Exact(h.to_owned());
    SiteGrant {
        name: "aistudio".into(),
        home: url::Url::parse(HOME).expect("static URL"),
        // AI Studio, Google sign-in, and the static/API hosts both load from.
        allow: vec![
            suffix("google.com"),
            suffix("gstatic.com"),
            suffix("googleapis.com"),
            suffix("googleusercontent.com"),
        ],
        // The same account cookie opens these; the session may not show them as pages.
        deny_documents: vec![
            exact("mail.google.com"),
            exact("drive.google.com"),
            exact("docs.google.com"),
            exact("myaccount.google.com"),
            exact("photos.google.com"),
            exact("contacts.google.com"),
            exact("calendar.google.com"),
            exact("pay.google.com"),
            exact("payments.google.com"),
            exact("takeout.google.com"),
            exact("admin.google.com"),
            exact("console.cloud.google.com"),
        ],
        allow_loopback_http: false,
    }
}

/// Where SAIL keeps the AI Studio profile and its usage records.
#[derive(Debug, Clone)]
pub struct Paths {
    pub profile: PathBuf,
    pub records: PathBuf,
}

impl Paths {
    pub fn default_location() -> Self {
        Self {
            profile: sail_common::storage::data_path("bridge/browser-profiles/aistudio"),
            records: sail_common::storage::data_path("bridge/aistudio"),
        }
    }
}

/// A per-day request budget, persisted so restarts don't reset it.
pub struct UsageQuota {
    path: PathBuf,
    limit: u32,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
struct QuotaState {
    day: String,
    used: u32,
}

impl UsageQuota {
    pub fn new(path: PathBuf, limit: u32) -> Self {
        Self { path, limit }
    }

    pub fn limit(&self) -> u32 {
        self.limit
    }

    /// Requests used on `today`. An unreadable file is an error, never zero.
    pub fn used(&self, today: &str) -> Result<u32, AiStudioError> {
        Ok(self
            .load()?
            .filter(|state| state.day == today)
            .map_or(0, |state| state.used))
    }

    /// Spend one request on `today`, or refuse.
    pub fn consume(&self, today: &str) -> Result<u32, AiStudioError> {
        let used = self.used(today)?;
        if used >= self.limit {
            return Err(AiStudioError::QuotaExhausted {
                used,
                limit: self.limit,
            });
        }
        let state = QuotaState {
            day: today.to_owned(),
            used: used + 1,
        };
        write_atomic(
            &self.path,
            &serde_json::to_vec(&state).expect("quota serializes"),
        )?;
        Ok(self.limit - state.used)
    }

    fn load(&self) -> Result<Option<QuotaState>, AiStudioError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
                AiStudioError::Records(format!("{} is corrupt: {e}", self.path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AiStudioError::Records(format!(
                "{}: {e}",
                self.path.display()
            ))),
        }
    }
}

/// One line of the usage log. The prompt is recorded by digest and length only.
#[derive(Debug, Serialize, Deserialize)]
pub struct UsageRecord {
    pub at: String,
    pub prompt_sha256: String,
    pub prompt_chars: usize,
    pub response_chars: Option<usize>,
    pub outcome: String,
    pub selectors: String,
}

pub fn append_usage(path: &Path, record: &UsageRecord) -> Result<(), AiStudioError> {
    let mut line = serde_json::to_vec(record).expect("record serializes");
    line.push(b'\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(&line))
        .map_err(|e| AiStudioError::Records(format!("{}: {e}", path.display())))
}

pub fn sha256_hex(text: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Which selector candidates match on the current page.
#[derive(Debug, Serialize)]
pub struct Probe {
    pub url: String,
    pub signed_in: bool,
    pub prompt_input: Option<String>,
    pub run_button: Option<String>,
    pub model_turns: Option<String>,
    pub selectors_version: &'static str,
}

pub struct AiStudio {
    session: BrowserSession,
}

impl AiStudio {
    /// Open the signed-in profile at AI Studio's new-chat page.
    pub async fn open(
        binary: PathBuf,
        profile: &Path,
        headed: bool,
    ) -> Result<Self, AiStudioError> {
        let mut options = LaunchOptions::new(binary, Profile::Persistent(profile.to_path_buf()));
        options.headed = headed;
        let session = BrowserSession::launch(options, grant()).await?;
        Ok(Self { session })
    }

    pub fn session(&mut self) -> &mut BrowserSession {
        &mut self.session
    }

    pub async fn close(self) -> Result<(), AiStudioError> {
        Ok(self.session.close().await?)
    }

    async fn current_host(&self) -> Result<String, AiStudioError> {
        Ok(self
            .session
            .evaluate("location.host")
            .await?
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }

    pub async fn probe(&self) -> Result<Probe, AiStudioError> {
        let url = self
            .session
            .evaluate("location.href")
            .await?
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let signed_in = self.current_host().await? == "aistudio.google.com";
        Ok(Probe {
            url,
            signed_in,
            prompt_input: self.first_match(PROMPT_INPUT).await?,
            run_button: self.first_match(RUN_BUTTON).await?,
            model_turns: self.first_match(MODEL_TURN).await?,
            selectors_version: SELECTORS_VERSION,
        })
    }

    async fn first_match(&self, candidates: &[&str]) -> Result<Option<String>, AiStudioError> {
        let script = format!(
            "(() => {{ for (const s of {}) {{ if (document.querySelector(s)) return s; }} return null; }})()",
            serde_json::to_string(candidates).expect("selectors serialize")
        );
        Ok(self
            .session
            .evaluate(&script)
            .await?
            .as_str()
            .map(str::to_owned))
    }

    async fn model_turn_count(&self) -> Result<u64, AiStudioError> {
        let script = format!(
            "(() => {{ for (const s of {}) {{ const n = document.querySelectorAll(s).length; if (n) return n; }} return 0; }})()",
            serde_json::to_string(MODEL_TURN).expect("selectors serialize")
        );
        Ok(self.session.evaluate(&script).await?.as_u64().unwrap_or(0))
    }

    /// Send one prompt in a fresh chat and return the model's reply as plain text.
    pub async fn ask(&self, prompt: &str, timeout: Duration) -> Result<String, AiStudioError> {
        self.session.navigate(HOME).await?;
        if self.current_host().await? != "aistudio.google.com" {
            return Err(AiStudioError::NotSignedIn);
        }
        let input_ready = format!(
            "{}.some(s => document.querySelector(s))",
            serde_json::to_string(PROMPT_INPUT).expect("selectors serialize")
        );
        self.session
            .wait_until(
                &input_ready,
                Duration::from_secs(30),
                "AI Studio's prompt box",
            )
            .await
            .map_err(|_| AiStudioError::Ui("no prompt input".into()))?;
        let before = self.model_turn_count().await?;

        let focus = format!(
            "(() => {{ for (const s of {}) {{ const e = document.querySelector(s); if (e) {{ e.focus(); return true; }} }} return false; }})()",
            serde_json::to_string(PROMPT_INPUT).expect("selectors serialize")
        );
        if self.session.evaluate(&focus).await? != Value::Bool(true) {
            return Err(AiStudioError::Ui(
                "prompt input could not be focused".into(),
            ));
        }
        // Typed as keyboard input so the page's own handlers see it, exactly as when the
        // user types.
        self.session.insert_text(prompt).await?;
        self.session.press_key("Enter", "Enter", 2).await?;

        let started = tokio::time::Instant::now();
        let mut last = String::new();
        let mut stable_since = tokio::time::Instant::now();
        loop {
            if started.elapsed() > timeout {
                return Err(BrowserError::Timeout("AI Studio's reply".into()).into());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            if self.model_turn_count().await? <= before {
                continue;
            }
            let text = self.last_model_text().await?;
            let running = self.is_running().await?;
            if text != last {
                last = text;
                stable_since = tokio::time::Instant::now();
            } else if !running
                && !last.trim().is_empty()
                && stable_since.elapsed() >= Duration::from_secs(2)
            {
                return Ok(last.trim().to_owned());
            }
        }
    }

    /// The run button shows "Stop" while a reply is streaming.
    async fn is_running(&self) -> Result<bool, AiStudioError> {
        let script = format!(
            "(() => {{ for (const s of {}) {{ const b = document.querySelector(s); if (b) return /stop/i.test((b.getAttribute('aria-label') || '') + ' ' + b.textContent); }} return false; }})()",
            serde_json::to_string(RUN_BUTTON).expect("selectors serialize")
        );
        Ok(self.session.evaluate(&script).await? == Value::Bool(true))
    }

    /// The last model turn as text, without icon ligatures and button labels.
    async fn last_model_text(&self) -> Result<String, AiStudioError> {
        let script = format!(
            r#"(() => {{
  let turns = [];
  for (const s of {sel}) {{ turns = document.querySelectorAll(s); if (turns.length) break; }}
  const turn = turns[turns.length - 1];
  if (!turn) return '';
  const clone = turn.cloneNode(true);
  clone.querySelectorAll('button, mat-icon, .material-symbols-outlined, .material-icons, ms-thought-chunk, script, style').forEach(e => e.remove());
  const block = /^(P|DIV|LI|PRE|H[1-6]|TR|BLOCKQUOTE|UL|OL|TABLE)$/;
  let out = '';
  const walk = n => {{
    if (n.nodeType === 3) {{ out += n.nodeValue; return; }}
    if (n.nodeName === 'BR') {{ out += '\n'; return; }}
    n.childNodes.forEach(walk);
    if (block.test(n.nodeName) && !out.endsWith('\n')) out += '\n';
  }};
  walk(clone);
  return out.replace(/\n{{3,}}/g, '\n\n');
}})()"#,
            sel = serde_json::to_string(MODEL_TURN).expect("selectors serialize")
        );
        Ok(self
            .session
            .evaluate(&script)
            .await?
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), AiStudioError> {
    let fail = |e: std::io::Error| AiStudioError::Records(format!("{}: {e}", path.display()));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(fail)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(fail)?;
    std::fs::rename(&tmp, path).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::{RequestKind, Verdict};

    #[test]
    fn grant_keeps_the_session_on_ai_studio_and_sign_in() {
        let g = grant();
        for url in [
            HOME,
            "https://accounts.google.com/ServiceLogin",
            "https://fonts.gstatic.com/s/x.woff2",
            "https://alkalimakersuite-pa.clients6.google.com/$rpc/x",
        ] {
            assert_eq!(
                g.check(url, RequestKind::Subresource),
                Verdict::Allow,
                "{url}"
            );
        }
        assert_eq!(g.check(HOME, RequestKind::Document), Verdict::Allow);
        for url in [
            "https://mail.google.com/mail/u/0/",
            "https://drive.google.com/drive/my-drive",
            "https://myaccount.google.com/security",
        ] {
            assert!(
                matches!(g.check(url, RequestKind::Document), Verdict::Deny(_)),
                "{url} must not open as a page"
            );
        }
        for url in [
            "https://example.com/",
            "https://google.com.evil.io/",
            "http://aistudio.google.com/",
        ] {
            assert!(
                matches!(g.check(url, RequestKind::Subresource), Verdict::Deny(_)),
                "{url} is outside the grant"
            );
        }
    }

    #[test]
    fn quota_counts_per_day_refuses_at_the_limit_and_fails_closed_on_corruption() {
        let dir = std::env::temp_dir().join(format!("sail-quota-{}", uuid::Uuid::new_v4()));
        let path = dir.join("quota.json");
        let quota = UsageQuota::new(path.clone(), 2);
        assert_eq!(quota.consume("2026-11-03").unwrap(), 1);
        assert_eq!(quota.consume("2026-11-03").unwrap(), 0);
        assert!(matches!(
            quota.consume("2026-11-03"),
            Err(AiStudioError::QuotaExhausted { used: 2, limit: 2 })
        ));
        // A new day starts fresh.
        assert_eq!(quota.consume("2026-11-04").unwrap(), 1);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            quota.consume("2026-11-05"),
            Err(AiStudioError::Records(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn usage_log_never_contains_the_prompt() {
        let dir = std::env::temp_dir().join(format!("sail-usage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("usage.jsonl");
        let prompt = "my private draft about the thesis";
        append_usage(
            &path,
            &UsageRecord {
                at: "2026-11-03T12:00:00Z".into(),
                prompt_sha256: sha256_hex(prompt),
                prompt_chars: prompt.chars().count(),
                response_chars: Some(10),
                outcome: "ok".into(),
                selectors: SELECTORS_VERSION.into(),
            },
        )
        .unwrap();
        let log = std::fs::read_to_string(&path).unwrap();
        assert!(!log.contains("thesis"));
        assert!(log.contains(&sha256_hex(prompt)));
        let _ = std::fs::remove_dir_all(dir);
    }
}
