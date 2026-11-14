//! Local demo of the SAIL Agents workspace (Flow workflows and Hermes skills and plugins)
//! with the real Flow and Hermes APIs, without loading speech or language models.
//!
//! Set SAIL_DATA and HERMES_HOME to temporary directories and SAIL_AGENTS_DEMO_TOKEN to a
//! 32+ character token. Plugins in `$HERMES_HOME/plugins` run in the real sandbox once
//! approved. Forge and the voice socket are unavailable in this demo.
use axum::Router;
use sail_guardian::authority::ApprovalAuthority;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = std::env::var("SAIL_AGENTS_DEMO_TOKEN")?;
    std::env::var_os("SAIL_DATA").ok_or("Set SAIL_DATA to an isolated demo directory")?;
    std::env::var_os("HERMES_HOME").ok_or("Set HERMES_HOME to an isolated demo directory")?;
    if token.len() < 32 {
        return Err("Use a token of at least 32 characters".into());
    }
    let authority = || ApprovalAuthority::new(&token);
    let app = Router::new()
        .merge(sail_dialogue::ui_assets::router())
        .nest_service(
            "/api/flow",
            sail_dialogue::flow_http::router(Some(authority()?)),
        )
        .nest_service(
            "/api/hermes",
            sail_dialogue::hermes_http::router(Some(authority()?)),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18809").await?;
    println!("SAIL Agents demo: http://127.0.0.1:18809/#agents");
    axum::serve(listener, app).await?;
    Ok(())
}
