//! Dedicated-UID command service; never spawn agent commands in the daemon.
//!
//! Two framed protocols are accepted on the administrator-created Unix socket:
//! - v1 `CommandPlan` (legacy command tool);
//! - v2 `ExecutionPlan` (`protocol = "execution-plan-v1"`), which additionally enforces an
//!   executable digest and exact writable roots, and fails closed for network or credential
//!   requests until those services are wired.
use sail_protocol::execution::{CommandLimits, CommandPlan, CommandResult, CommandStatus};
use sail_sandbox::ExecutionPlan;
use sail_sandbox::plan::path_within;
use std::sync::Arc;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::MetadataExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    process::Command,
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const PLAN_PROTOCOL: &str = "execution-plan-v1";
const MAX_FRAME: usize = 64 * 1024;

pub struct RunnerConfig {
    pub rootfs: PathBuf,
    pub cgroup_parent: PathBuf,
    pub runner_uid: u32,
    pub daemon_uid: u32,
}
impl RunnerConfig {
    pub fn validate(&self) -> io::Result<()> {
        let uid = unsafe { libc::geteuid() };
        if uid == 0 || uid != self.runner_uid || uid == self.daemon_uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "runner requires a dedicated non-root UID distinct from daemon",
            ));
        }
        let root = self.rootfs.canonicalize()?;
        if root == Path::new("/") || root != self.rootfs {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "rootfs must be an explicit canonical image directory",
            ));
        }
        // The image is administrator-provisioned. The runner cannot change the image
        // or any ancestor to substitute a production tree before bubblewrap opens it.
        for ancestor in root.ancestors() {
            let metadata = fs::symlink_metadata(ancestor)?;
            if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "rootfs ancestors must be root-owned and non-writable",
                ));
            }
        }
        let marker = root.join(".sail-runner-image-v1");
        let metadata = fs::symlink_metadata(&marker)?;
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || metadata.len() != b"SAIL isolated root image v1\n".len() as u64
            || fs::read_to_string(&marker)? != "SAIL isolated root image v1\n"
        {
            return Err(io::Error::other("root image marker missing or untrusted"));
        }
        let name = std::ffi::CString::new(self.cgroup_parent.as_os_str().as_encoded_bytes())
            .map_err(io::Error::other)?;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::statfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { stat.assume_init() }.f_type != 0x63677270 {
            return Err(io::Error::other("delegated cgroup v2 required"));
        }
        Ok(())
    }
}

struct Group {
    path: PathBuf,
}
impl Group {
    fn new(parent: &Path, request_id: Uuid, limits: &CommandLimits) -> io::Result<Self> {
        let path = parent.join(format!("command-{request_id}"));
        fs::create_dir(&path)?;
        let group = Self { path };
        fs::write(
            group.path.join("memory.max"),
            limits.memory_bytes.to_string(),
        )?;
        fs::write(group.path.join("memory.swap.max"), "0")?;
        fs::write(group.path.join("pids.max"), limits.pids.to_string())?;
        fs::write(group.path.join("cpu.max"), "100000 100000")?;
        Ok(group)
    }
    fn kill(&self) {
        let _ = fs::write(self.path.join("cgroup.kill"), "1");
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        self.kill();
        let _ = fs::remove_dir(&self.path);
    }
}

fn empty_result(
    request_id: Uuid,
    correlation_id: Uuid,
    status: CommandStatus,
    message: String,
    start: Instant,
) -> CommandResult {
    CommandResult {
        version: 1,
        request_id,
        correlation_id,
        status,
        exit_code: None,
        stdout: String::new(),
        stderr: message,
        output_truncated: false,
        elapsed_ms: start.elapsed().as_millis() as u64,
    }
}

async fn capture<R: AsyncRead + Unpin>(
    mut stream: R,
    limit: usize,
    overflow: CancellationToken,
) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut buf = [0; 4096];
    loop {
        let count = stream.read(&mut buf).await?;
        if count == 0 {
            return Ok(data);
        }
        let remaining = limit.saturating_sub(data.len());
        data.extend_from_slice(&buf[..count.min(remaining)]);
        if count > remaining {
            overflow.cancel();
            return Ok(data);
        }
    }
}

