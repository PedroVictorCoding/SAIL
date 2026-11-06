//! Live check against the real public sources. Needs network, so it is ignored by default:
//! `cargo test -p sail-tracker --test live -- --ignored --nocapture`
use sail_tracker::{Source, Tracker, store::Store};

#[tokio::test]
#[ignore = "needs network access to public sources"]
async fn every_source_answers() {
    let tracker = Tracker::with_store(Store::open_in_memory().unwrap()).unwrap();
    for source in Source::ALL {
        tracker.refresh(source).await;
    }
    let board = tracker.board();
    for (name, status) in board["sources"].as_object().unwrap() {
        println!("{name:8} ok={} error={}", status["ok"], status["error"]);
    }
    for key in [
        "news",
        "quotes",
        "fx",
        "odds",
        "conditions",
        "air",
        "routes",
        "quakes",
        "anomalies",
    ] {
        println!(
            "{key:10} {} items",
            board[key].as_array().map_or(0, Vec::len)
        );
    }
    println!("first story: {}", board["news"][0]);
    println!(
        "first quote: {} {} {}",
        board["quotes"][0]["symbol"], board["quotes"][0]["price"], board["quotes"][0]["move_z"]
    );
    println!("weather: {}", board["conditions"][0]["summary"]);
    println!("anomalies: {}", board["anomalies"]);
    let failed: Vec<_> = board["sources"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, s)| s["ok"] != true)
        .map(|(n, _)| n.clone())
        .collect();
    assert!(failed.is_empty(), "sources failed: {failed:?}");
}
