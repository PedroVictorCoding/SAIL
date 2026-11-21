//! Round 28: the `/api/calendar` contract over real HTTP, against a fake feed.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use reqwest::Url;
use sail_calendar::contracts::CalendarError;
use sail_calendar::fetch::{FeedFetcher, FetchOutcome};
use sail_calendar::service::CalendarService;
use sail_dialogue::calendar_http;
use sail_guardian::authority::ApprovalAuthority;
use serde_json::{Value, json};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";
const SECRET: &str = "private-9f8e7d6c5b4a";

const FEED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
BEGIN:VEVENT\r\nUID:lab@example.test\r\nDTSTAMP:20261120T100000Z\r\n\
DTSTART:20261126T150000Z\r\nDTEND:20261126T170000Z\r\nSUMMARY:Physics lab\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";

struct OneFeed(Mutex<Option<String>>);

#[async_trait]
impl FeedFetcher for OneFeed {
    async fn fetch(
        &self,
        _url: &Url,
        _etag: Option<&str>,
        _last_modified: Option<&str>,
    ) -> Result<FetchOutcome, CalendarError> {
        match self.0.lock().unwrap().take() {
            Some(text) => Ok(FetchOutcome::Body {
                text,
                etag: None,
                last_modified: None,
            }),
            None => Err(CalendarError::FetchFailed(
                "the feed returned HTTP 500".into(),
            )),
        }
    }
}

async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn calendar_contract_over_http() {
    let dir = std::env::temp_dir().join(format!(
        "sail-calendar-http-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let service =
        CalendarService::open(&dir, Arc::new(OneFeed(Mutex::new(Some(FEED.into()))))).unwrap();
    let base = serve(calendar_http::router(
        service,
        Some(ApprovalAuthority::new(TOKEN).unwrap()),
    ))
    .await;
    let client = reqwest::Client::new();

    // Owner token required, with the contract's error shape.
    let res = client.get(format!("{base}/sources")).send().await.unwrap();
    assert_eq!(res.status(), 401);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unauthorized");

    // First run: only SAIL calendar.
    let sources: Value = client
        .get(format!("{base}/sources"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sources["sources"].as_array().unwrap().len(), 1);
    assert_eq!(sources["sources"][0]["id"], "src_local000000");

    // http:// is refused in plain words; nothing saved.
    let res = client
        .post(format!("{base}/sources"))
        .bearer_auth(TOKEN)
        .json(&json!({"name":"Bad","url":"http://calendar.example.test/x.ics"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid");

    // Add a feed; the URL never comes back.
    let res = client
        .post(format!("{base}/sources"))
        .bearer_auth(TOKEN)
        .json(&json!({"name":"Personal","url":format!("https://calendar.google.com/calendar/ical/me/{SECRET}/basic.ics")}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 201);
    let feed: Value = res.json().await.unwrap();
    assert_eq!(feed["url_hint"], "https://calendar.google.com/…");
    assert_eq!(feed["event_count"], 1);
    let listed = client
        .get(format!("{base}/sources"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!listed.contains(SECRET));

    // Manual sync right away is rate-limited.
    let res = client
        .post(format!(
            "{base}/sources/{}/sync",
            feed["id"].as_str().unwrap()
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);

    // The window, then a read-only refusal on the feed event.
    let window: Value = client
        .get(format!(
            "{base}/events?start=2026-11-23T00:00:00Z&end=2026-11-30T00:00:00Z"
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let occurrences = window["occurrences"].as_array().unwrap();
    assert_eq!(occurrences.len(), 1);
    assert_eq!(occurrences[0]["time"]["type"], "timed");
    let event_id = occurrences[0]["event_id"].as_str().unwrap().to_string();
    let res = client
        .patch(format!("{base}/events/{event_id}"))
        .bearer_auth(TOKEN)
        .json(&json!({"title":"Renamed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "read_only");
    let res = client
        .patch(format!("{base}/events/{event_id}"))
        .bearer_auth(TOKEN)
        .json(&json!({"prepare":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    // Bad window bounds are a 400, not a panic.
    let res = client
        .get(format!(
            "{base}/events?start=yesterday&end=2026-11-30T00:00:00Z"
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    // Create a local event, then import a raw .ics body.
    let res = client
        .post(format!("{base}/events"))
        .bearer_auth(TOKEN)
        .json(&json!({
            "title":"Dentist",
            "time":{"type":"timed","start":"2026-11-24T15:00:00Z","end":"2026-11-24T16:00:00Z","tz":"Europe/Lisbon"},
            "location":null,"notes":null,"repeat":"none","repeat_until":null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 201);
    let preview: Value = client
        .post(format!("{base}/import/preview"))
        .bearer_auth(TOKEN)
        .header("Content-Type", "text/calendar")
        .body(FEED)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(preview["events"].as_array().unwrap().len(), 1);
    let result: Value = client
        .post(format!("{base}/import/commit"))
        .bearer_auth(TOKEN)
        .json(&json!({"preview_id": preview["preview_id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["imported"], 1);

    // Not a calendar, and over the 5 MiB cap.
    let res = client
        .post(format!("{base}/import/preview"))
        .bearer_auth(TOKEN)
        .body("hello")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 422);
    let res = client
        .post(format!("{base}/import/preview"))
        .bearer_auth(TOKEN)
        .body(vec![b'A'; 6 * 1024 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 413);

    // Export is an .ics attachment with both local events.
    let res = client
        .get(format!("{base}/export.ics?sources=src_local000000"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(
        res.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/calendar")
    );
    let text = res.text().await.unwrap();
    assert!(text.contains("SUMMARY:Dentist") && text.contains("SUMMARY:Physics lab"));

    let busy: Value = client
        .get(format!(
            "{base}/freebusy?start=2026-11-24T00:00:00Z&end=2026-11-25T00:00:00Z"
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(busy["busy"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
