//! The yfinance price source: symbol mapping, the helper's JSON, and the child process
//! (with a fake helper, never the network).
use sail_capital::market::{
    Adjustment, MAX_PRICE_RESPONSE_BYTES, PriceSource, PriceSourceError, YahooSource,
    parse_yfinance_json, yahoo_symbol,
};
use std::io::Write;

#[test]
fn xtb_symbols_map_to_yahoo() {
    assert_eq!(yahoo_symbol("AAPL.US").as_deref(), Some("AAPL"));
    assert_eq!(yahoo_symbol("BRK.B.US").as_deref(), Some("BRK-B"));
    assert_eq!(yahoo_symbol("GALP.PT").as_deref(), Some("GALP.LS"));
    assert_eq!(yahoo_symbol("shell.nl").as_deref(), Some("SHELL.AS"));
    assert_eq!(yahoo_symbol("SAP.DE").as_deref(), Some("SAP.DE"));
    assert_eq!(yahoo_symbol("VUSA.UK").as_deref(), Some("VUSA.L"));
    assert_eq!(yahoo_symbol("MC.FR").as_deref(), Some("MC.PA"));
    assert_eq!(yahoo_symbol("US500"), None);
    assert_eq!(yahoo_symbol("GOLD"), None);
    assert_eq!(yahoo_symbol("X.ZZ"), None);
    assert_eq!(yahoo_symbol("A B.US"), None);
}

#[test]
fn helper_json_becomes_a_sorted_series() {
    let body = br#"{"symbol":"VUSA.L","currency":"GBX","timezone":"Europe/London","adjustment":"split_adjusted","bars":[
        ["2026-11-10","100.5","101","99.5","100.25","1200"],
        ["2026-11-11","54.123","54.5","53.125","1234.567","1200"],
        ["2026-11-09",null,null,null,"99.75",null],
        ["2026-11-09",null,null,null,"99.8",null],
        ["bad-date",null,null,null,"1",null],
        ["2026-11-06",null,null,null,"0",null]]}"#;
    let series = parse_yfinance_json(body, "VUSA.UK", "VUSA.L").unwrap();
    assert_eq!(series.source_id, "yfinance");
    assert_eq!(series.symbol, "VUSA.UK");
    assert_eq!(series.source_symbol, "VUSA.L");
    assert_eq!(series.currency.as_ref().map(|c| c.as_str()), Some("GBX"));
    assert_eq!(series.adjustment, Adjustment::SplitAdjusted);
    let sessions: Vec<String> = series.bars.iter().map(|b| b.session.to_string()).collect();
    assert_eq!(sessions, ["2026-11-09", "2026-11-10", "2026-11-11"]);
    // Three decimals are a decimal fraction here, never thousands.
    assert_eq!(series.bars[2].close.to_string(), "1234.567");
    assert_eq!(
        series.bars[2].open.map(|d| d.to_string()).as_deref(),
        Some("54.123")
    );
    assert_eq!(series.bars[0].close.to_string(), "99.8");
    assert_eq!(
        series.bars[1].open.map(|d| d.to_string()).as_deref(),
        Some("100.5")
    );
    assert_eq!(series.payload_sha256.len(), 64);
}

#[test]
fn helper_errors_map_to_price_errors() {
    let parse = |b: &[u8]| parse_yfinance_json(b, "ZZZ.US", "ZZZ").unwrap_err();
    assert_eq!(
        parse(br#"{"error":"no_data","detail":"x"}"#),
        PriceSourceError::UnknownSymbol("ZZZ.US".into())
    );
    assert_eq!(
        parse(br#"{"error":"rate_limited","detail":"x"}"#),
        PriceSourceError::Refused(429)
    );
    assert!(matches!(
        parse(br#"{"error":"failed","detail":"ValueError"}"#),
        PriceSourceError::Network(_)
    ));
    assert!(matches!(
        parse(b"Traceback ..."),
        PriceSourceError::Malformed(_)
    ));
    assert_eq!(
        parse(br#"{"symbol":"ZZZ","bars":[]}"#),
        PriceSourceError::UnknownSymbol("ZZZ.US".into())
    );
}

fn fake_helper(dir: &tempfile::TempDir, body: &str) -> String {
    let path = dir.path().join("helper.py");
    let mut file = std::fs::File::create(&path).unwrap();
    write!(file, "{body}").unwrap();
    path.to_string_lossy().into_owned()
}

#[tokio::test]
async fn runs_the_helper_with_the_mapped_symbol() {
    let dir = tempfile::tempdir().unwrap();
    // Echoes its argument.
    let script = fake_helper(
        &dir,
        r#"import json, os, sys
json.dump({"symbol": sys.argv[1], "currency": "EUR", "adjustment": "split_adjusted",
           "bars": [["2026-11-10", None, None, None, "12.5", None]]}, sys.stdout)"#,
    );
    let source = YahooSource::with_command("python3", script);
    let series = source.daily("GALP.PT").await.unwrap();
    assert_eq!(series.source_symbol, "GALP.LS");
    assert_eq!(series.symbol, "GALP.PT");
    assert_eq!(series.bars.len(), 1);
}

#[tokio::test]
async fn the_helper_does_not_see_sail_environment() {
    let dir = tempfile::tempdir().unwrap();
    let script = fake_helper(
        &dir,
        r#"import json, os, sys
if os.environ.get("SAIL_TEST_SECRET2"):
    json.dump({"error": "failed", "detail": "leaked"}, sys.stdout)
else:
    json.dump({"bars": [["2026-11-10", None, None, None, "1", None]]}, sys.stdout)"#,
    );
    // SAFETY: set before the child is spawned; only this test reads it.
    unsafe { std::env::set_var("SAIL_TEST_SECRET2", "x") };
    let source = YahooSource::with_command("python3", script);
    assert!(source.daily("AAPL.US").await.is_ok());
}

#[tokio::test]
async fn unmappable_symbols_never_start_the_helper() {
    let source = YahooSource::with_command("/nonexistent/python", "/nonexistent/helper.py");
    assert_eq!(
        source.daily("US500").await.unwrap_err(),
        PriceSourceError::UnknownSymbol("US500".into())
    );
}

#[tokio::test]
async fn oversized_output_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let script = fake_helper(
        &dir,
        &format!(
            "import sys\nsys.stdout.write('x' * {})",
            MAX_PRICE_RESPONSE_BYTES + 10
        ),
    );
    let source = YahooSource::with_command("python3", script);
    assert_eq!(
        source.daily("AAPL.US").await.unwrap_err(),
        PriceSourceError::TooLarge
    );
}

#[tokio::test]
async fn a_missing_interpreter_is_a_plain_error() {
    let source = YahooSource::with_command("/nonexistent/python", "/nonexistent/helper.py");
    let err = source.daily("AAPL.US").await.unwrap_err();
    assert!(err.to_string().contains("scripts/prices/setup.sh"));
}
