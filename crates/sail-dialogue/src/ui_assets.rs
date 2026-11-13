//! The browser interface's pages, scripts and styles, and their optional hot reload.
//!
//! Every file is compiled into the binary, so a normal SAIL serves exactly what it was built
//! with. For interface work, set `SAIL_WEB_DEV`:
//!
//! - `SAIL_WEB_DEV=1` serves these files from the source tree the binary was built from;
//! - `SAIL_WEB_DEV=/path/to/checkout` serves them from that checkout (for example a worktree).
//!
//! In that mode each request reads the file from disk, pages get a small client that listens
//! on `/__dev/events`, stylesheet edits are swapped in place (the conversation and map keep
//! their state), any other edit reloads the page that uses the file, and a restarted server
//! (for example after a Rust rebuild) reloads open pages. Only the files in [`ASSETS`] are
//! ever served; nothing else on disk becomes reachable.
use axum::{
    Router,
    http::{HeaderValue, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use futures_util::stream::{self, Stream};
use serde_json::json;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::sync::broadcast;

/// One file of the interface.
pub struct Asset {
    /// The path it is served at.
    pub url: &'static str,
    /// Its source file, relative to the `sail-dialogue` crate.
    pub file: &'static str,
    /// The contents compiled into this binary.
    pub embedded: &'static str,
    pub content_type: &'static str,
}

impl Asset {
    fn is_html(&self) -> bool {
        self.content_type.starts_with("text/html")
    }
    fn is_css(&self) -> bool {
        self.content_type.starts_with("text/css")
    }
}

macro_rules! assets {
    ($(($url:literal, $file:literal, $content_type:expr)),* $(,)?) => {
        /// Every interface file SAIL serves.
        pub const ASSETS: &[Asset] = &[$(Asset {
            url: $url,
            file: $file,
            embedded: include_str!(concat!("../", $file)),
            content_type: $content_type,
        }),*];
    };
}

const HTML: &str = "text/html; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";

assets![
    ("/", "web/index.html", HTML),
    ("/tracker", "web/tracker.html", HTML),
    ("/moodle", "web/moodle.html", HTML),
    ("/moodle.js", "web/moodle.js", JS),
    ("/moodle.css", "web/moodle.css", CSS),
    ("/initiative.js", "web/initiative.js", JS),
    ("/initiative.css", "web/initiative.css", CSS),
    ("/escalation-usage.js", "web/escalation-usage.js", JS),
    ("/escalation-usage.css", "web/escalation-usage.css", CSS),
    ("/capital", "web/capital.html", HTML),
    ("/capital.css", "web/capital.css", CSS),
    ("/capital-core.js", "web/capital-core.js", JS),
    ("/capital-today.js", "web/capital-today.js", JS),
    ("/capital-portfolio.js", "web/capital-portfolio.js", JS),
    ("/capital-income.js", "web/capital-income.js", JS),
    ("/capital-outlook.js", "web/capital-outlook.js", JS),
    ("/capital-record.js", "web/capital-record.js", JS),
    ("/capital-settings.js", "web/capital-settings.js", JS),
    ("/capital-today.css", "web/capital-today.css", CSS),
    ("/capital-portfolio.css", "web/capital-portfolio.css", CSS),
    ("/capital-income.css", "web/capital-income.css", CSS),
    ("/capital-outlook.css", "web/capital-outlook.css", CSS),
    ("/capital-record.css", "web/capital-record.css", CSS),
    ("/capital-settings.css", "web/capital-settings.css", CSS),
    ("/reader", "web/reader.html", HTML),
    ("/reader.js", "web/reader.js", JS),
    ("/reader.css", "web/reader.css", CSS),
    ("/reader-audio.js", "web/reader-audio.js", JS),
    ("/music", "web/music.html", HTML),
    ("/music.js", "web/music.js", JS),
    ("/music.css", "web/music.css", CSS),
    ("/tracker.js", "web/tracker.js", JS),
    ("/tracker.css", "web/tracker.css", CSS),
    ("/toolsmap.js", "web/toolsmap.js", JS),
    ("/flow.js", "web/flow.js", JS),
    ("/flow.css", "web/flow.css", CSS),
    ("/toolsmap.css", "web/toolsmap.css", CSS),
    ("/card-modal.js", "web/card-modal.js", JS),
    ("/markdown.js", "web/markdown.js", JS),
    ("/ambient-state.js", "web/ambient-state.js", JS),
    ("/ambient-backdrop.js", "web/ambient-backdrop.js", JS),
    ("/ambient-backdrop.css", "web/ambient-backdrop.css", CSS),
    ("/voice.js", "web/voice.js", JS),
    ("/forge.js", "web/forge.js", JS),
    ("/approvals.js", "web/approvals.js", JS),
    ("/app.css", "web/app.css", CSS),
    ("/tokens.css", "web/tokens.css", CSS),
    ("/learn.js", "web/learn.js", JS),
    ("/model-picker.js", "web/model-picker.js", JS),
    ("/courses.js", "web/courses.js", JS),
    ("/courses.css", "web/courses.css", CSS),
    ("/learn-factory.js", "web/learn-factory.js", JS),
    ("/learn-factory.css", "web/learn-factory.css", CSS),
    ("/memory.js", "web/memory.js", JS),
    ("/subsystem-pages.js", "web/subsystem-pages.js", JS),
    ("/briefing.js", "web/briefing.js", JS),
    ("/ui.js", "web/ui.js", JS),
    (
        "/icons.svg",
        "../../assets/source/icons/sail-icons.svg",
        "image/svg+xml; charset=utf-8"
    ),
    ("/client-speech.js", "web/client-speech.js", JS),
    (
        "/client-speech-worker.js",
        "web/client-speech-worker.js",
        JS
    ),
    ("/capture.js", "web/capture.js", JS),
    ("/mermaid.js", "web/mermaid.js", JS),
    ("/plans.js", "web/plans.js", JS),
    ("/reminders.js", "web/reminders.js", JS),
    ("/terminal.js", "web/terminal.js", JS),
    ("/vendor/xterm.js", "web/vendor/xterm/xterm.js", JS),
    ("/vendor/addon-fit.js", "web/vendor/xterm/addon-fit.js", JS),
    ("/vendor/xterm.css", "web/vendor/xterm/xterm.css", CSS),
    (
        "/vendor/paper-shaders.js",
        "web/vendor/paper-shaders/paper-shaders.js",
        JS
    ),
    (
        "/vendor/liquidglass.js",
        "web/vendor/liquidglass/liquidglass.js",
        JS
    ),
];

/// The client injected into pages in dev mode.
const RELOAD_JS: &str = r#"// SAIL interface hot reload (SAIL_WEB_DEV). Not present in normal runs.
(() => {
  const uses = (url) => url === location.pathname
    || !!document.querySelector(`script[src="${url}"], link[href="${url}"], link[href^="${url}?"]`);
  let boot = null;
  const events = new EventSource('/__dev/events');
  events.addEventListener('hello', (e) => {
    const id = JSON.parse(e.data).boot;
    if (boot && boot !== id) location.reload(); // the server restarted, perhaps with new code
    boot = id;
  });
  events.addEventListener('change', (e) => {
    const {url, css} = JSON.parse(e.data);
    if (!uses(url)) return;
    const sheets = css ? [...document.querySelectorAll('link[rel="stylesheet"]')]
      .filter((l) => new URL(l.href).pathname === url) : [];
    if (!sheets.length) { location.reload(); return; }
    for (const old of sheets) {
      const fresh = old.cloneNode();
      fresh.href = `${url}?v=${Date.now()}`;
      fresh.addEventListener('load', () => old.remove(), {once: true});
      old.after(fresh);
    }
    console.info(`[SAIL dev] reloaded ${url}`);
  });
  console.info('[SAIL dev] interface hot reload is on');
})();
"#;
const RELOAD_TAG: &str = "<script src=\"/__dev/reload.js\"></script>";

/// Where `SAIL_WEB_DEV` points: the `sail-dialogue` crate directory to read files from.
pub fn dev_dir(setting: Option<&str>) -> Option<PathBuf> {
    let value = setting?.trim();
    match value {
        "" | "0" | "false" | "off" => None,
        "1" | "true" | "on" => Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))),
        root => Some(Path::new(root).join("crates/sail-dialogue")),
    }
}

