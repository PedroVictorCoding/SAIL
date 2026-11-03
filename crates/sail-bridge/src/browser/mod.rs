//! Bridge's Chromium worker (vision §7.2): a pinned Chromium driven over CDP from Rust.
//!
//! A session is one browser process, one profile and one [`SiteGrant`]. The grant is
//! enforced inside the browser: every target (pages, popups, iframes, workers) is
//! auto-attached paused, request interception is switched on, and only then is it allowed
//! to run. Requests outside the grant fail before they leave the machine.
//!
//! Two profile kinds, as the vision requires:
//! - [`Profile::Ephemeral`]: a throwaway directory, removed when the session ends.
//! - [`Profile::Persistent`]: an isolated, long-lived directory holding a site login that
//!   the user created by hand. It always runs with Chromium's own sandbox; if the host
//!   cannot provide one, launch fails instead of falling back to `--no-sandbox`.

pub mod cdp;
pub mod grant;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

pub use cdp::{CdpConnection, CdpEvent};
pub use grant::{HostRule, RequestKind, SiteGrant, Verdict};

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("Chromium could not start: {0}")]
    Launch(String),
    #[error("Chromium's sandbox is unavailable on this host: {0}")]
    SandboxUnavailable(String),
    #[error("profile {0} is already open in another Chromium window; close it first")]
    ProfileInUse(PathBuf),
    #[error("DevTools protocol error: {0}")]
    Protocol(String),
    #[error("timed out waiting for {0}")]
    Timeout(String),
    #[error("the browser closed")]
    Closed,
    #[error("outside the site grant: {0}")]
    Denied(String),
    #[error("page script failed: {0}")]
    Script(String),
}

/// Where the browser keeps its state.
#[derive(Debug, Clone)]
pub enum Profile {
    Ephemeral,
    Persistent(PathBuf),
}

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// The pinned Chromium binary.
    pub binary: PathBuf,
    pub profile: Profile,
    /// A visible window the user can watch and take over. Persistent profiles default to it.
    pub headed: bool,
    /// Chromium's own sandbox. Only an ephemeral profile may turn it off (test fixtures on
    /// hosts that restrict user namespaces); a persistent profile refuses to launch without.
    pub chromium_sandbox: bool,
    /// Bound on each DevTools command and on startup.
    pub timeout: Duration,
}

impl LaunchOptions {
    pub fn new(binary: PathBuf, profile: Profile) -> Self {
        let headed = matches!(profile, Profile::Persistent(_));
        Self {
            binary,
            profile,
            headed,
            chromium_sandbox: true,
            timeout: Duration::from_secs(30),
        }
    }
}

/// The pinned Chromium: `SAIL_CHROMIUM` if set, otherwise none. SAIL does not guess at
/// whatever browser happens to be installed.
pub fn configured_binary() -> Result<PathBuf, BrowserError> {
    let path = std::env::var_os("SAIL_CHROMIUM")
        .map(PathBuf::from)
        .ok_or_else(|| {
            BrowserError::Launch("set SAIL_CHROMIUM to the pinned Chromium binary".into())
        })?;
    if !path.is_file() {
        return Err(BrowserError::Launch(format!(
            "SAIL_CHROMIUM={} is not a file",
            path.display()
        )));
    }
    Ok(path)
}

struct ProfileDir {
    path: PathBuf,
    remove_on_drop: bool,
}

