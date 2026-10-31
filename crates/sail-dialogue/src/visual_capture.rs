//! Route explicit capture requests before image reasoning. Never infer permission
//! to observe the device from an ordinary image-analysis question.
pub fn requested_source(prompt: &str, previous_user: Option<&str>) -> Option<&'static str> {
    let text = prompt.to_lowercase();
    let capture = [
        "capture",
        "take a picture",
        "take a photo",
        "take a screenshot",
        "tira uma foto",
        "tirar uma foto",
        "captura",
        "screenshot",
    ]
    .iter()
    .any(|word| text.contains(word));
    if !capture {
        return None;
    }
    let screen = |s: &str| {
        [
            "screen", "desktop", "window", "display", "ecrã", "tela", "monitor",
        ]
        .iter()
        .any(|w| s.contains(w))
    };
    let camera = |s: &str| {
        [
            "camera",
            "câmera",
            "câmara",
            "photo",
            "picture",
            "foto",
            "webcam",
            "in front of me",
        ]
        .iter()
        .any(|w| s.contains(w))
    };
    if screen(&text) {
        Some("screen")
    } else if camera(&text) {
        Some("camera")
    } else if text.contains("image")
        || text.contains("imagem")
        || text.contains("capture it")
        || text.contains("capture that")
    {
        let context = previous_user.unwrap_or_default().to_lowercase();
        if screen(&context) {
            Some("screen")
        } else if camera(&context) {
            Some("camera")
        } else {
            Some("choose")
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_capture_without_turning_analysis_into_device_access() {
        assert_eq!(
            requested_source("Capture an image and tell me what you see.", None),
            Some("choose")
        );
        assert_eq!(requested_source("Take a screenshot", None), Some("screen"));
        assert_eq!(requested_source("Take a picture", None), Some("camera"));
        assert_eq!(
            requested_source("Capture an image", Some("Look at my desktop")),
            Some("screen")
        );
        assert_eq!(
            requested_source("Capture it", Some("Use the webcam")),
            Some("camera")
        );
        assert_eq!(requested_source("Analyze this image", None), None);
        assert_eq!(requested_source("Explain packet capture", None), None);
    }
}
