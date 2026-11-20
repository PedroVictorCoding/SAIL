//! K0 contract test: every Calendar client fixture deserializes into the Rust types.

use std::path::{Path, PathBuf};

use sail_calendar::contracts::{
    CalendarChanged, Event, EventsWindow, ImportPreview, ImportResult, Preferences, Source,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct SourcesBody {
    sources: Vec<Source>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorPayload,
}

#[derive(Deserialize)]
struct ErrorPayload {
    code: String,
    message: String,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/ui/fixtures/calendar")
}

#[test]
fn every_calendar_client_fixture_matches_the_rust_contracts() {
    let dir = fixture_dir();
    if !dir.is_dir() {
        // G0's fixtures are not logged yet; the contract test runs once they exist.
        eprintln!(
            "skipping Calendar fixtures: {} does not exist yet",
            dir.display()
        );
        return;
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("fixture directory")
        .map(|entry| entry.expect("fixture entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no Calendar fixtures in {}",
        dir.display()
    );

    for path in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("fixture")
            .to_string();
        let text = std::fs::read_to_string(&path).expect("fixture readable");
        match name.as_str() {
            "sources_first_run.json" | "sources_mixed.json" => {
                let body: SourcesBody =
                    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(!body.sources.is_empty(), "{name}: no sources");
                for source in &body.sources {
                    assert!(
                        source.id.starts_with("src_") && source.id.len() == 16,
                        "{name}: bad source id"
                    );
                    assert!(source.refresh_minutes >= 15 && source.refresh_minutes <= 1_440);
                }
            }
            "week_mixed.json"
            | "week_empty.json"
            | "month_dense.json"
            | "window_truncated.json" => {
                let window: EventsWindow =
                    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
                for occurrence in &window.occurrences {
                    assert!(!occurrence.key.is_empty(), "{name}: empty key");
                    assert!(
                        occurrence.event_id.starts_with("fe_")
                            || occurrence.event_id.starts_with("loc_")
                    );
                }
            }
            "event_local.json" | "event_feed_readonly.json" | "event_recurring.json" => {
                let event: Event =
                    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(!event.title.is_empty(), "{name}: empty title");
                assert!(event.id.starts_with("fe_") || event.id.starts_with("loc_"));
            }
            "import_preview.json" => {
                let preview: ImportPreview =
                    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(preview.preview_id.starts_with("imp_"));
            }
            "import_result.json" => {
                let _: ImportResult =
                    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            }
            "preferences.json" => {
                let prefs: Preferences =
                    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(prefs.day_start_hour < prefs.day_end_hour);
                assert!(!prefs.time_zone.is_empty());
            }
            "error_blocked_address.json" | "error_read_only.json" => {
                let body: ErrorBody =
                    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(!body.error.code.is_empty());
                assert!(!body.error.message.is_empty());
            }
            "ws_calendar_changed.json" => {
                let event: CalendarChanged =
                    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(event.revision > 0);
            }
            other => panic!("unknown Calendar fixture {other}"),
        }
    }
}
