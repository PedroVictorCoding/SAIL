//! Real local text/vision smoke test. No speech runtime or cloud account is needed.
use base64::Engine;
use sail_mind::{CancellationToken, MindService, protocol::*};
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let service = MindService::from_env()?;
    service.start().await?;
    let mut request =
        InferenceRequest::text("Reply with one short sentence confirming local inference works.");
    request.generation.max_tokens = 64;
    if let Ok(path) = std::env::var("SAIL_MIND_IMAGE") {
        let data = std::fs::read(&path)?;
        let mime = if path.ends_with(".png") {
            "image/png"
        } else {
            "image/jpeg"
        };
        request.messages[0].content = Content::Parts(vec![
            ContentPart::Text {
                text: "Describe the dominant color in one short sentence.".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: format!(
                        "data:{mime};base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(data)
                    ),
                },
            },
        ]);
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let worker = tokio::spawn({
        let service = service.clone();
        async move {
            service
                .infer(request, CancellationToken::new(), Some(tx))
                .await
        }
    });
    while let Some(event) = rx.recv().await {
        if let InferenceEventPayload::TextDelta { text } = event.payload {
            print!("{text}");
            use std::io::Write;
            std::io::stdout().flush()?;
        }
    }
    let response = worker.await??;
    println!("\n{}", serde_json::to_string_pretty(&response)?);
    let status = service.status().await;
    assert_eq!(status.metrics.completed, 1);
    assert_eq!(status.metrics.active, 0);
    println!("{}", serde_json::to_string_pretty(&status)?);
    service.shutdown().await?;
    Ok(())
}
