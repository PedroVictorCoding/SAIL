//! Integration tests against a fake Moodle bound to `127.0.0.1:0`.
//!
//! The client is allowed to use plain HTTP only for loopback addresses, so these tests
//! exercise the real request shape (form body, no redirects, pluginfile rewrite) without a
//! certificate or a real site.

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bytes::Bytes;
use sail_moodle::client::RestClient;
use sail_moodle::contracts::{FileRef, MoodleApi, MoodleError, MoodleToken, SiteConfig};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

#[derive(Default)]
struct Record {
    query: String,
    body: String,
}

type Shared = Arc<Mutex<Record>>;

fn client(base: &str) -> RestClient {
    RestClient::new(
        SiteConfig {
            base_url: url::Url::parse(base).expect("test URL"),
        },
        MoodleToken::new(TOKEN),
    )
    .expect("test client")
}

fn file_ref(base: &str, size: u64) -> FileRef {
    FileRef {
        course_id: 7,
        module_id: 101,
        filename: "a.pdf".to_string(),
        filepath: "/".to_string(),
        size,
        modified: None,
        mimetype: Some("application/pdf".to_string()),
        url: format!("{base}/pluginfile.php/1/a.pdf"),
    }
}

fn json_response(body: impl Into<Body>) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body.into()).into_response()
}

async fn serve(app: Router) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("fake Moodle");
    });
    (format!("http://127.0.0.1:{}", address.port()), server)
}

fn function_of(uri: &Uri) -> &str {
    uri.query()
        .unwrap_or_default()
        .split('&')
        .find_map(|pair| pair.strip_prefix("wsfunction="))
        .unwrap_or_default()
}

async fn record_site_info(State(state): State<Shared>, uri: Uri, body: String) -> Response {
    {
        let mut record = state.lock().expect("record");
        record.query = uri.query().unwrap_or_default().to_string();
        record.body = body;
    }
    json_response(
        r#"{"sitename":"Fake Moodle","userid":42,"fullname":"Pessoa Exemplo","release":"4.5","functions":[{"name":"core_webservice_get_site_info"}],"downloadfiles":1}"#,
    )
}

async fn error_handler(uri: Uri) -> Response {
    match function_of(&uri) {
        "core_webservice_get_site_info" => json_response(
            r#"{"exception":"moodle_exception","errorcode":"invalidtoken","message":"Invalid token - token not found"}"#,
        ),
        "core_enrol_get_users_courses" => json_response(
            r#"{"exception":"moodle_exception","errorcode":"servicenotavailable","message":"Web services are not enabled"}"#,
        ),
        "core_course_get_contents" => json_response(
            r#"{"exception":"moodle_exception","errorcode":"unexpected","message":"Something odd"}"#,
        ),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
    }
}

async fn echo_token_error(body: String) -> Response {
    let token = body
        .split('&')
        .find_map(|pair| pair.strip_prefix("wstoken="))
        .unwrap_or_default();
    let body = serde_json::json!({
        "exception": "moodle_exception",
        "errorcode": "invalidtoken",
        "message": format!("the request carried {token}"),
    });
    json_response(body.to_string())
}

async fn big_file() -> Response {
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        Body::from(vec![0u8; 2 * 1024 * 1024]),
    )
        .into_response()
}

async fn redirect_file() -> Response {
    (StatusCode::FOUND, [(header::LOCATION, "/elsewhere")]).into_response()
}

async fn hello_file() -> Response {
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        b"hello".to_vec(),
    )
        .into_response()
}

async fn json_error_file() -> Response {
    json_response(
        r#"{"exception":"moodle_exception","errorcode":"invalidtoken","message":"expired"}"#,
    )
}

async fn slow_file() -> Response {
    let stream = futures_util::stream::unfold(0_u8, |step| async move {
        match step {
            0 => Some((
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"partial-data")),
                1,
            )),
            _ => {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Some((Ok(Bytes::new()), 2))
            }
        }
    });
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

struct TempPath(PathBuf);

impl TempPath {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "sail-moodle-{label}-{}-{nanos}",
            std::process::id()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tokio::test]
async fn token_is_sent_in_the_form_body_not_the_query() {
    let state: Shared = Arc::new(Mutex::new(Record::default()));
    let app = Router::new()
        .route("/webservice/rest/server.php", post(record_site_info))
        .with_state(state.clone());
    let (base, server) = serve(app).await;

    let info = client(&base).site_info().await.expect("site info");
    assert_eq!(info.site_name, "Fake Moodle");
    assert_eq!(info.user_id, 42);

    let record = state.lock().expect("record");
    assert!(
        record
            .query
            .contains("wsfunction=core_webservice_get_site_info")
    );
    assert!(!record.query.contains("wstoken"));
    assert!(!record.query.contains(TOKEN));
    assert!(record.body.contains(&format!("wstoken={TOKEN}")));
    drop(record);
    server.abort();
}

