//! Real-model check of the local vision describer. Needs the LFM3-Omni-2B weights
//! (scripts/fetch-vision-model.sh) and a llama.cpp llama-server b11960 or newer.
//! Run with: SAIL_VISION_TEST_IMAGE=<png/jpeg> SAIL_VISION_TEST_EXPECT=<word> \
//!   cargo test -p sail-mind --test vision_describer_test -- --ignored --nocapture
#![cfg(unix)]
use sail_mind::vision::{VisionDescriber, describer_config_from_env};
use std::time::Instant;

#[tokio::test]
#[ignore = "loads the real LFM3-Omni-2B model"]
async fn real_describer_reads_an_image() {
    let image_path = std::env::var("SAIL_VISION_TEST_IMAGE").expect("SAIL_VISION_TEST_IMAGE");
    let image = std::fs::read(&image_path).unwrap();
    let mime = if image_path.ends_with(".png") {
        "image/png"
    } else {
        "image/jpeg"
    };
    let mut config = describer_config_from_env().expect("describer weights installed");
    // A private port and no persistence, so the test never touches a running SAIL.
    config.endpoint = "http://127.0.0.1:18082/v1/chat/completions".into();
    config.managed = true;
    config.persist = false;
    let describer = VisionDescriber::new(config).unwrap();
    for question in [None, Some("What is the heading at the top?")] {
        let started = Instant::now();
        let text = describer
            .describe(&image, mime, "screen", question)
            .await
            .unwrap();
        println!("[{:?} in {:?}] {text}", question, started.elapsed());
        assert!(!text.is_empty());
        assert!(text.chars().count() <= sail_mind::vision::MAX_OBSERVATION_CHARS + 1);
        if let Ok(expected) = std::env::var("SAIL_VISION_TEST_EXPECT") {
            assert!(
                text.to_lowercase().contains(&expected.to_lowercase()),
                "description should mention {expected:?}: {text}"
            );
        }
    }
}
