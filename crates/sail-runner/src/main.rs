use std::{os::fd::FromRawFd, path::PathBuf, sync::Arc};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("LISTEN_PID")? != std::process::id().to_string()
        || std::env::var("LISTEN_FDS")? != "1"
    {
        return Err("requires one systemd-activated Unix socket".into());
    }
    let config = sail_runner::RunnerConfig {
        rootfs: PathBuf::from(std::env::var("SAIL_RUNNER_ROOTFS")?),
        cgroup_parent: PathBuf::from(std::env::var("SAIL_RUNNER_CGROUP")?),
        runner_uid: unsafe { libc::geteuid() },
        daemon_uid: std::env::var("SAIL_RUNNER_DAEMON_UID")?.parse()?,
    };
    config.validate()?;
    // SAFETY: systemd supplied exactly one owned listening descriptor at FD 3.
    if unsafe { libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let socket = unsafe { std::os::unix::net::UnixListener::from_raw_fd(3) };
    socket.set_nonblocking(true)?;
    sail_runner::serve(
        tokio::net::UnixListener::from_std(socket)?,
        Arc::new(config),
    )
    .await?;
    Ok(())
}