/// The parts the executor needs. A v1 command always gets `/work` and `/tmp`; a v2 plan gets
/// exactly the writable roots it declared.
struct LaunchSpec {
    request_id: Uuid,
    correlation_id: Uuid,
    executable: String,
    argv: Vec<String>,
    cwd: String,
    limits: CommandLimits,
    write_roots: Vec<String>,
}

impl LaunchSpec {
    fn from_command(plan: &CommandPlan) -> Self {
        Self {
            request_id: plan.request_id,
            correlation_id: plan.correlation_id,
            executable: plan.executable.clone(),
            argv: plan.argv.clone(),
            cwd: "/work".into(),
            limits: plan.limits,
            write_roots: vec!["/work".into(), "/tmp".into()],
        }
    }

    fn from_plan(plan: &ExecutionPlan, limits: CommandLimits) -> Self {
        Self {
            request_id: plan.request_id,
            correlation_id: plan.audit_correlation_id,
            executable: plan.executable.sandbox_path.clone(),
            argv: plan.argv.clone(),
            cwd: plan.cwd.clone(),
            limits,
            write_roots: normalize_roots(
                plan.write_roots
                    .iter()
                    .map(|root| root.path.clone())
                    .collect(),
            ),
        }
    }
}

/// Drop a root when a broader root already covers it, so overlapping declarations do not
/// produce duplicate tmpfs mounts.
fn normalize_roots(mut roots: Vec<String>) -> Vec<String> {
    roots.sort_by_key(|root| root.len());
    let mut normalized: Vec<String> = Vec::new();
    for root in roots {
        if !normalized
            .iter()
            .any(|existing| path_within(&root, existing))
        {
            normalized.push(root);
        }
    }
    normalized
}

fn command_limits(limits: &sail_sandbox::ExecutionLimits) -> CommandLimits {
    CommandLimits {
        wall_ms: limits.wall_ms,
        cpu_seconds: limits.cpu_seconds,
        memory_bytes: limits.memory_bytes,
        pids: limits.pids,
        fds: limits.fds,
        scratch_bytes: limits.scratch_bytes,
        output_bytes: limits.output_bytes,
    }
}

pub async fn execute(
    config: &RunnerConfig,
    plan: &CommandPlan,
    cancel: CancellationToken,
) -> CommandResult {
    let start = Instant::now();
    if let Err(error) = plan.validate() {
        return empty_result(
            plan.request_id,
            plan.correlation_id,
            CommandStatus::Denied,
            error,
            start,
        );
    }
    if let Err(error) = config.validate() {
        return empty_result(
            plan.request_id,
            plan.correlation_id,
            CommandStatus::Unavailable,
            error.to_string(),
            start,
        );
    }
    execute_spec(config, LaunchSpec::from_command(plan), cancel, start).await
}

/// Execute the typed plan. Network and credential requests are denied until the broker and
/// provider bridges are actually wired; they are never silently ignored.
pub async fn execute_plan(
    config: &RunnerConfig,
    plan: &ExecutionPlan,
    cancel: CancellationToken,
) -> CommandResult {
    let start = Instant::now();
    let request_id = plan.request_id;
    let correlation_id = plan.audit_correlation_id;
    if let Err(error) = plan.validate(chrono::Utc::now().timestamp_millis()) {
        return empty_result(
            request_id,
            correlation_id,
            CommandStatus::Denied,
            error.to_string(),
            start,
        );
    }
    if plan.network.is_some() {
        return empty_result(
            request_id,
            correlation_id,
            CommandStatus::Denied,
            "network policy requested but the broker is not wired into the runner".into(),
            start,
        );
    }
    if !plan.credentials.is_empty() {
        return empty_result(
            request_id,
            correlation_id,
            CommandStatus::Denied,
            "credential references requested but the provider bridge is not wired".into(),
            start,
        );
    }
    if let Err(error) = config.validate() {
        return empty_result(
            request_id,
            correlation_id,
            CommandStatus::Unavailable,
            error.to_string(),
            start,
        );
    }
    if let Err(error) = verify_executable(config, plan) {
        return empty_result(
            request_id,
            correlation_id,
            CommandStatus::Denied,
            error,
            start,
        );
    }
    execute_spec(
        config,
        LaunchSpec::from_plan(plan, command_limits(&plan.limits)),
        cancel,
        start,
    )
    .await
}

