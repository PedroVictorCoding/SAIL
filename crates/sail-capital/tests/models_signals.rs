//! Round 12: model signals through Python helpers (fake helpers; no model, no network).
use async_trait::async_trait;
use chrono::{Duration, NaiveDate, Utc};
use rust_decimal::Decimal;
use sail_capital::market::*;
use sail_capital::models::{self, HelperItem, ModelError, origins, parse_output, specs};
use sail_capital::service::CapitalService;
use sail_capital::signals::SignalId;
use std::sync::Arc;

const FILE: &[u8] = include_bytes!("fixtures/xtb/xtb_2026_layout.xlsx");

/// 400 weekday sessions from 2024-06-03, with OHL.
struct LongPrices;

#[async_trait]
impl PriceSource for LongPrices {
    fn source_id(&self) -> &str {
        "fake"
    }

    async fn daily(&self, symbol: &str) -> Result<PriceSeries, PriceSourceError> {
        let mut day = NaiveDate::from_ymd_opt(2024, 6, 3).unwrap();
        let mut bars = Vec::new();
        let mut i = 0i64;
        while bars.len() < 400 {
            use chrono::Datelike;
            if day.weekday().number_from_monday() <= 5 {
                let close = Decimal::new(10_000 + (i % 37) * 13, 2);
                bars.push(PriceBar {
                    session: day,
                    open: Some(close),
                    high: Some(close + Decimal::ONE),
                    low: Some(close - Decimal::ONE),
                    close,
                    volume: None,
                });
                i += 1;
            }
            day += Duration::days(1);
        }
        Ok(PriceSeries {
            source_id: "fake".into(),
            symbol: symbol.into(),
            source_symbol: symbol.to_lowercase(),
            currency: None,
            adjustment: Adjustment::SplitAdjusted,
            bars,
            payload_sha256: "x".into(),
        })
    }
}

fn sessions(n: usize) -> Vec<NaiveDate> {
    let start = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    (0..n).map(|i| start + Duration::days(i as i64)).collect()
}

#[test]
fn origins_step_back_from_the_latest_after_enough_history() {
    let s = sessions(270);
    let o = origins(&s, NaiveDate::MIN);
    assert_eq!(o.last(), Some(&s[269]));
    assert_eq!(o.first(), Some(&s[254]));
    assert!(o.windows(2).all(|w| (w[1] - w[0]).num_days() == 5));
    // Nothing before `from`, and nothing without a year of history.
    assert_eq!(origins(&s, s[265]), vec![s[269]]);
    assert!(origins(&sessions(252), NaiveDate::MIN).is_empty());
}

#[test]
fn helper_output_is_checked() {
    let ok = parse_output(
        "chronos",
        br#"{"model_version":"m@1","values":[["sym:A","2025-01-02",0.1],["sym:B","2025-01-02",1e999]],"skipped":2}"#,
    );
    // 1e999 is not a finite f64 in JSON; serde rejects it outright.
    assert!(ok.is_err() || ok.as_ref().unwrap().values.len() == 1);
    let ok = parse_output(
        "chronos",
        br#"{"model_version":"m@1","values":[["sym:A","2025-01-02",0.1]],"skipped":2,"notes":["n"]}"#,
    )
    .unwrap();
    assert_eq!(ok.values.len(), 1);
    assert_eq!(ok.skipped, 2);
    assert_eq!(
        parse_output(
            "yolo",
            br#"{"error":"the YOLO weights import code SAIL does not allow: os.system"}"#
        ),
        Err(ModelError::Failed(
            "yolo".into(),
            "the YOLO weights import code SAIL does not allow: os.system".into()
        ))
    );
    assert!(matches!(
        parse_output("yolo", b""),
        Err(ModelError::NotInstalled(_))
    ));
    assert!(matches!(
        parse_output("yolo", b"Traceback"),
        Err(ModelError::BadOutput(..))
    ));
}

#[test]
fn the_known_models_are_dated_and_licensed() {
    let all = specs();
    let chronos = all.iter().find(|s| s.name == "chronos").unwrap();
    assert_eq!(chronos.signal, SignalId::FoundationForecast);
    assert_eq!(chronos.license, "Apache-2.0");
    let yolo = all.iter().find(|s| s.name == "yolo").unwrap();
    assert_eq!(yolo.signal, SignalId::ChartPattern);
    assert!(yolo.needs_ohl);
    assert!(yolo.license.contains("not stated"));
    assert!(yolo.evaluate_from >= NaiveDate::from_ymd_opt(2025, 4, 2).unwrap());
}

