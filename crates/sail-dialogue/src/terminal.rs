use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
};
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalClientMessage {
    /// First message on the socket: the owner token Guardian approvals use.
    Auth {
        token: String,
    },
    Input {
        data: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Restart,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalServerMessage {
    Connected {
        shell: String,
        cwd: String,
    },
    Output {
        data: String,
    },
    Exit {
        code: Option<i32>,
    },
    /// No shell was spawned: the socket did not prove it is the owner.
    Locked {
        reason: String,
    },
    Error {
        message: String,
    },
}

/// What a terminal socket must prove before a shell is spawned. The host shell is the widest
/// capability SAIL exposes, so the socket is not trusted: the owner token is required, the
/// socket gets no shell until the first message authenticates, and every attempt is audited.
pub struct TerminalGuard {
    /// The owner token this boot uses. `None` closes the terminal (fail closed).
    owner_token: Option<String>,
    /// Where the browser said it came from; recorded, never authenticated by itself.
    origin: String,
    /// Guardian's ledger, so opening and refusing a host shell are auditable.
    audit: Option<Arc<sail_guardian::AuditLedger>>,
}

impl TerminalGuard {
    pub fn new(
        owner_token: Option<String>,
        origin: impl Into<String>,
        audit: Option<Arc<sail_guardian::AuditLedger>>,
    ) -> Self {
        Self {
            owner_token,
            origin: origin.into(),
            audit,
        }
    }

    fn record(&self, outcome: sail_guardian::audit::DecisionOutcome, reason: String) {
        use sail_guardian::capability::{Capability, ToolCapability};
        if let Some(audit) = &self.audit {
            let receipt = audit.record(
                sail_common::types::SubsystemId::Dialogue,
                Capability::Tool(ToolCapability::KernelTool("terminal:session".into())),
                outcome,
                reason.clone(),
            );
            match outcome {
                sail_guardian::audit::DecisionOutcome::Granted => {
                    info!(receipt = %receipt.receipt_id, %reason, "Host shell authorized")
                }
                _ => warn!(receipt = %receipt.receipt_id, %reason, "Host shell refused"),
            }
            return;
        }
        warn!(%reason, "Host shell event without an audit ledger");
    }
}

#[derive(Clone)]
pub struct TerminalSession {
    #[cfg(unix)]
    master_fd: Option<Arc<std::os::fd::OwnedFd>>,
    child_id: u32,
    exit_code: Arc<AtomicI32>,
    stdin_tx: mpsc::Sender<Vec<u8>>,
    alive: Arc<AtomicBool>,
    shell_name: String,
    cwd: String,
}

impl TerminalSession {
    pub fn spawn(cwd_override: Option<PathBuf>) -> Result<(Self, mpsc::Receiver<Vec<u8>>), String> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        let shell_path = if std::path::Path::new(&shell).exists() {
            shell
        } else if std::path::Path::new("/bin/bash").exists() {
            "/bin/bash".into()
        } else {
            "/bin/sh".into()
        };

        let cwd = cwd_override
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let cwd_str = cwd.to_string_lossy().to_string();

        #[cfg(unix)]
        {
            Self::spawn_pty(&shell_path, &cwd, &cwd_str)
        }

        #[cfg(not(unix))]
        {
            Self::spawn_piped(&shell_path, &cwd, &cwd_str)
        }
    }

    #[cfg(unix)]
    fn spawn_pty(
        shell: &str,
        cwd: &std::path::Path,
        cwd_str: &str,
    ) -> Result<(Self, mpsc::Receiver<Vec<u8>>), String> {
        use std::io::{Read, Write};
        use std::os::unix::io::FromRawFd;
        use std::os::unix::process::CommandExt;

        let mut master: libc::c_int = 0;
        let mut slave: libc::c_int = 0;

        let res = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };

        if res != 0 {
            warn!("openpty failed; falling back to piped shell");
            return Self::spawn_piped(shell, cwd, cwd_str);
        }

        let master = unsafe { std::os::fd::OwnedFd::from_raw_fd(master) };
        let slave_file = unsafe { std::fs::File::from_raw_fd(slave) };
        // openpty does not set close-on-exec; the shell must not inherit the master.
        use std::os::fd::AsRawFd;
        if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let master_writer = std::fs::File::from(master.try_clone().map_err(|e| e.to_string())?);
        let master_reader = std::fs::File::from(master.try_clone().map_err(|e| e.to_string())?);
        let slave_in = slave_file.try_clone().map_err(|e| e.to_string())?;
        let slave_out = slave_file.try_clone().map_err(|e| e.to_string())?;
        let slave_err = slave_file;

        let mut cmd = std::process::Command::new(shell);
        cmd.current_dir(cwd)
            .stdin(slave_in)
            .stdout(slave_out)
            .stderr(slave_err)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor");

        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn shell: {e}"))?;
        let child_id = child.id();

        let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(128);
        let (output_tx, output_rx) = mpsc::channel::<Vec<u8>>(128);
        let alive = Arc::new(AtomicBool::new(true));
        let exit_code = Arc::new(AtomicI32::new(i32::MIN));

        // Background writer thread
        let alive_writer = alive.clone();
        std::thread::spawn(move || {
            let mut writer = master_writer;
            while let Some(data) = stdin_rx.blocking_recv() {
                if !alive_writer.load(Ordering::SeqCst) {
                    break;
                }
                if writer.write_all(&data).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
        });

        // Background reader thread
        std::thread::spawn(move || {
            let mut reader = master_reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if output_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        // Monitor child exit
        let alive_monitor = alive.clone();
        let status_monitor = exit_code.clone();
        std::thread::spawn(move || {
            let mut child = child;
            if let Ok(status) = child.wait() {
                status_monitor.store(status.code().unwrap_or(-1), Ordering::SeqCst);
            }
            alive_monitor.store(false, Ordering::SeqCst);
        });

        Ok((
            Self {
                master_fd: Some(Arc::new(master)),
                child_id,
                exit_code,
                stdin_tx,
                alive,
                shell_name: shell.to_string(),
                cwd: cwd_str.to_string(),
            },
            output_rx,
        ))
    }

    pub fn spawn_piped(
        shell: &str,
        cwd: &std::path::Path,
        cwd_str: &str,
    ) -> Result<(Self, mpsc::Receiver<Vec<u8>>), String> {
        use std::io::{Read, Write};
        use std::process::Stdio;

        let mut cmd = std::process::Command::new(shell);
        cmd.current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor");

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn shell: {e}"))?;
        let child_id = child.id();
        let mut stdin = child.stdin.take().ok_or("Failed to open child stdin")?;
        let mut stdout = child.stdout.take().ok_or("Failed to open child stdout")?;
        let mut stderr = child.stderr.take().ok_or("Failed to open child stderr")?;

        let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(128);
        let (output_tx, output_rx) = mpsc::channel::<Vec<u8>>(128);
        let alive = Arc::new(AtomicBool::new(true));
        let exit_code = Arc::new(AtomicI32::new(i32::MIN));

        let alive_writer = alive.clone();
        std::thread::spawn(move || {
            while let Some(data) = stdin_rx.blocking_recv() {
                if !alive_writer.load(Ordering::SeqCst) {
                    break;
                }
                if stdin.write_all(&data).is_err() {
                    break;
                }
                let _ = stdin.flush();
            }
        });

        let output_tx_err = output_tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if output_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match stderr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if output_tx_err.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        let alive_monitor = alive.clone();
        let status_monitor = exit_code.clone();
        std::thread::spawn(move || {
            if let Ok(status) = child.wait() {
                status_monitor.store(status.code().unwrap_or(-1), Ordering::SeqCst);
            }
            alive_monitor.store(false, Ordering::SeqCst);
        });

        Ok((
            Self {
                #[cfg(unix)]
                master_fd: None,
                child_id,
                exit_code,
                stdin_tx,
                alive,
                shell_name: shell.to_string(),
                cwd: cwd_str.to_string(),
            },
            output_rx,
        ))
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let Some(master) = &self.master_fd else {
                return;
            };
            if cols == 0 || rows == 0 {
                return;
            }
            let ws = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            unsafe {
                libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws);
            }
        }
    }

    pub async fn write(&self, data: &[u8]) -> Result<(), String> {
        self.stdin_tx
            .send(data.to_vec())
            .await
            .map_err(|e| e.to_string())
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn kill(&self) {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        #[cfg(unix)]
        unsafe {
            // Hang up the terminal session and its foreground application.
            if let Some(master) = &self.master_fd {
                use std::os::fd::AsRawFd;
                let foreground = libc::tcgetpgrp(master.as_raw_fd());
                if foreground > 0 {
                    libc::kill(-foreground, libc::SIGHUP);
                }
                libc::kill(-(self.child_id as libc::pid_t), libc::SIGHUP);
            }
            libc::kill(self.child_id as libc::pid_t, libc::SIGTERM);
        }
    }
}

/// How long the socket waits for the owner token before the shell request is dropped.
pub const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Tell the socket why no shell was opened, then close it.
async fn send_and_close(mut socket: WebSocket, message: TerminalServerMessage) {
    let text = serde_json::to_string(&message).unwrap_or_default();
    let _ = socket.send(Message::Text(text)).await;
    let _ = socket.close().await;
}

pub async fn handle_terminal_socket(
    socket: WebSocket,
    initial_cwd: Option<PathBuf>,
    guard: TerminalGuard,
) {
    let mut socket = socket;
    // No shell exists yet: the socket must prove it is the owner first.
    let authority = guard
        .owner_token
        .as_deref()
        .and_then(|token| sail_guardian::authority::ApprovalAuthority::new(token).ok());
    let Some(authority) = authority else {
        guard.record(
            sail_guardian::audit::DecisionOutcome::Denied,
            "no owner token is configured, so the host shell stays closed".into(),
        );
        send_and_close(
            socket,
            TerminalServerMessage::Locked {
                reason: "This SAIL boot has no owner token, so the terminal stays closed. Set \
                         SAIL_APPROVAL_TOKEN and restart SAIL."
                    .into(),
            },
        )
        .await;
        return;
    };
    let owner = match tokio::time::timeout(AUTH_TIMEOUT, socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            matches!(serde_json::from_str::<TerminalClientMessage>(&text),
                Ok(TerminalClientMessage::Auth { token }) if authority.authenticates(&token))
        }
        _ => false,
    };
    if !owner {
        guard.record(
            sail_guardian::audit::DecisionOutcome::Denied,
            format!(
                "a host shell was requested without the owner token (origin: {})",
                guard.origin
            ),
        );
        send_and_close(
            socket,
            TerminalServerMessage::Locked {
                reason: "The owner token is required to open the host shell. Paste it in \
                         Guardian → Approvals and try again."
                    .into(),
            },
        )
        .await;
        return;
    }
    guard.record(
        sail_guardian::audit::DecisionOutcome::Granted,
        format!("owner opened a host shell (origin: {})", guard.origin),
    );
    let (mut ws_sender, mut ws_receiver) = socket.split();

    let (mut session, mut output_rx) = match TerminalSession::spawn(initial_cwd.clone()) {
        Ok(res) => res,
        Err(err) => {
            let err_msg = serde_json::to_string(&TerminalServerMessage::Error {
                message: format!("Failed to spawn terminal: {err}"),
            })
            .unwrap_or_default();
            let _ = ws_sender.send(Message::Text(err_msg)).await;
            return;
        }
    };

    let connected = serde_json::to_string(&TerminalServerMessage::Connected {
        shell: session.shell_name.clone(),
        cwd: session.cwd.clone(),
    })
    .unwrap_or_default();
    if ws_sender.send(Message::Text(connected)).await.is_err() {
        session.kill();
        return;
    }

    info!(
        shell = %session.shell_name,
        cwd = %session.cwd,
        "In-browser terminal connected"
    );

    loop {
        tokio::select! {
            output = output_rx.recv() => {
                match output {
                    Some(bytes) => {
                        // Preserve bytes across arbitrary PTY read boundaries. xterm owns UTF-8 decoding.
                        if ws_sender.send(Message::Binary(bytes)).await.is_err() { break; }
                    }
                    None => {
                        // The output channel closes after every reader has drained.
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                            while session.exit_code.load(Ordering::SeqCst) == i32::MIN && session.is_alive() {
                                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                            }
                        }).await;
                        let code = session.exit_code.load(Ordering::SeqCst);
                        let message = TerminalServerMessage::Exit {
                            code: (code != i32::MIN).then_some(code),
                        };
                        let _ = ws_sender.send(Message::Text(serde_json::to_string(&message).unwrap())).await;
                        break;
                    }
                }
            }
            incoming = ws_receiver.next() => {
                let Some(Ok(message)) = incoming else { break; };
                match message {
                    Message::Text(text) => {
                        match serde_json::from_str::<TerminalClientMessage>(&text) {
                            Ok(TerminalClientMessage::Input { data }) => {
                                if session.write(data.as_bytes()).await.is_err() { break; }
                            }
                            Ok(TerminalClientMessage::Resize { cols, rows }) => session.resize(cols, rows),
                            // Already authenticated; a repeated token changes nothing.
                            Ok(TerminalClientMessage::Auth { .. }) => {}
                            Ok(TerminalClientMessage::Restart) => {
                                session.kill();
                                match TerminalSession::spawn(initial_cwd.clone()) {
                                    Ok((new_session, new_output)) => {
                                        session = new_session;
                                        output_rx = new_output;
                                        let message = TerminalServerMessage::Connected {
                                            shell: session.shell_name.clone(), cwd: session.cwd.clone(),
                                        };
                                        if ws_sender.send(Message::Text(serde_json::to_string(&message).unwrap())).await.is_err() { break; }
                                    }
                                    Err(message) => {
                                        let error = TerminalServerMessage::Error { message };
                                        let _ = ws_sender.send(Message::Text(serde_json::to_string(&error).unwrap())).await;
                                        break;
                                    }
                                }
                            }
                            Err(_) => {} // Malformed protocol messages are never shell commands.
                        }
                    }
                    Message::Binary(bytes) => {
                        if session.write(&bytes).await.is_err() { break; }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
    session.kill();
    info!("In-browser terminal disconnected");
}