impl Drop for ProfileDir {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

pub struct BrowserSession {
    child: Child,
    cdp: CdpConnection,
    page: String,
    grant: Arc<SiteGrant>,
    blocked: Arc<Mutex<Vec<String>>>,
    // Declared last: the process must be gone before an ephemeral profile is removed.
    _profile: ProfileDir,
}

const BLOCKED_HISTORY: usize = 64;

impl BrowserSession {
    /// Start Chromium under `grant` and open its home page.
    pub async fn launch(options: LaunchOptions, grant: SiteGrant) -> Result<Self, BrowserError> {
        let profile = match &options.profile {
            Profile::Ephemeral => {
                let path =
                    std::env::temp_dir().join(format!("sail-browser-{}", uuid::Uuid::new_v4()));
                create_private_dir(&path)?;
                ProfileDir {
                    path,
                    remove_on_drop: true,
                }
            }
            Profile::Persistent(path) => {
                if !options.chromium_sandbox {
                    return Err(BrowserError::SandboxUnavailable(
                        "a persistent (signed-in) profile never runs with --no-sandbox".into(),
                    ));
                }
                create_private_dir(path)?;
                ProfileDir {
                    path: path.clone(),
                    remove_on_drop: false,
                }
            }
        };
        let port_file = profile.path.join("DevToolsActivePort");
        let _ = std::fs::remove_file(&port_file);

        let mut command = Command::new(&options.binary);
        command
            .arg(format!("--user-data-dir={}", profile.path.display()))
            .arg("--remote-debugging-port=0")
            .arg("--remote-debugging-address=127.0.0.1")
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--disable-sync",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-default-apps",
                "--password-store=basic",
            ])
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if !options.headed {
            command.arg("--headless=new");
        }
        if !options.chromium_sandbox {
            command.arg("--no-sandbox");
        }
        let mut child = command
            .spawn()
            .map_err(|e| BrowserError::Launch(format!("{}: {e}", options.binary.display())))?;

