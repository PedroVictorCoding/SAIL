//! Local demo of the real Flow API and editor without loading speech or language models.
//! Set SAIL_DATA to a temporary directory and SAIL_FLOW_DEMO_TOKEN to a 32+ character
//! token; enter the same token in the interface's Guardian settings.
use axum::Router;
use sail_guardian::authority::ApprovalAuthority;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = std::env::var("SAIL_FLOW_DEMO_TOKEN")?;
    std::env::var_os("SAIL_DATA").ok_or("Set SAIL_DATA to an isolated demo directory")?;
    if token.len() < 32 {
        return Err("Use a token of at least 32 characters".into());
    }
    let app = Router::new()
        .merge(sail_dialogue::ui_assets::router())
        .nest_service(
            "/api/flow",
            sail_dialogue::flow_http::router(Some(ApprovalAuthority::new(&token)?)),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18808").await?;
    println!("Flow demo: http://127.0.0.1:18808/#flow (other SAIL services are unavailable)");
    axum::serve(listener, app).await?;
    Ok(())
}
