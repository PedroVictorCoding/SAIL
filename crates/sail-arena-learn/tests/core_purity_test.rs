//! The core module must stay pure (#232): no IO, clock, async runtime or storage crates.
#[test]
fn core_has_no_io_dependencies() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/core");
    let forbidden = [
        "rusqlite",
        "tokio::",
        "reqwest",
        "std::fs",
        "std::net",
        "std::process",
        "SystemTime",
        "Instant::now",
        "sail_mind",
        "std::env",
    ];
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let source = std::fs::read_to_string(&path).unwrap();
        for word in forbidden {
            // `tokio_util::sync::CancellationToken` in the Model port is a plain type, allowed.
            assert!(
                !source.contains(word),
                "{} uses `{word}`; core must not perform IO",
                path.display()
            );
        }
    }
}