/// Add the reload client to a page.
pub fn with_reload_client(html: &str) -> String {
    match html.rfind("</body>") {
        Some(at) => format!("{}{RELOAD_TAG}\n{}", &html[..at], &html[at..]),
        None => format!("{html}\n{RELOAD_TAG}\n"),
    }
}

/// The interface routes, reading `SAIL_WEB_DEV` from the environment.
pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    let dev = dev_dir(std::env::var("SAIL_WEB_DEV").ok().as_deref());
    if let Some(dir) = &dev {
        tracing::warn!(
            dir = %dir.display(),
            "Interface hot reload is on (SAIL_WEB_DEV): pages and scripts are read from disk"
        );
    }
    router_with(dev)
}

/// The interface routes; `dev` is the crate directory to serve from, if hot reload is on.
pub fn router_with<S: Clone + Send + Sync + 'static>(dev: Option<PathBuf>) -> Router<S> {
    let mut app = Router::new();
    match dev {
        None => {
            for asset in ASSETS {
                app = app.route(
                    asset.url,
                    get(move || async move {
                        ([(header::CONTENT_TYPE, asset.content_type)], asset.embedded)
                    }),
                );
            }
        }
        Some(dir) => {
            let (changes, _) = broadcast::channel::<&'static Asset>(64);
            tokio::spawn(watch(dir.clone(), changes.clone()));
            let boot = uuid::Uuid::new_v4().to_string();
            for asset in ASSETS {
                let dir = dir.clone();
                app = app.route(
                    asset.url,
                    get(move || async move { serve_from_disk(&dir, asset).await }),
                );
            }
            app = app
                .route(
                    "/__dev/reload.js",
                    get(|| async { ([(header::CONTENT_TYPE, JS)], RELOAD_JS) }),
                )
                .route(
                    "/__dev/events",
                    get(move || {
                        let updates = changes.subscribe();
                        let boot = boot.clone();
                        async move { events(boot, updates) }
                    }),
                );
        }
    }
    app
}