fn verify_executable(config: &RunnerConfig, plan: &ExecutionPlan) -> Result<(), String> {
    let relative = plan.executable.sandbox_path.trim_start_matches('/');
    let path = config.rootfs.join(relative);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("executable is not readable in the trusted image: {error}"))?;
    if !metadata.is_file() {
        return Err("executable must be a regular file in the trusted image".into());
    }
    let bytes = fs::read(&path).map_err(|error| format!("read executable digest: {error}"))?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    let hex: String = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if hex != plan.executable.sha256 {
        return Err("executable digest does not match the trusted image".into());
    }
    Ok(())
}

async fn execute_spec(
    config: &RunnerConfig,
    spec: LaunchSpec,
    cancel: CancellationToken,
    start: Instant,
) -> CommandResult {
    let group = match Group::new(&config.cgroup_parent, spec.request_id, &spec.limits) {
        Ok(group) => group,
        Err(error) => {
            return empty_result(
                spec.request_id,
                spec.correlation_id,
                CommandStatus::Unavailable,
                error.to_string(),
                start,
            );
        }
    };
    let membership = match OpenOptions::new()
        .write(true)
        .open(group.path.join("cgroup.procs"))
    {
        Ok(file) => file,
        Err(error) => {
            return empty_result(
                spec.request_id,
                spec.correlation_id,
                CommandStatus::Unavailable,
                error.to_string(),
                start,
            );
        }
    };
    let mut command = Command::new("/usr/bin/bwrap");
    command
        .args([
            "--unshare-all",
            "--disable-userns",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--clearenv",
            "--ro-bind",
        ])
        .arg(&config.rootfs)
        .arg("/")
        .args(["--proc", "/proc", "--dev", "/dev"]);
    for root in &spec.write_roots {
        command
            .args(["--tmpfs", root])
            .arg("--size")
            .arg(spec.limits.scratch_bytes.to_string());
    }
    command
        .arg("--chdir")
        .arg(&spec.cwd)
        .args([
            "--setenv",
            "PATH",
            "/usr/bin:/bin",
            "--setenv",
            "LANG",
            "C.UTF-8",
            "--",
        ])
        .arg(&spec.executable)
        .args(&spec.argv)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let limits = spec.limits;
    // SAFETY: only async-signal-safe syscalls, stack data and pre-opened FDs run
    // between fork and exec. Join the constrained cgroup before any program runs.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::write(membership.as_raw_fd(), b"0".as_ptr().cast(), 1) != 1 {
                return Err(io::Error::last_os_error());
            }
            for (resource, value) in [
                (libc::RLIMIT_CPU, limits.cpu_seconds),
                (libc::RLIMIT_NOFILE, limits.fds as u64),
                (libc::RLIMIT_FSIZE, limits.scratch_bytes),
                (libc::RLIMIT_CORE, 0),
            ] {
                let bound = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
                if libc::setrlimit(resource, &bound) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return empty_result(
                spec.request_id,
                spec.correlation_id,
                CommandStatus::Unavailable,
                error.to_string(),
                start,
            );
        }
    };
    let overflow = CancellationToken::new();
    let stdout = tokio::spawn(capture(
        child.stdout.take().unwrap(),
        limits.output_bytes / 2,
        overflow.clone(),
    ));
    let stderr = tokio::spawn(capture(
        child.stderr.take().unwrap(),
        limits.output_bytes / 2,
        overflow.clone(),
    ));
    let (mut status, exit_code) = tokio::select! {
        biased;
        _=cancel.cancelled()=>(CommandStatus::Cancelled,None),
        _=overflow.cancelled()=>(CommandStatus::OutputLimit,None),
        _=tokio::time::sleep(Duration::from_millis(limits.wall_ms))=>(CommandStatus::TimedOut,None),
        exit=child.wait()=>match exit {Ok(exit)=>(if exit.success(){CommandStatus::Completed}else{CommandStatus::Failed},exit.code()),Err(_)=>(CommandStatus::Failed,None)}
    };
    group.kill();
    let _ = child.kill().await;
    let _ = child.wait().await;
    let read = async {
        Ok::<_, io::Error>((
            stdout.await.map_err(io::Error::other)??,
            stderr.await.map_err(io::Error::other)??,
        ))
    };
    let (out, err) = match tokio::time::timeout(Duration::from_secs(2), read).await {
        Ok(Ok(output)) => output,
        _ => {
            if status == CommandStatus::Completed {
                status = CommandStatus::Failed;
            }
            (Vec::new(), b"output capture unavailable".to_vec())
        }
    };
    if overflow.is_cancelled() {
        status = CommandStatus::OutputLimit;
    }
    CommandResult {
        version: 1,
        request_id: spec.request_id,
        correlation_id: spec.correlation_id,
        status,
        exit_code,
        stdout: sail_common::diagnostics::redact(&String::from_utf8_lossy(&out)),
        stderr: sail_common::diagnostics::redact(&String::from_utf8_lossy(&err)),
        output_truncated: overflow.is_cancelled(),
        elapsed_ms: start.elapsed().as_millis() as u64,
    }
}

