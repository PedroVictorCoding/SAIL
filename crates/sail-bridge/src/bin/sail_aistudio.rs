//! Use Google AI Studio through SAIL's isolated, signed-in browser profile.
//!
//! ```text
//! sail_aistudio login                 # opens a window; sign in by hand, then close it
//! sail_aistudio probe                 # signed in? do SAIL's selectors match the page?
//! sail_aistudio ask [--headless] <prompt...>   # or `-` to read the prompt from stdin
//! sail_aistudio status                # today's quota and where the profile lives
//! ```
//!
//! Needs `SAIL_CHROMIUM` (the pinned Chromium binary). `SAIL_AISTUDIO_DAILY_LIMIT` sets
//! the daily request quota (default 50).

use std::io::Read;
use std::time::Duration;

use sail_bridge::aistudio::{
    AiStudio, AiStudioError, HOME, Paths, SELECTORS_VERSION, UsageQuota, UsageRecord, append_usage,
    sha256_hex,
};
use sail_bridge::browser::configured_binary;

const DEFAULT_DAILY_LIMIT: u32 = 50;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = Paths::default_location();
    let limit = std::env::var("SAIL_AISTUDIO_DAILY_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_DAILY_LIMIT);
    let quota = UsageQuota::new(paths.records.join("quota.json"), limit);
    let result = match args.first().map(String::as_str) {
        Some("login") => login(&paths).await,
        Some("probe") => probe(&paths).await,
        Some("status") => status(&paths, &quota),
        Some("ask") => {
            let headless = args.iter().any(|a| a == "--headless");
            let words: Vec<&str> = args[1..]
                .iter()
                .map(String::as_str)
                .filter(|a| *a != "--headless")
                .collect();
            ask(&paths, &quota, &words, headless).await
        }
        _ => {
            eprintln!(
                "usage: sail_aistudio login | probe | status | ask [--headless] <prompt...|->\n\
                 profile: {}",
                paths.profile.display()
            );
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("sail_aistudio: {error}");
        std::process::exit(1);
    }
}

async fn login(paths: &Paths) -> Result<(), AiStudioError> {
    let mut studio = AiStudio::open(configured_binary()?, &paths.profile, true).await?;
    eprintln!(
        "A SAIL browser window is open at {HOME}.\n\
         Sign in to the Google account you want SAIL to use (a dedicated account is safest),\n\
         then close the window."
    );
    studio.session().wait_for_exit().await?;
    eprintln!("Profile saved at {}", paths.profile.display());
    Ok(())
}

async fn probe(paths: &Paths) -> Result<(), AiStudioError> {
    let studio = AiStudio::open(configured_binary()?, &paths.profile, false).await?;
    // Give the single-page app a moment to render past its loading shell.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let probe = studio.probe().await;
    let _ = studio.close().await;
    let probe = probe?;
    println!(
        "{}",
        serde_json::to_string_pretty(&probe).unwrap_or_default()
    );
    if !probe.signed_in {
        return Err(AiStudioError::NotSignedIn);
    }
    if probe.prompt_input.is_none() || probe.run_button.is_none() {
        return Err(AiStudioError::Ui("see the probe output above".into()));
    }
    Ok(())
}

fn status(paths: &Paths, quota: &UsageQuota) -> Result<(), AiStudioError> {
    let used = quota.used(&today())?;
    println!(
        "used today: {used}/{}\nprofile: {}\nrecords: {}\nselectors: {SELECTORS_VERSION}",
        quota.limit(),
        paths.profile.display(),
        paths.records.display()
    );
    Ok(())
}

async fn ask(
    paths: &Paths,
    quota: &UsageQuota,
    words: &[&str],
    headless: bool,
) -> Result<(), AiStudioError> {
    let prompt = if words == ["-"] {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| AiStudioError::Records(format!("stdin: {e}")))?;
        text
    } else {
        words.join(" ")
    };
    if prompt.trim().is_empty() {
        return Err(AiStudioError::Ui("empty prompt".into()));
    }
    let binary = configured_binary()?;
    let remaining = quota.consume(&today())?;
    let studio = AiStudio::open(binary, &paths.profile, !headless).await?;
    let reply = studio.ask(&prompt, Duration::from_secs(300)).await;
    let _ = studio.close().await;
    append_usage(
        &paths.records.join("usage.jsonl"),
        &UsageRecord {
            at: chrono::Utc::now().to_rfc3339(),
            prompt_sha256: sha256_hex(&prompt),
            prompt_chars: prompt.chars().count(),
            response_chars: reply.as_ref().ok().map(|r| r.chars().count()),
            outcome: match &reply {
                Ok(_) => "ok".into(),
                Err(e) => e.to_string(),
            },
            selectors: SELECTORS_VERSION.into(),
        },
    )?;
    println!("{}", reply?);
    eprintln!("({remaining} AI Studio requests left today)");
    Ok(())
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}
