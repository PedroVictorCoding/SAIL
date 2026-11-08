//! Local deterministic DAG runner. Checkpoints are atomic, fsynced snapshots.
//! A restart pauses incomplete runs; explicit resume reuses committed receipts.
use sail_protocol::flow::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Mutex,
};
use uuid::Uuid;

#[derive(Debug)]
pub enum Error {
    Invalid(String),
    Conflict(String),
    NotFound,
    Limit,
    Storage,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(s) | Self::Conflict(s) => f.write_str(s),
            Self::NotFound => f.write_str("Not found"),
            Self::Limit => f.write_str("Flow storage or execution limit reached"),
            Self::Storage => f.write_str("Flow storage unavailable"),
        }
    }
}
type Result<T> = std::result::Result<T, Error>;
#[derive(Default, Clone, Serialize, Deserialize)]
struct Snapshot {
    workflows: Vec<Workflow>,
    runs: Vec<Run>,
}
pub struct Store {
    path: PathBuf,
    state: Mutex<Snapshot>,
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn invalid(s: &str) -> Error {
    Error::Invalid(s.into())
}

/// Validate every node and edge before writing or running. One input per node,
/// arbitrary fan-out, exactly one manual trigger, at least one result sink.
pub fn validate(w: &Workflow) -> Result<Vec<String>> {
    if w.version != VERSION {
        return Err(invalid("Unsupported Flow version"));
    }
    if w.name.trim().is_empty() || w.name.len() > 120 || w.nodes.is_empty() || w.nodes.len() > 32 {
        return Err(invalid("Use a name of 1–120 bytes and 1–32 nodes"));
    }
    let mut ids = HashSet::new();
    for n in &w.nodes {
        if n.id.is_empty()
            || n.id.len() > 48
            || !n.id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            || !ids.insert(n.id.clone())
        {
            return Err(invalid("Node IDs must be unique alphanumeric identifiers"));
        }
        if let Operation::Prefix { text } | Operation::Contains { text } = &n.operation {
            if text.len() > 1024 {
                return Err(invalid("Node text exceeds 1024 bytes"));
            }
        }
    }
    if w.nodes
        .iter()
        .filter(|n| matches!(n.operation, Operation::Trigger {}))
        .count()
        != 1
        || !w
            .nodes
            .iter()
            .any(|n| matches!(n.operation, Operation::Output {}))
    {
        return Err(invalid(
            "Use exactly one manual trigger and at least one output",
        ));
    }
    for n in &w.nodes {
        if matches!(n.operation, Operation::Trigger {}) {
            if n.source.is_some() || n.branch != Branch::Always {
                return Err(invalid("Trigger cannot have an input"));
            }
        } else {
            let src = w
                .nodes
                .iter()
                .find(|s| Some(&s.id) == n.source.as_ref())
                .ok_or_else(|| invalid("Each non-trigger node needs an existing source"))?;
            if matches!(src.operation, Operation::Output {}) {
                return Err(invalid("Output is a terminal node"));
            }
            if n.branch != Branch::Always && !matches!(src.operation, Operation::Contains { .. }) {
                return Err(invalid("True/false routes require a condition source"));
            }
        }
    }
    let mut order = Vec::new();
    while order.len() < w.nodes.len() {
        let before = order.len();
        for n in &w.nodes {
            if !order.contains(&n.id) && n.source.as_ref().is_none_or(|s| order.contains(s)) {
                order.push(n.id.clone());
            }
        }
        if before == order.len() {
            return Err(invalid("Workflow contains a cycle"));
        }
    }
    Ok(order)
}
impl Store {
    pub fn open(path: PathBuf) -> Result<Self> {
        let mut snapshot: Snapshot = if path.exists() {
            if fs::metadata(&path).map_err(|_| Error::Storage)?.len() > 32 * 1024 * 1024 {
                return Err(Error::Limit);
            }
            serde_json::from_slice(&fs::read(&path).map_err(|_| Error::Storage)?)
                .map_err(|_| Error::Storage)?
        } else {
            Snapshot::default()
        };
        for w in &snapshot.workflows {
            validate(w)?;
        }
        for r in &mut snapshot.runs {
            validate(&r.workflow)?;
            if r.state == RunState::Running {
                r.state = RunState::Paused;
                r.updated_at = now();
            }
        }
        let store = Self {
            path,
            state: Mutex::new(snapshot),
        };
        {
            let state = store.state.lock().map_err(|_| Error::Storage)?;
            store.persist(&state)?;
        }
        Ok(store)
    }
    fn persist(&self, s: &Snapshot) -> Result<()> {
        let parent = self.path.parent().ok_or(Error::Storage)?;
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o770);
        }
        directory.create(parent).map_err(|_| Error::Storage)?;
        let temp = parent.join(format!(".flow-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut opts = OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o660);
            }
            let mut file = opts.open(&temp).map_err(|_| Error::Storage)?;
            serde_json::to_writer(&mut file, s).map_err(|_| Error::Storage)?;
            file.flush().map_err(|_| Error::Storage)?;
            file.sync_all().map_err(|_| Error::Storage)?;
            fs::rename(&temp, &self.path).map_err(|_| Error::Storage)?;
            File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|_| Error::Storage)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
    fn change<T>(&self, f: impl FnOnce(&mut Snapshot) -> Result<T>) -> Result<T> {
        let mut guard = self.state.lock().map_err(|_| Error::Storage)?;
        let mut next = guard.clone();
        let out = f(&mut next)?;
        self.persist(&next)?;
        *guard = next;
        Ok(out)
    }
    pub fn list(&self) -> Result<(Vec<Workflow>, Vec<Run>)> {
        let s = self.state.lock().map_err(|_| Error::Storage)?;
        let latest = s
            .workflows
            .iter()
            .filter(|w| {
                !s.workflows
                    .iter()
                    .any(|v| v.id == w.id && v.revision > w.revision)
            })
            .cloned()
            .collect();
        Ok((latest, s.runs.clone()))
    }
    pub fn save(&self, request: SaveWorkflow) -> Result<Workflow> {
        validate(&request.workflow)?;
        self.change(|s| {
            let latest = s
                .workflows
                .iter()
                .filter(|w| w.id == request.workflow.id)
                .map(|w| w.revision)
                .max()
                .unwrap_or(0);
            if latest != request.expected_revision || request.workflow.revision != latest + 1 {
                return Err(Error::Conflict(
                    "Workflow changed; reload before saving".into(),
                ));
            }
            if s.workflows.len() >= 128 {
                return Err(Error::Limit);
            }
            s.workflows.push(request.workflow.clone());
            Ok(request.workflow)
        })
    }
    pub fn start(&self, q: StartRun) -> Result<Run> {
        if q.input.len() > 4096 {
            return Err(invalid("Input exceeds 4096 bytes"));
        }
        self.change(|s| {
            if let Some(r) = s.runs.iter().find(|r| r.id == q.id) {
                if r.workflow.id != q.workflow_id
                    || r.workflow.revision != q.revision
                    || r.input != q.input
                {
                    return Err(Error::Conflict(
                        "Run ID already used for another request".into(),
                    ));
                }
                return Ok(r.clone());
            }
            if s.runs.len() >= 64 {
                return Err(Error::Limit);
            }
            let workflow = s
                .workflows
                .iter()
                .find(|w| w.id == q.workflow_id && w.revision == q.revision)
                .cloned()
                .ok_or(Error::NotFound)?;
            let r = Run {
                id: q.id,
                workflow,
                input: q.input,
                state: RunState::Running,
                receipts: vec![],
                error: None,
                created_at: now(),
                updated_at: now(),
            };
            s.runs.push(r.clone());
            Ok(r)
        })
    }
    pub fn control(&self, id: Uuid, resume: bool) -> Result<Run> {
        self.change(|s| {
            let r = s
                .runs
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or(Error::NotFound)?;
            if resume {
                if r.state != RunState::Paused {
                    return Err(Error::Conflict("Only interrupted runs can resume".into()));
                }
                r.state = RunState::Running;
            } else if matches!(r.state, RunState::Running | RunState::Paused) {
                r.state = RunState::Cancelled;
            } else {
                return Err(Error::Conflict("Run is already finished".into()));
            }
            r.updated_at = now();
            Ok(r.clone())
        })
    }
    /// Explicit owner-requested deletion; live work must be cancelled first.
    pub fn delete_run(&self, id: Uuid) -> Result<()> {
        self.change(|s| {
            let r = s.runs.iter().find(|r| r.id == id).ok_or(Error::NotFound)?;
            if matches!(r.state, RunState::Running | RunState::Paused) {
                return Err(Error::Conflict(
                    "Cancel the run before deleting its receipts".into(),
                ));
            }
            s.runs.retain(|r| r.id != id);
            Ok(())
        })
    }
    pub fn delete_workflow(&self, id: Uuid) -> Result<()> {
        self.change(|s| {
            if !s.workflows.iter().any(|w| w.id == id) {
                return Err(Error::NotFound);
            }
            if s.runs.iter().any(|r| {
                r.workflow.id == id && matches!(r.state, RunState::Running | RunState::Paused)
            }) {
                return Err(Error::Conflict(
                    "Cancel active runs before deleting the workflow".into(),
                ));
            }
            s.workflows.retain(|w| w.id != id);
            Ok(())
        })
    }
    /// Execute at most one pure node. Returns whether more work remains.
    pub fn step(&self, id: Uuid) -> Result<bool> {
        self.change(|s| {
            let r = s
                .runs
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or(Error::NotFound)?;
            if r.state != RunState::Running {
                return Ok(false);
            }
            let order = validate(&r.workflow)?;
            let next = order
                .iter()
                .find(|id| !r.receipts.iter().any(|p| &p.node_id == *id));
            if let Some(id) = next {
                let n = r
                    .workflow
                    .nodes
                    .iter()
                    .find(|n| &n.id == id)
                    .ok_or(Error::Storage)?;
                let source = n
                    .source
                    .as_ref()
                    .and_then(|id| r.receipts.iter().find(|p| &p.node_id == id));
                let skipped = source.is_some_and(|p| {
                    p.skipped
                        || match n.branch {
                            Branch::Always => false,
                            Branch::True => p.decision != Some(true),
                            Branch::False => p.decision != Some(false),
                        }
                });
                let input = source.map(|p| p.output.as_str()).unwrap_or(&r.input);
                let (output, decision) = if skipped {
                    (String::new(), None)
                } else {
                    match &n.operation {
                        Operation::Trigger {} | Operation::Output {} => (input.to_string(), None),
                        Operation::Trim {} => (input.trim().to_string(), None),
                        Operation::Uppercase {} => (input.to_uppercase(), None),
                        Operation::Prefix { text } => (format!("{text}{input}"), None),
                        Operation::Contains { text } => {
                            (input.to_string(), Some(input.contains(text)))
                        }
                    }
                };
                if output.len() > 8192 {
                    r.state = RunState::Failed;
                    r.error = Some(format!("Node {} exceeded 8192 output bytes", n.id));
                } else {
                    r.receipts.push(NodeReceipt {
                        node_id: n.id.clone(),
                        skipped,
                        output,
                        decision,
                        completed_at: now(),
                    });
                }
            }
            if r.state == RunState::Running && r.receipts.len() == r.workflow.nodes.len() {
                r.state = RunState::Completed;
            }
            r.updated_at = now();
            Ok(r.state == RunState::Running)
        })
    }
}