/// Serve only daemon peers on an administrator-created Unix socket. Bounded
/// concurrency and frames prevent untrusted clients exhausting service memory.
pub async fn serve(listener: UnixListener, config: Arc<RunnerConfig>) -> io::Result<()> {
    config.validate()?;
    // Reconcile only our UUID-named job groups; never touch the service supervisor.
    for entry in fs::read_dir(&config.cgroup_parent)?.take(4096) {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name
            .strip_prefix("command-")
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            && entry.file_type()?.is_dir()
        {
            drop(Group { path: entry.path() });
        }
    }
    let slots = Arc::new(Semaphore::new(2));
    loop {
        let (mut stream, _) = listener.accept().await?;
        if stream.peer_cred()?.uid() != config.daemon_uid {
            continue;
        }
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let config = config.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let result = async {
                let length = tokio::time::timeout(Duration::from_secs(2), stream.read_u32())
                    .await
                    .map_err(io::Error::other)??;
                if length as usize > MAX_FRAME {
                    return Err(io::Error::other("command frame limit"));
                }
                let mut bytes = vec![0; length as usize];
                tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut bytes))
                    .await
                    .map_err(io::Error::other)??;
                let cancel = CancellationToken::new();
                let task = async {
                    match decode_frame(&bytes)? {
                        Incoming::Command(plan) => {
                            Ok::<CommandResult, io::Error>(execute(&config, &plan, cancel.clone()).await)
                        }
                        Incoming::Plan(plan) => Ok::<CommandResult, io::Error>(
                            execute_plan(&config, &plan, cancel.clone()).await,
                        ),
                    }
                };
                tokio::pin!(task);
                let response = tokio::select! {response=&mut task=>response?,_=stream.read_u8()=>{cancel.cancel();task.await?}};
                let bytes = serde_json::to_vec(&response).map_err(io::Error::other)?;
                tokio::time::timeout(Duration::from_secs(2), async {
                    stream.write_u32(bytes.len() as u32).await?;
                    stream.write_all(&bytes).await
                })
                .await
                .map_err(io::Error::other)??;
                Ok::<(), io::Error>(())
            }
            .await;
            if result.is_err() {
                let _ = stream.shutdown().await;
            }
        });
    }
}

enum Incoming {
    Command(CommandPlan),
    Plan(Box<ExecutionPlan>),
}

#[derive(serde::Deserialize)]
struct PlanFrame {
    protocol: String,
    plan: ExecutionPlan,
}

