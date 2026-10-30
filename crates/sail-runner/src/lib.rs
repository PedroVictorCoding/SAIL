//! Dedicated-UID command service; never spawn agent commands in the daemon.
use sail_protocol::execution::{CommandPlan, CommandResult, CommandStatus};
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
    fn new(parent: &Path, plan: &CommandPlan) -> io::Result<Self> {
        let path = parent.join(format!("command-{}", plan.request_id));
        fs::create_dir(&path)?;
        let group = Self { path };
        fs::write(
            group.path.join("memory.max"),
            plan.limits.memory_bytes.to_string(),
        )?;
        fs::write(group.path.join("memory.swap.max"), "0")?;
        fs::write(group.path.join("pids.max"), plan.limits.pids.to_string())?;
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
    plan: &CommandPlan,
    status: CommandStatus,
    message: String,
    start: Instant,
) -> CommandResult {
    CommandResult {
        version: 1,
        request_id: plan.request_id,
        correlation_id: plan.correlation_id,
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
pub async fn execute(
    config: &RunnerConfig,
    plan: &CommandPlan,
    cancel: CancellationToken,
) -> CommandResult {
    let start = Instant::now();
    if let Err(error) = plan.validate() {
        return empty_result(plan, CommandStatus::Denied, error, start);
    }
    if let Err(error) = config.validate() {
        return empty_result(plan, CommandStatus::Unavailable, error.to_string(), start);
    }
    let group = match Group::new(&config.cgroup_parent, plan) {
        Ok(group) => group,
        Err(error) => {
            return empty_result(plan, CommandStatus::Unavailable, error.to_string(), start);
        }
    };
    let membership = match OpenOptions::new()
        .write(true)
        .open(group.path.join("cgroup.procs"))
    {
        Ok(file) => file,
        Err(error) => {
            return empty_result(plan, CommandStatus::Unavailable, error.to_string(), start);
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
        .args(["--proc", "/proc", "--dev", "/dev", "--size"])
        .arg(plan.limits.scratch_bytes.to_string())
        .args(["--tmpfs", "/work", "--size"])
        .arg(plan.limits.scratch_bytes.to_string())
        .args([
            "--tmpfs",
            "/tmp",
            "--chdir",
            "/work",
            "--setenv",
            "PATH",
            "/usr/bin:/bin",
            "--setenv",
            "LANG",
            "C.UTF-8",
            "--",
        ])
        .arg(&plan.executable)
        .args(&plan.argv)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let limits = plan.limits;
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
            return empty_result(plan, CommandStatus::Unavailable, error.to_string(), start);
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
        request_id: plan.request_id,
        correlation_id: plan.correlation_id,
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
            let result=async {
                let length=tokio::time::timeout(Duration::from_secs(2),stream.read_u32()).await.map_err(io::Error::other)??;
                if length>64*1024 {return Err(io::Error::other("command frame limit"));}
                let mut bytes=vec![0;length as usize];tokio::time::timeout(Duration::from_secs(2),stream.read_exact(&mut bytes)).await.map_err(io::Error::other)??;
                let plan:CommandPlan=serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                let cancel=CancellationToken::new();let task=execute(&config,&plan,cancel.clone());tokio::pin!(task);
                let response=tokio::select! {response=&mut task=>response,_=stream.read_u8()=>{cancel.cancel();task.await}};
                let bytes=serde_json::to_vec(&response).map_err(io::Error::other)?;
                tokio::time::timeout(Duration::from_secs(2),async {stream.write_u32(bytes.len() as u32).await?;stream.write_all(&bytes).await}).await.map_err(io::Error::other)??;
                Ok::<(),io::Error>(())
            }.await;
            if result.is_err() {
                let _ = stream.shutdown().await;
            }
        });
    }
}
/// Exact request digest becomes the Guardian capability; argv, budgets and task
/// correlation cannot be altered after approval.
pub fn capability(plan: &CommandPlan) -> sail_guardian::Capability {
    let bytes = serde_json::to_vec(plan).expect("typed command serialization");
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    sail_guardian::Capability::Tool(sail_guardian::ToolCapability::SandboxCommand {
        request: String::from_utf8(bytes.clone()).expect("JSON UTF-8"),
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
    if serde_json::to_vec(plan)
        .map_err(|error| sail_guardian::ExecutionDenied::PolicyDenied {
            reason: error.to_string(),
        })?
        .len()
        > 64 * 1024
    {
        return Err(sail_guardian::ExecutionDenied::PolicyDenied {
            reason: "command frame limit".into(),
        });
    }
    broker.authorize_and_execute(
        actor,
        capability(plan),
        token,
        Some(plan.correlation_id),
        || {
            use std::io::Read;
            plan.validate().map_err(io::Error::other)?;
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
            let approvals = broker.approval().clone();
            let monitor = std::thread::spawn(move || {
                while !monitor_stopped.load(std::sync::atomic::Ordering::Acquire) {
                    if token.is_some_and(|token| approvals.is_revoked(token)) {
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
            stream.set_read_timeout(Some(Duration::from_millis(plan.limits.wall_ms + 5000)))?;
            stream.set_write_timeout(Some(Duration::from_secs(2)))?;
            let bytes = serde_json::to_vec(plan).map_err(io::Error::other)?;
            if bytes.len() > 64 * 1024 {
                return Err(io::Error::other("command request frame limit"));
            }
            stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
            stream.write_all(&bytes)?;
            let mut size = [0; 4];
            stream.read_exact(&mut size)?;
            let size = u32::from_be_bytes(size) as usize;
            if size > 2 * 1024 * 1024 {
                return Err(io::Error::other("command result frame limit"));
            }
            let mut bytes = vec![0; size];
            stream.read_exact(&mut bytes)?;
            let mut result: CommandResult =
                serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if result.version != 1
                || result.request_id != plan.request_id
                || result.correlation_id != plan.correlation_id
            {
                return Err(io::Error::other("command result correlation mismatch"));
            }
            result.stdout = sail_common::diagnostics::redact(&result.stdout);
            result.stderr = sail_common::diagnostics::redact(&result.stderr);
            Ok(result)
        },
    )
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