        let stderr_tail = Arc::new(Mutex::new(String::new()));
        if let Some(mut stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let mut tail = tail.lock().expect("stderr tail");
                    tail.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if tail.len() > 8192 {
                        let cut = tail.len() - 8192;
                        let cut = (cut..tail.len())
                            .find(|i| tail.is_char_boundary(*i))
                            .unwrap_or(0);
                        tail.drain(..cut);
                    }
                }
            });
        }

        let endpoint = wait_for_endpoint(
            &mut child,
            &port_file,
            &profile.path,
            &stderr_tail,
            &options,
        )
        .await?;
        let cdp = CdpConnection::connect(&endpoint, options.timeout).await?;
        let grant = Arc::new(grant);
        let blocked: Arc<Mutex<Vec<String>>> = Arc::default();

        // Subscribe before auto-attach so no target can start unobserved.
        spawn_interceptor(cdp.clone(), grant.clone(), blocked.clone());
        cdp.call(
            None,
            "Browser.setDownloadBehavior",
            json!({ "behavior": "deny" }),
        )
        .await?;
        cdp.call(
            None,
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
        )
        .await?;

        let mut events = cdp.subscribe();
        let created = cdp
            .call(None, "Target.createTarget", json!({ "url": "about:blank" }))
            .await?;
        let target_id = created["targetId"]
            .as_str()
            .ok_or_else(|| BrowserError::Protocol("createTarget returned no targetId".into()))?
            .to_owned();
        let page = tokio::time::timeout(options.timeout, async {
            loop {
                match events.recv().await {
                    Ok(event)
                        if event.method == "Target.attachedToTarget"
                            && event.params["targetInfo"]["targetId"] == target_id =>
                    {
                        break event.params["sessionId"].as_str().map(str::to_owned);
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break None,
                }
            }
        })
        .await
        .map_err(|_| BrowserError::Timeout("the page target".into()))?
        .ok_or(BrowserError::Closed)?;

        // The window Chromium opened itself is not needed; every page is a target we made.
        if let Ok(targets) = cdp.call(None, "Target.getTargets", json!({})).await {
            for info in targets["targetInfos"].as_array().into_iter().flatten() {
                if info["type"] == "page" && info["targetId"] != target_id {
                    cdp.send(
                        None,
                        "Target.closeTarget",
                        json!({ "targetId": info["targetId"] }),
                    );
                }
            }
        }

        cdp.call(Some(&page), "Page.enable", json!({})).await?;
        cdp.call(Some(&page), "Runtime.enable", json!({})).await?;
        let session = Self {
            child,
            cdp,
            page,
            grant,
            blocked,
            _profile: profile,
        };
        let home = session.grant.home.to_string();
        session.navigate(&home).await?;
        Ok(session)
    }

    pub fn grant(&self) -> &SiteGrant {
        &self.grant
    }

    /// Requests the grant refused, most recent last (bounded).
    pub fn blocked_requests(&self) -> Vec<String> {
        self.blocked.lock().expect("blocked").clone()
    }

    /// Open `url` in the page, if the grant allows it as a document. Waits for the load.
    pub async fn navigate(&self, url: &str) -> Result<(), BrowserError> {
        if let Verdict::Deny(reason) = self.grant.check(url, RequestKind::Document) {
            return Err(BrowserError::Denied(reason));
        }
        let mut events = self.cdp.subscribe();
        let result = self
            .cdp
            .call(Some(&self.page), "Page.navigate", json!({ "url": url }))
            .await?;
        if let Some(error) = result["errorText"].as_str() {
            return Err(BrowserError::Protocol(format!(
                "navigation to {url} failed: {error}"
            )));
        }
        let page = self.page.clone();
        tokio::time::timeout(Duration::from_secs(60), async move {
            loop {
                match events.recv().await {
                    Ok(e)
                        if e.method == "Page.loadEventFired"
                            && e.session_id == Some(page.clone()) =>
                    {
                        break Ok(());
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break Err(BrowserError::Closed),
                }
            }
        })
        .await
        .map_err(|_| BrowserError::Timeout(format!("{url} to load")))?
    }

    /// Evaluate a script in the page and return its JSON value. Page content is data: the
    /// caller decides what a returned string means, never the page.
    pub async fn evaluate(&self, expression: &str) -> Result<Value, BrowserError> {
        let result = self
            .cdp
            .call(
                Some(&self.page),
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                    "userGesture": true
                }),
            )
            .await?;
        if let Some(details) = result.get("exceptionDetails") {
            let text = details["exception"]["description"]
                .as_str()
                .or_else(|| details["text"].as_str())
                .unwrap_or("exception");
            return Err(BrowserError::Script(text.to_owned()));
        }
        Ok(result["result"]["value"].clone())
    }

    /// Poll `predicate` (a script returning a boolean) until true or `timeout`.
    pub async fn wait_until(
        &self,
        predicate: &str,
        timeout: Duration,
        what: &str,
    ) -> Result<(), BrowserError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.evaluate(predicate).await? == Value::Bool(true) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(BrowserError::Timeout(what.to_owned()));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Press a key with modifiers (`2` = Ctrl), as a user would.
    pub async fn press_key(
        &self,
        key: &str,
        code: &str,
        modifiers: u32,
    ) -> Result<(), BrowserError> {
        for kind in ["keyDown", "keyUp"] {
            self.cdp
                .call(
                    Some(&self.page),
                    "Input.dispatchKeyEvent",
                    json!({ "type": kind, "key": key, "code": code, "modifiers": modifiers }),
                )
                .await?;
        }
        Ok(())
    }

    /// Type text as keyboard input into the focused element.
    pub async fn insert_text(&self, text: &str) -> Result<(), BrowserError> {
        self.cdp
            .call(
                Some(&self.page),
                "Input.insertText",
                json!({ "text": text }),
            )
            .await?;
        Ok(())
    }

    /// Resolve once the user closes the window (or the browser exits).
    pub async fn wait_for_exit(&mut self) -> Result<(), BrowserError> {
        self.child
            .wait()
            .await
            .map(|_| ())
            .map_err(|e| BrowserError::Launch(e.to_string()))
    }

    /// Close the browser cleanly so the profile is flushed to disk.
    pub async fn close(mut self) -> Result<(), BrowserError> {
        let _ = self.cdp.call(None, "Browser.close", json!({})).await;
        match tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await {
            Ok(_) => Ok(()),
            Err(_) => {
                let _ = self.child.kill().await;
                Ok(())
            }
        }
    }
}