fn decode_frame(bytes: &[u8]) -> io::Result<Incoming> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| io::Error::other(error.to_string()))?;
    if value.get("protocol").and_then(|value| value.as_str()) == Some(PLAN_PROTOCOL) {
        let frame: PlanFrame =
            serde_json::from_value(value).map_err(|error| io::Error::other(error.to_string()))?;
        if frame.protocol != PLAN_PROTOCOL {
            return Err(io::Error::other("unsupported execution plan protocol"));
        }
        Ok(Incoming::Plan(Box::new(frame.plan)))
    } else {
        let plan: CommandPlan =
            serde_json::from_value(value).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Incoming::Command(plan))
    }
}

fn plan_frame(plan: &ExecutionPlan) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "protocol": PLAN_PROTOCOL,
        "plan": plan,
    }))
    .map_err(io::Error::other)
}

/// Exact request digest becomes the Guardian capability; argv, budgets and task
/// correlation cannot be altered after approval.
pub fn capability(plan: &CommandPlan) -> sail_guardian::Capability {
    let bytes = serde_json::to_vec(plan).expect("typed command serialization");
    capability_for(&bytes)
}

/// Digest over the full v2 frame, including the protocol and all plan fields.
pub fn capability_plan(plan: &ExecutionPlan) -> sail_guardian::Capability {
    let bytes = plan_frame(plan).expect("typed execution plan serialization");
    capability_for(&bytes)
}

fn capability_for(bytes: &[u8]) -> sail_guardian::Capability {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    sail_guardian::Capability::Tool(sail_guardian::ToolCapability::SandboxCommand {
        request: String::from_utf8(bytes.to_vec()).expect("JSON UTF-8"),
        request_digest: digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    })
}

/// Synchronous trusted dispatch for spawn_blocking callers. Dropping a socket
/// cancels service execution; no command is ever started on the daemon's UID.
pub fn dispatch(
    socket: &Path,
    expected_runner_uid: u32,
    broker: &sail_guardian::GuardianExecutionBroker,
    actor: sail_common::types::SubsystemId,
    plan: &CommandPlan,
    token: Option<uuid::Uuid>,
) -> Result<io::Result<CommandResult>, sail_guardian::ExecutionDenied> {
    plan.validate()
        .map_err(|reason| sail_guardian::ExecutionDenied::PolicyDenied { reason })?;
    let frame =
        serde_json::to_vec(plan).map_err(|error| sail_guardian::ExecutionDenied::PolicyDenied {
            reason: error.to_string(),
        })?;
    if frame.len() > MAX_FRAME {
        return Err(sail_guardian::ExecutionDenied::PolicyDenied {
            reason: "command frame limit".into(),
        });
    }
    let approvals = broker.approval().clone();
    broker.authorize_and_execute(
        actor,
        capability(plan),
        token,
        Some(plan.correlation_id),
        || {
            exchange(
                socket,
                expected_runner_uid,
                plan.request_id,
                plan.correlation_id,
                &frame,
                plan.limits.wall_ms + 5000,
                Arc::new(move || token.is_some_and(|token| approvals.is_revoked(token))),
            )
        },
    )
}

/// Dispatch a typed plan through Guardian. Network/credential plans are refused here as well as
/// in the runner, so a misconfiguration cannot silently fall back.
pub fn dispatch_plan(
    socket: &Path,
    expected_runner_uid: u32,
    broker: &sail_guardian::GuardianExecutionBroker,
    actor: sail_common::types::SubsystemId,
    plan: &ExecutionPlan,
    token: Option<uuid::Uuid>,
) -> Result<io::Result<CommandResult>, sail_guardian::ExecutionDenied> {
    plan.validate(chrono::Utc::now().timestamp_millis())
        .map_err(|error| sail_guardian::ExecutionDenied::PolicyDenied {
            reason: error.to_string(),
        })?;
    if plan.network.is_some() {
        return Err(sail_guardian::ExecutionDenied::PolicyDenied {
            reason: "network policy is not wired into the runner".into(),
        });
    }
    if !plan.credentials.is_empty() {
        return Err(sail_guardian::ExecutionDenied::PolicyDenied {
            reason: "credential references are not wired into the runner".into(),
        });
    }
    let frame = plan_frame(plan).map_err(|error| sail_guardian::ExecutionDenied::PolicyDenied {
        reason: error.to_string(),
    })?;
    if frame.len() > MAX_FRAME {
        return Err(sail_guardian::ExecutionDenied::PolicyDenied {
            reason: "command frame limit".into(),
        });
    }
    let approvals = broker.approval().clone();
    broker.authorize_and_execute(
        actor,
        capability_plan(plan),
        token,
        Some(plan.audit_correlation_id),
        || {
            exchange(
                socket,
                expected_runner_uid,
                plan.request_id,
                plan.audit_correlation_id,
                &frame,
                plan.limits.wall_ms + 5000,
                Arc::new(move || token.is_some_and(|token| approvals.is_revoked(token))),
            )
        },
    )
}