fn fake_helpers(dir: &std::path::Path) {
    // Echoes a value per origin (the number of bars up to it), and reports whether a
    // SAIL secret leaked into its environment and whether telemetry is off.
    let body = r#"import json, os, sys
req = json.load(sys.stdin)
values = []
for item in req["items"]:
    for origin in item["origins"]:
        used = [b for b in item["bars"] if b[0] <= origin]
        values.append([item["key"], origin, float(len(used))])
json.dump({"model_version": "fake@1", "values": values, "skipped": 0,
           "notes": ["leak" if os.environ.get("SAIL_TEST_SECRET") else "clean",
                     os.environ.get("HF_HUB_DISABLE_TELEMETRY", "")]}, sys.stdout)
"#;
    std::fs::write(dir.join("chronos_signal.py"), body).unwrap();
    std::fs::write(
        dir.join("yolo_signal.py"),
        "import sys; sys.stdout.write('{\"error\": \"boom\"}')",
    )
    .unwrap();
}

#[tokio::test]
async fn a_refresh_stores_new_values_once_and_feeds_the_signal_report() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = tempfile::tempdir().unwrap();
    fake_helpers(scripts.path());
    // SAFETY: only this test in this binary reads these variables.
    unsafe {
        std::env::set_var("SAIL_SIGNALS_PYTHON", "python3");
        std::env::set_var("SAIL_SIGNALS_SCRIPTS", scripts.path());
        std::env::set_var("SAIL_SIGNALS_CACHE", dir.path().join("cache"));
        std::env::set_var("SAIL_TEST_SECRET", "x");
    }
    let mut chronos = specs().into_iter().find(|s| s.name == "chronos").unwrap();
    chronos.evaluate_from = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    let yolo = specs().into_iter().find(|s| s.name == "yolo").unwrap();
    let svc = CapitalService::open(dir.path())
        .unwrap()
        .with_prices(Arc::new(LongPrices), true)
        .with_models(vec![chronos.clone(), yolo]);
    let batch = svc
        .import_xtb(None, "export.xlsx", FILE, None, None, Utc::now())
        .unwrap();
    let account = batch.account;
    svc.refresh_prices(Some(&account)).await.unwrap();

    let status = svc.refresh_model_signals(&account).await.unwrap();
    let c = status
        .models
        .iter()
        .find(|m| m.spec.name == "chronos")
        .unwrap();
    let run = c.last_run.as_ref().unwrap();
    assert!(c.enabled);
    assert_eq!(run.error, None);
    assert!(run.added > 0);
    assert_eq!(run.added, run.requested);
    assert_eq!(run.model_version.as_deref(), Some("fake@1"));
    assert_eq!(run.notes, vec!["clean".to_string(), "1".to_string()]);
    let stored = c.stored.as_ref().unwrap();
    assert_eq!(stored.values, run.added);

    // The failing helper is reported, not fatal.
    let y = status
        .models
        .iter()
        .find(|m| m.spec.name == "yolo")
        .unwrap();
    assert_eq!(
        y.last_run.as_ref().unwrap().error.as_deref(),
        Some("the yolo helper failed: boom")
    );

    // A second refresh has nothing new to compute.
    let again = svc.refresh_model_signals(&account).await.unwrap();
    let c2 = again
        .models
        .iter()
        .find(|m| m.spec.name == "chronos")
        .unwrap();
    assert_eq!(c2.last_run.as_ref().unwrap().requested, 0);
    assert_eq!(c2.stored.as_ref().unwrap().values, stored.values);

    // The stored values flow into the signal report without error (its scoring is
    // tested with Kilo's slice).
    svc.signals(&account).unwrap();
}

#[tokio::test]
async fn a_missing_interpreter_is_a_plain_error() {
    let spec = specs().into_iter().next().unwrap();
    let item = HelperItem {
        key: "sym:A".into(),
        origins: vec![NaiveDate::from_ymd_opt(2025, 1, 2).unwrap()],
        bars: vec![],
    };
    let err = models::run_helper(&spec, "/nonexistent/python", "/nonexistent", None, &[item])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("scripts/signals/setup.sh"));
}
