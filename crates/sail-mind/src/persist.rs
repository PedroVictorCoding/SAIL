//! Keep a managed llama-server warm across SAIL restarts.
//!
//! When `LocalModelConfig::persist` is on, SAIL launches llama-server in its own process
//! group, so a terminal Ctrl+C does not reach it. SAIL detaches from it on shutdown
//! instead of killing it, and writes a record of how it was launched. The next run reads
//! that record:
//! - if the server was started with the same binary and arguments, it is adopted as-is;
//! - otherwise it is stopped and relaunched, so changed settings take effect.
//!
//! `./run.sh --stop` (or `--stop-model`) stops it explicitly.

use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedServer {
    pub pid: u32,
    /// Process start time (clock ticks since boot) from `/proc/<pid>/stat`; with the pid it
    /// identifies this exact process, so a recycled pid is never signalled.
    pub start_ticks: u64,
    pub binary: PathBuf,
    pub args: Vec<String>,
}

/// Where the record for the server on `port` is kept (next to `.local/llama-server.log`).
pub fn record_path(port: u16) -> PathBuf {
    PathBuf::from(".local").join(format!("llama-server.{port}.json"))
}

/// State and start time of a running process, from `/proc/<pid>/stat`.
fn proc_stat(pid: u32) -> Option<(char, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the parenthesised command name: state is #3, starttime is #22.
    let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
    let state = fields.next()?.chars().next()?;
    Some((state, fields.nth(18)?.parse().ok()?))
}
pub fn process_start_ticks(pid: u32) -> Option<u64> {
    proc_stat(pid).map(|(_, start)| start)
}

impl PersistedServer {
    pub fn read(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temp, path)
    }
    /// True while the recorded process (same pid and start time) runs and is not a zombie.
    pub fn is_alive(&self) -> bool {
        proc_stat(self.pid).is_some_and(|(state, start)| state != 'Z' && start == self.start_ticks)
    }
    pub fn same_launch(&self, binary: &Path, args: &[String]) -> bool {
        self.binary == binary && self.args == args
    }
    /// SIGTERM, then SIGKILL after `grace`. Returns once the process has gone.
    pub async fn terminate(&self, grace: Duration) {
        let pid = self.pid as libc::pid_t;
        if pid <= 1 || !self.is_alive() {
            return;
        }
        // SAFETY: plain signal delivery to a pid verified above to be the recorded process.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let deadline = Instant::now() + grace;
        while self.is_alive() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if self.is_alive() {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            while self.is_alive() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}