/// Trusted scheduler adapter entry. The caller must already have consumed the
/// exact command invocation grant. Never expose this primitive as a client route.
pub fn dispatch_authorized(
    socket: &Path,
    expected_runner_uid: u32,
    plan: &CommandPlan,
    cancel: CancellationToken,
) -> io::Result<CommandResult> {
    plan.validate().map_err(io::Error::other)?;
    let frame = serde_json::to_vec(plan).map_err(io::Error::other)?;
    if frame.len() > MAX_FRAME {
        return Err(io::Error::other("command request frame limit"));
    }
    exchange(
        socket,
        expected_runner_uid,
        plan.request_id,
        plan.correlation_id,
        &frame,
        plan.limits.wall_ms + 5000,
        Arc::new(move || cancel.is_cancelled()),
    )
}

/// Trusted scheduler adapter entry for typed plans.
pub fn dispatch_plan_authorized(
    socket: &Path,
    expected_runner_uid: u32,
    plan: &ExecutionPlan,
    cancel: CancellationToken,
) -> io::Result<CommandResult> {
    plan.validate(chrono::Utc::now().timestamp_millis())
        .map_err(io::Error::other)?;
    if plan.network.is_some() {
        return Err(io::Error::other(
            "network policy is not wired into the runner",
        ));
    }
    if !plan.credentials.is_empty() {
        return Err(io::Error::other(
            "credential references are not wired into the runner",
        ));
    }
    let frame = plan_frame(plan)?;
    if frame.len() > MAX_FRAME {
        return Err(io::Error::other("command request frame limit"));
    }
    exchange(
        socket,
        expected_runner_uid,
        plan.request_id,
        plan.audit_correlation_id,
        &frame,
        plan.limits.wall_ms + 5000,
        Arc::new(move || cancel.is_cancelled()),
    )
}