#[tokio::test]
async fn maps_moodle_errors_to_typed_variants() {
    let app = Router::new().route("/webservice/rest/server.php", post(error_handler));
    let (base, server) = serve(app).await;
    let client = client(&base);

    assert!(matches!(
        client.site_info().await,
        Err(MoodleError::Unauthorized(_))
    ));
    assert!(matches!(
        client.courses(42).await,
        Err(MoodleError::FunctionNotAllowed { .. })
    ));
    assert!(matches!(
        client.course_contents(7).await,
        Err(MoodleError::Remote { .. })
    ));
    assert!(matches!(
        client.assignments(&[7]).await,
        Err(MoodleError::Unavailable(_))
    ));
    server.abort();
}

#[tokio::test]
async fn refuses_a_download_larger_than_the_snapshot_entry() {
    let app = Router::new().route("/webservice/pluginfile.php/*path", get(big_file));
    let (base, server) = serve(app).await;
    let dest = TempPath::new("big");

    let result = client(&base)
        .download(&file_ref(&base, 10), dest.path(), CancellationToken::new())
        .await;
    assert!(matches!(result, Err(MoodleError::FileRefused(_))));
    assert!(!dest.path().exists());
    server.abort();
}

#[tokio::test]
async fn refuses_a_redirected_download() {
    let app = Router::new().route("/webservice/pluginfile.php/*path", get(redirect_file));
    let (base, server) = serve(app).await;
    let dest = TempPath::new("redirect");

    let result = client(&base)
        .download(&file_ref(&base, 100), dest.path(), CancellationToken::new())
        .await;
    assert!(matches!(result, Err(MoodleError::FileRefused(_))));
    assert!(!dest.path().exists());
    server.abort();
}

#[tokio::test]
async fn refuses_a_file_url_on_another_host() {
    let client = client("http://127.0.0.1:9");
    let dest = TempPath::new("cross-host");
    let mut file = file_ref("https://evil.test", 100);
    file.url = "https://evil.test/pluginfile.php/1/a.pdf".to_string();

    let result = client
        .download(&file, dest.path(), CancellationToken::new())
        .await;
    assert!(matches!(result, Err(MoodleError::FileRefused(_))));
    assert!(!dest.path().exists());
}

#[tokio::test]
async fn cancel_removes_the_partial_file() {
    let app = Router::new().route("/webservice/pluginfile.php/*path", get(slow_file));
    let (base, server) = serve(app).await;
    let dest = TempPath::new("cancel");
    let file = file_ref(&base, 1000);
    let path = dest.path().to_path_buf();
    let cancel = CancellationToken::new();
    let child = cancel.clone();
    let task = tokio::spawn(async move { client(&base).download(&file, &path, child).await });

    for _ in 0..100 {
        if tokio::fs::metadata(dest.path()).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        tokio::fs::metadata(dest.path()).await.is_ok(),
        "the partial file should exist before cancel"
    );

    cancel.cancel();
    let result = task.await.expect("download task");
    assert!(matches!(result, Err(MoodleError::Cancelled)));
    assert!(!dest.path().exists(), "cancel must delete the partial file");
    server.abort();
}

#[tokio::test]
async fn token_never_appears_in_an_error() {
    let app = Router::new().route("/webservice/rest/server.php", post(echo_token_error));
    let (base, server) = serve(app).await;

    let error = client(&base).site_info().await.expect_err("an error");
    assert!(!format!("{error}").contains(TOKEN));
    assert!(!format!("{error:?}").contains(TOKEN));
    server.abort();
}

#[tokio::test]
async fn a_json_error_from_pluginfile_is_mapped() {
    let app = Router::new().route("/webservice/pluginfile.php/*path", get(json_error_file));
    let (base, server) = serve(app).await;
    let dest = TempPath::new("json-error");

    let result = client(&base)
        .download(&file_ref(&base, 100), dest.path(), CancellationToken::new())
        .await;
    assert!(matches!(result, Err(MoodleError::Unauthorized(_))));
    assert!(!dest.path().exists());
    server.abort();
}

#[tokio::test]
async fn downloads_and_hashes_a_file() {
    let app = Router::new().route("/webservice/pluginfile.php/*path", get(hello_file));
    let (base, server) = serve(app).await;
    let dest = TempPath::new("hello");

    let downloaded = client(&base)
        .download(&file_ref(&base, 5), dest.path(), CancellationToken::new())
        .await
        .expect("download");
    assert_eq!(downloaded.bytes, 5);
    assert_eq!(
        downloaded.sha256,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_eq!(
        tokio::fs::read(dest.path()).await.expect("read back"),
        b"hello"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest.path())
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    server.abort();
}