async fn serve_from_disk(dir: &Path, asset: &'static Asset) -> Response {
    let body = match tokio::fs::read_to_string(dir.join(asset.file)).await {
        Ok(text) => text,
        Err(error) => {
            tracing::warn!(file = asset.file, %error, "Hot reload could not read the file; serving the built-in copy");
            asset.embedded.to_string()
        }
    };
    let body = if asset.is_html() {
        with_reload_client(&body)
    } else {
        body
    };
    let mut response = ([(header::CONTENT_TYPE, asset.content_type)], body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn events(
    boot: String,
    updates: broadcast::Receiver<&'static Asset>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let hello = Event::default()
        .event("hello")
        .data(json!({"boot": boot}).to_string());
    let stream = stream::unfold(
        (Some(hello), updates),
        |(pending, mut updates)| async move {
            if let Some(hello) = pending {
                return Some((Ok(hello), (None, updates)));
            }
            loop {
                match updates.recv().await {
                    Ok(asset) => {
                        let data = json!({"url": asset.url, "css": asset.is_css()}).to_string();
                        return Some((
                            Ok(Event::default().event("change").data(data)),
                            (None, updates),
                        ));
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Poll the files' modification times (a few dozen files, so a quarter-second poll is cheap
/// and needs no platform file-watching dependency) and announce each change.
async fn watch(dir: PathBuf, changes: broadcast::Sender<&'static Asset>) {
    let mut seen: Vec<Option<SystemTime>> =
        ASSETS.iter().map(|a| modified(&dir.join(a.file))).collect();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tick.tick().await;
        for (asset, last) in ASSETS.iter().zip(seen.iter_mut()) {
            let now = modified(&dir.join(asset.file));
            if now != *last {
                *last = now;
                if now.is_some() {
                    tracing::info!(
                        file = asset.file,
                        "Interface file changed; notifying open pages"
                    );
                    let _ = changes.send(asset);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dev_mode_serves_edits_and_announces_them() {
        use futures_util::StreamExt;
        let dir = std::env::temp_dir().join(format!("sail-ui-dev-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("web")).unwrap();
        std::fs::write(dir.join("web/app.css"), "body{color:red}").unwrap();
        let app: Router = router_with(Some(dir.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::new();

        // Edited file from disk, never cached; a file missing on disk falls back to the build.
        let css = http.get(format!("{base}/app.css")).send().await.unwrap();
        assert_eq!(css.headers()["cache-control"], "no-store");
        assert_eq!(css.text().await.unwrap(), "body{color:red}");
        let page = http
            .get(format!("{base}/"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(page.contains(RELOAD_TAG), "pages get the reload client");
        let tracker = http
            .get(format!("{base}/tracker.js"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(
            tracker,
            ASSETS
                .iter()
                .find(|a| a.url == "/tracker.js")
                .unwrap()
                .embedded
        );
        assert!(
            http.get(format!("{base}/__dev/reload.js"))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        // Only listed files are served.
        assert_eq!(
            http.get(format!("{base}/web/app.css"))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );

        let mut events = http
            .get(format!("{base}/__dev/events"))
            .send()
            .await
            .unwrap()
            .bytes_stream();
        let mut seen = String::new();
        while !seen.contains("event: hello") {
            seen.push_str(&String::from_utf8_lossy(
                &events.next().await.unwrap().unwrap(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(dir.join("web/app.css"), "body{color:blue}").unwrap();
        let file = std::fs::File::options()
            .write(true)
            .open(dir.join("web/app.css"))
            .unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        let change = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let chunk =
                    String::from_utf8_lossy(&events.next().await.unwrap().unwrap()).to_string();
                if chunk.contains("event: change") {
                    return chunk;
                }
            }
        })
        .await
        .expect("a change event after editing the file");
        assert!(
            change.contains(r#""url":"/app.css""#) && change.contains(r#""css":true"#),
            "{change}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn normal_runs_serve_the_built_files_without_dev_routes() {
        let app: Router = router_with(None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let page = reqwest::get(format!("{base}/"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(page, ASSETS[0].embedded);
        assert!(!page.contains(RELOAD_TAG));
        assert_eq!(
            reqwest::get(format!("{base}/__dev/events"))
                .await
                .unwrap()
                .status(),
            404
        );
    }

    #[test]
    fn dev_setting_selects_the_built_tree_or_a_checkout() {
        assert_eq!(dev_dir(None), None);
        assert_eq!(dev_dir(Some("0")), None);
        assert_eq!(dev_dir(Some("")), None);
        assert_eq!(
            dev_dir(Some("1")),
            Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        );
        assert_eq!(
            dev_dir(Some("/home/main/SAIL/.worktrees/x")),
            Some(PathBuf::from(
                "/home/main/SAIL/.worktrees/x/crates/sail-dialogue"
            ))
        );
    }

    #[test]
    fn the_reload_client_goes_before_the_end_of_the_body() {
        assert_eq!(
            with_reload_client("<body><p>x</p></body></html>"),
            format!("<body><p>x</p>{RELOAD_TAG}\n</body></html>")
        );
        assert!(with_reload_client("<p>x</p>").ends_with(&format!("{RELOAD_TAG}\n")));
    }

    #[test]
    fn urls_are_unique_and_types_match_files() {
        let mut urls: Vec<_> = ASSETS.iter().map(|a| a.url).collect();
        urls.sort();
        urls.dedup();
        assert_eq!(urls.len(), ASSETS.len());
        for asset in ASSETS {
            let ext = asset.file.rsplit('.').next().unwrap();
            let expected = match ext {
                "html" => HTML,
                "js" => JS,
                "css" => CSS,
                "svg" => "image/svg+xml; charset=utf-8",
                other => panic!("unexpected asset type {other}"),
            };
            assert_eq!(asset.content_type, expected, "{}", asset.file);
            assert!(!asset.embedded.is_empty(), "{}", asset.file);
        }
    }
}