fn exchange(
    socket: &Path,
    expected_runner_uid: u32,
    request_id: Uuid,
    correlation_id: Uuid,
    frame: &[u8],
    read_timeout_ms: u64,
    should_cancel: Arc<dyn Fn() -> bool + Send + Sync>,
) -> io::Result<CommandResult> {
    use std::io::Read;
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    let mut peer = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            peer.as_mut_ptr().cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if unsafe { peer.assume_init() }.uid != expected_runner_uid
        || expected_runner_uid == unsafe { libc::geteuid() }
        || expected_runner_uid == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unexpected runner peer",
        ));
    }
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let monitor_stopped = stopped.clone();
    let watcher = stream.try_clone()?;
    let monitor = std::thread::spawn(move || {
        while !monitor_stopped.load(std::sync::atomic::Ordering::Acquire) {
            if should_cancel() {
                let _ = watcher.shutdown(std::net::Shutdown::Both);
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    });
    let _monitor = Monitor {
        stopped,
        thread: Some(monitor),
    };
    stream.set_read_timeout(Some(Duration::from_millis(read_timeout_ms)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    if frame.len() > MAX_FRAME {
        return Err(io::Error::other("command request frame limit"));
    }
    stream.write_all(&(frame.len() as u32).to_be_bytes())?;
    stream.write_all(frame)?;
    let mut size = [0; 4];
    stream.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if size > 2 * 1024 * 1024 {
        return Err(io::Error::other("command result frame limit"));
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    let mut result: CommandResult = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if result.version != 1
        || result.request_id != request_id
        || result.correlation_id != correlation_id
    {
        return Err(io::Error::other("command result correlation mismatch"));
    }
    result.stdout = sail_common::diagnostics::redact(&result.stdout);
    result.stderr = sail_common::diagnostics::redact(&result.stderr);
    Ok(result)
}

struct Monitor {
    stopped: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sail_common::types::SubsystemId;
    use sail_sandbox::{
        CredentialRef, ExecutableIdentity, ExecutionLimits, MountRoot, NetworkPolicyRef,
    };

    fn plan() -> ExecutionPlan {
        let now = chrono::Utc::now().timestamp_millis();
        ExecutionPlan {
            version: 1,
            job_id: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            principal: SubsystemId::Forge,
            capability_grant_id: Uuid::new_v4(),
            executable: ExecutableIdentity {
                sandbox_path: "/bin/echo".into(),
                sha256: "a".repeat(64),
            },
            argv: vec!["hello".into()],
            cwd: "/work".into(),
            read_roots: vec![MountRoot {
                path: "/bin".into(),
            }],
            write_roots: vec![MountRoot {
                path: "/work".into(),
            }],
            network: None,
            credentials: Vec::new(),
            limits: ExecutionLimits::default(),
            issued_at_ms: now,
            expires_at_ms: now + 60_000,
            audit_correlation_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn overlapping_write_roots_are_collapsed() {
        assert_eq!(
            normalize_roots(vec![
                "/work/scratch".into(),
                "/work".into(),
                "/cache".into(),
                "/work".into()
            ]),
            vec!["/work".to_string(), "/cache".to_string()]
        );
        assert!(normalize_roots(Vec::new()).is_empty());
    }

    #[test]
    fn executable_digest_is_checked_and_symlinks_are_rejected() {
        let root = std::env::temp_dir().join(format!("sail-runner-digest-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/echo"), b"trusted image bytes").unwrap();
        let config = RunnerConfig {
            rootfs: root.clone(),
            cgroup_parent: root.join("cgroup"),
            runner_uid: 1,
            daemon_uid: 2,
        };
        let mut plan = plan();
        let digest = ring::digest::digest(&ring::digest::SHA256, b"trusted image bytes");
        plan.executable.sha256 = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        verify_executable(&config, &plan).unwrap();

        plan.executable.sha256 = "0".repeat(64);
        assert!(verify_executable(&config, &plan).is_err());

        std::fs::remove_file(root.join("bin/echo")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("bin/echo")).unwrap();
        plan.executable.sha256 = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert!(verify_executable(&config, &plan).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn frames_round_trip_and_plan_binding_covers_mounts() {
        let command = CommandPlan {
            version: 1,
            request_id: Uuid::new_v4(),
            correlation_id: Uuid::new_v4(),
            executable: "/bin/echo".into(),
            argv: vec!["hello".into()],
            limits: CommandLimits::default(),
        };
        assert!(matches!(
            decode_frame(&serde_json::to_vec(&command).unwrap()).unwrap(),
            Incoming::Command(_)
        ));

        let original = plan();
        let bytes = plan_frame(&original).unwrap();
        assert!(matches!(decode_frame(&bytes).unwrap(), Incoming::Plan(_)));
        let original_cap = capability_plan(&original);
        let mut changed = original.clone();
        changed.write_roots.push(MountRoot {
            path: "/tmp".into(),
        });
        assert_ne!(capability_plan(&changed), original_cap);
        changed = original;
        changed.network = Some(NetworkPolicyRef {
            policy_id: "registry".into(),
            policy_version: 1,
        });
        assert_ne!(capability_plan(&changed), original_cap);
    }

    #[test]
    fn credentials_and_network_refs_are_well_formed() {
        let mut plan = plan();
        plan.credentials.push(CredentialRef {
            handle_id: Uuid::new_v4(),
            purpose: "github:issues".into(),
        });
        assert!(plan.validate(chrono::Utc::now().timestamp_millis()).is_ok());
        plan.credentials[0].purpose = "contains a space and =".into();
        assert!(
            plan.validate(chrono::Utc::now().timestamp_millis())
                .is_err()
        );
    }
}
