use std::{
    fs::OpenOptions,
    io::{self, Write},
    sync::{Arc, Mutex},
};
use tracing_subscriber::{
    fmt::{MakeWriter, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

#[derive(Clone)]
struct RunnerWriter(Arc<Mutex<std::fs::File>>);
impl Write for RunnerWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        io::stderr().write_all(bytes)?;
        self.0
            .lock()
            .map_err(|_| io::Error::other("runner log lock poisoned"))?
            .write_all(bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()?;
        self.0
            .lock()
            .map_err(|_| io::Error::other("runner log lock poisoned"))?
            .flush()
    }
}
impl<'a> MakeWriter<'a> for RunnerWriter {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}
pub fn init() -> Result<(), Box<dyn std::error::Error>> {
    // Logs go to `logs/` under the SAIL data root; `.local/` in the working directory is the
    // fallback when that cannot be created.
    let log_path = {
        let dir = sail_common::storage::data_dir("logs");
        if dir.is_dir() {
            dir.join("runner.log")
        } else {
            std::fs::create_dir_all(".local")?;
            std::path::PathBuf::from(".local/runner.log")
        }
    };
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let filter=tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_|"warn,sail_core=debug,sail_common=debug,sail_mind=debug,sail_agents=debug,sail_flow=debug,sail_discovery=debug,sail_arena=debug,sail_bridge=debug,sail_dialogue=debug,sail_canvas=debug".into());
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .pretty()
                .with_ansi(false)
                .with_file(true)
                .with_line_number(true)
                .with_thread_ids(true)
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .with_writer(RunnerWriter(Arc::new(Mutex::new(log)))),
        )
        .try_init()?;
    tracing::info!(
        log_path = %log_path.display(),
        "Detailed runner logging enabled; use RUST_LOG for per-token and per-frame trace output"
    );
    Ok(())
}