/// Attach to every new target paused, switch on interception, then let it run; and answer
/// every paused request against the grant.
fn spawn_interceptor(cdp: CdpConnection, grant: Arc<SiteGrant>, blocked: Arc<Mutex<Vec<String>>>) {
    let mut events = cdp.subscribe();
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                // Missing a paused request would stall it, not leak it; keep going.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "browser interceptor lagged");
                    continue;
                }
                Err(_) => break,
            };
            match event.method.as_str() {
                "Target.attachedToTarget" => {
                    let Some(session) = event.params["sessionId"].as_str() else {
                        continue;
                    };
                    // Commands on one session run in order: interception is on before the
                    // target resumes.
                    cdp.send(
                        Some(session),
                        "Fetch.enable",
                        json!({ "patterns": [{ "urlPattern": "*" }] }),
                    );
                    cdp.send(
                        Some(session),
                        "Target.setAutoAttach",
                        json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
                    );
                    cdp.send(Some(session), "Runtime.runIfWaitingForDebugger", json!({}));
                }
                "Fetch.requestPaused" => {
                    let session = event.session_id.as_deref();
                    let request_id = event.params["requestId"].clone();
                    let url = event.params["request"]["url"].as_str().unwrap_or("");
                    let kind = if event.params["resourceType"] == "Document" {
                        RequestKind::Document
                    } else {
                        RequestKind::Subresource
                    };
                    match grant.check(url, kind) {
                        Verdict::Allow => cdp.send(
                            session,
                            "Fetch.continueRequest",
                            json!({ "requestId": request_id }),
                        ),
                        Verdict::Deny(reason) => {
                            tracing::info!(grant = %grant.name, %reason, "browser request blocked");
                            let mut list = blocked.lock().expect("blocked");
                            if list.len() == BLOCKED_HISTORY {
                                list.remove(0);
                            }
                            list.push(url.chars().take(512).collect());
                            drop(list);
                            cdp.send(
                                session,
                                "Fetch.failRequest",
                                json!({ "requestId": request_id, "errorReason": "BlockedByClient" }),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

async fn wait_for_endpoint(
    child: &mut Child,
    port_file: &Path,
    profile: &Path,
    stderr_tail: &Arc<Mutex<String>>,
    options: &LaunchOptions,
) -> Result<String, BrowserError> {
    let deadline = tokio::time::Instant::now() + options.timeout;
    loop {
        if let Ok(contents) = std::fs::read_to_string(port_file) {
            let mut lines = contents.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next())
                && port.parse::<u16>().is_ok()
                && path.starts_with("/devtools/browser/")
            {
                return Ok(format!("ws://127.0.0.1:{port}{path}"));
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            // Give the stderr reader a moment to drain the fatal message.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let tail = stderr_tail.lock().expect("stderr tail").clone();
            if tail.contains("No usable sandbox") {
                return Err(BrowserError::SandboxUnavailable(
                    "the host restricts unprivileged user namespaces; install \
                     deploy/apparmor/sail-chromium for the pinned binary"
                        .into(),
                ));
            }
            if status.success() || tail.contains("ProcessSingleton") {
                // Chromium handed the launch to a window already using this profile.
                return Err(BrowserError::ProfileInUse(profile.to_path_buf()));
            }
            let last = tail.lines().rev().take(5).collect::<Vec<_>>();
            return Err(BrowserError::Launch(format!(
                "exited with {status}: {}",
                last.into_iter().rev().collect::<Vec<_>>().join(" | ")
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(BrowserError::Timeout("Chromium's DevTools endpoint".into()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn create_private_dir(path: &Path) -> Result<(), BrowserError> {
    std::fs::create_dir_all(path)
        .map_err(|e| BrowserError::Launch(format!("profile {}: {e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| BrowserError::Launch(format!("profile {}: {e}", path.display())))?;
    }
    Ok(())
}
