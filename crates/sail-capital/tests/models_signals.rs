//! Round 12: model signals through Python helpers (fake helpers; no model, no network).
use async_trait::async_trait;
use chrono::{Duration, NaiveDate, Utc};
use rust_decimal::Decimal;
use sail_capital::market::*;
use sail_capital::models::{
    self, HelperItem, ModelError, PathOutput, group_paths, origins, outlook_origins, outlook_specs,
    parse_output, parse_path_output, specs,
};
use sail_capital::outlook::ComponentId;
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
    // SAFETY: only this test in this binary reads this variable.
    unsafe {
        std::env::set_var("SAIL_TEST_SECRET", "x");
    }
    let mut chronos = specs().into_iter().find(|s| s.name == "chronos").unwrap();
    chronos.evaluate_from = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    let yolo = specs().into_iter().find(|s| s.name == "yolo").unwrap();
    let svc = CapitalService::open(dir.path())
        .unwrap()
        .with_prices(Arc::new(LongPrices), true)
        .with_models(vec![chronos.clone(), yolo])
        .with_helper_paths(
            "python3",
            scripts.path().to_str().unwrap(),
            dir.path().join("cache").to_str().unwrap(),
        );
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

#[test]
fn path_rows_group_into_sorted_non_crossing_paths() {
    let out = parse_path_output(
        "gbdt",
        br#"{"model_version":"v","paths":[
            ["sym:A","2025-03-03",21,10,[0.0,-0.1,0.02,0.05,0.1]],
            ["sym:A","2025-03-03",21,5,[-0.05,-0.02,0.0,0.02,0.05]],
            ["sym:A","2025-03-03",21,30,[-0.05,-0.02,0.0,0.02,0.05]],
            ["sym:A","2025-03-03",63,63,[-0.2,-0.1,0.0,0.1,0.2]]],"skipped":1}"#,
    )
    .unwrap();
    // The step past the horizon is dropped.
    assert_eq!(out.paths.len(), 3);
    let paths = group_paths(ComponentId::Gbdt, &out);
    assert_eq!(paths.len(), 2);
    let p21 = paths.iter().find(|p| p.horizon == 21).unwrap();
    assert_eq!(p21.path.steps, vec![5, 10]);
    // Quantiles that crossed are sorted.
    assert_eq!(p21.path.values[1], [-0.1, 0.0, 0.02, 0.05, 0.1]);
    assert!(matches!(
        parse_path_output(
            "chronos",
            br#"{"error":"Chronos-Bolt answers horizons up to 64 sessions only"}"#
        ),
        Err(ModelError::Failed(..))
    ));
    let _ = PathOutput {
        model_version: String::new(),
        paths: vec![],
        skipped: 0,
        notes: vec![],
    };
}

#[test]
fn outlook_origins_stay_within_six_years_and_after_the_model_date() {
    let s = sessions(3000);
    let all = outlook_origins(&s, NaiveDate::MIN);
    let latest = *s.last().unwrap();
    assert_eq!(all.last(), Some(&latest));
    assert!(all.first().unwrap() >= &(latest - Duration::days(models::OUTLOOK_HISTORY_DAYS)));
    let chronos = outlook_specs()
        .into_iter()
        .find(|s| s.name == "chronos")
        .unwrap();
    assert_eq!(chronos.horizons, vec![21, 63]);
    assert!(
        outlook_origins(&s, chronos.evaluate_from)
            .iter()
            .all(|d| *d >= chronos.evaluate_from)
    );
}

#[tokio::test]
async fn outlook_paths_are_stored_once_and_served() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = tempfile::tempdir().unwrap();
    // A fake gbdt helper: one horizon-only row per origin and horizon.
    std::fs::write(
        scripts.path().join("gbdt_outlook.py"),
        r#"import json, sys
req = json.load(sys.stdin)
rows = []
for item in req["items"]:
    for origin in item["origins"]:
        for h in req["horizons"]:
            assert str(h) in req["steps"]
            rows.append([item["key"], origin, h, h, [-0.1, -0.05, 0.0, 0.05, 0.1]])
json.dump({"model_version": "fake-gbdt@1", "paths": rows}, sys.stdout)
"#,
    )
    .unwrap();
    let gbdt = outlook_specs()
        .into_iter()
        .find(|s| s.name == "gbdt")
        .unwrap();
    let svc = CapitalService::open(dir.path())
        .unwrap()
        .with_prices(Arc::new(LongPrices), true)
        .with_outlook_models(vec![gbdt])
        .with_helper_paths(
            "python3",
            scripts.path().to_str().unwrap(),
            dir.path().join("cache").to_str().unwrap(),
        );
    let account = svc
        .import_xtb(None, "export.xlsx", FILE, None, None, Utc::now())
        .unwrap()
        .account;
    svc.refresh_prices(Some(&account)).await.unwrap();
    let status = svc.refresh_model_signals(&account).await.unwrap();
    let g = status
        .outlook
        .iter()
        .find(|m| m.spec.name == "gbdt")
        .unwrap();
    let run = g.last_run.as_ref().unwrap();
    assert_eq!(run.error, None, "{run:?}");
    assert!(run.added > 0);
    let (version, instruments, stored) = g.stored.clone().unwrap();
    assert_eq!(version, "fake-gbdt@1");
    assert!(instruments >= 1);
    assert_eq!(stored, run.added);
    let again = svc.refresh_model_signals(&account).await.unwrap();
    let g2 = again
        .outlook
        .iter()
        .find(|m| m.spec.name == "gbdt")
        .unwrap();
    assert_eq!(g2.last_run.as_ref().unwrap().requested, 0);

    // The route-level checks: a horizon outside the list and a symbol not held.
    assert!(svc.outlook(&account, "AAPL.US", 30, None).is_err());
    assert!(svc.outlook(&account, "NOPE.US", 63, None).is_err());
    let view = svc.outlook(&account, "aapl.us", 63, None).unwrap();
    assert_eq!(view.outlook.horizon, 63);
    assert!(view.history.len() >= 63);
    assert_eq!(view.sessions.len(), 400);
    assert!(svc.track_record(&account, 21).is_ok());
}
