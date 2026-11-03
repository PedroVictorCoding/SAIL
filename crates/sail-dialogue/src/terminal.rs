use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalClientMessage {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
    Restart,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalServerMessage {
    Connected { shell: String, cwd: String },
    Output { data: String },
    Exit { code: Option<i32> },
    Error { message: String },
}

#[derive(Clone)]
pub struct TerminalSession {
    #[cfg(unix)]
    master_fd: i32,
    child_id: u32,
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

        let slave_file = unsafe { std::fs::File::from_raw_fd(slave) };
        let slave_in = slave_file.try_clone().map_err(|e| e.to_string())?;
        let slave_out = slave_file.try_clone().map_err(|e| e.to_string())?;
        let slave_err = slave_file;

        let mut cmd = std::process::Command::new(shell);
        cmd.current_dir(cwd)
            .stdin(slave_in)
            .stdout(slave_out)
            .stderr(slave_err)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("LANG", "en_US.UTF-8");

        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 1);
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

        // Background writer thread
        let master_writer_fd = unsafe { libc::dup(master) };
        let alive_writer = alive.clone();
        std::thread::spawn(move || {
            let mut writer = unsafe { std::fs::File::from_raw_fd(master_writer_fd) };
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
        let master_reader_fd = unsafe { libc::dup(master) };
        let alive_reader = alive.clone();
        std::thread::spawn(move || {
            let mut reader = unsafe { std::fs::File::from_raw_fd(master_reader_fd) };
            let mut buf = [0u8; 4096];
            loop {
                if !alive_reader.load(Ordering::SeqCst) {
                    break;
                }
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
            alive_reader.store(false, Ordering::SeqCst);
        });

        // Monitor child exit
        let alive_monitor = alive.clone();
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
            alive_monitor.store(false, Ordering::SeqCst);
        });

        Ok((
            Self {
                master_fd: master,
                child_id,
                stdin_tx,
                alive,
                shell_name: shell.to_string(),
                cwd: cwd_str.to_string(),
            },
            output_rx,
        ))
    }

    fn spawn_piped(
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
            .env("COLORTERM", "truecolor")
            .env("LANG", "en_US.UTF-8");

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
        let alive_stdout = alive.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                if !alive_stdout.load(Ordering::SeqCst) {
                    break;
                }
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
            alive_stdout.store(false, Ordering::SeqCst);
        });

        let alive_stderr = alive.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                if !alive_stderr.load(Ordering::SeqCst) {
                    break;
                }
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
        std::thread::spawn(move || {
            let _ = child.wait();
            alive_monitor.store(false, Ordering::SeqCst);
        });

        Ok((
            Self {
                #[cfg(unix)]
                master_fd: -1,
                child_id,
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
        if self.master_fd >= 0 {
            let ws = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            unsafe {
                libc::ioctl(self.master_fd, libc::TIOCSWINSZ as _, &ws);
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
        self.alive.store(false, Ordering::SeqCst);
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child_id as libc::pid_t, libc::SIGTERM);
        }
    }
}

pub async fn handle_terminal_socket(socket: WebSocket, initial_cwd: Option<PathBuf>) {
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
        return;
    }

    info!(
        shell = %session.shell_name,
        cwd = %session.cwd,
        "In-browser terminal connected"
    );

    let (out_tx, mut out_rx) = mpsc::channel::<Message>(128);

    let writer_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    let out_tx_clone = out_tx.clone();
    let mut forwarder_task = tokio::spawn(async move {
        while let Some(bytes) = output_rx.recv().await {
            let data = String::from_utf8_lossy(&bytes).to_string();
            let msg =
                serde_json::to_string(&TerminalServerMessage::Output { data }).unwrap_or_default();
            if out_tx_clone.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    while let Some(result) = ws_receiver.next().await {
        let msg = match result {
            Ok(m) => m,
            Err(_) => break,
        };

        match msg {
            Message::Text(text) => {
                if let Ok(client_msg) = serde_json::from_str::<TerminalClientMessage>(&text) {
                    match client_msg {
                        TerminalClientMessage::Input { data } => {
                            let _ = session.write(data.as_bytes()).await;
                        }
                        TerminalClientMessage::Resize { cols, rows } => {
                            session.resize(cols, rows);
                        }
                        TerminalClientMessage::Restart => {
                            session.kill();
                            forwarder_task.abort();
                            if let Ok((new_session, mut new_output_rx)) =
                                TerminalSession::spawn(initial_cwd.clone())
                            {
                                session = new_session;
                                let reconnected =
                                    serde_json::to_string(&TerminalServerMessage::Connected {
                                        shell: session.shell_name.clone(),
                                        cwd: session.cwd.clone(),
                                    })
                                    .unwrap_or_default();
                                let _ = out_tx.send(Message::Text(reconnected)).await;

                                let out_tx_restart = out_tx.clone();
                                forwarder_task = tokio::spawn(async move {
                                    while let Some(bytes) = new_output_rx.recv().await {
                                        let data = String::from_utf8_lossy(&bytes).to_string();
                                        let msg =
                                            serde_json::to_string(&TerminalServerMessage::Output {
                                                data,
                                            })
                                            .unwrap_or_default();
                                        if out_tx_restart.send(Message::Text(msg)).await.is_err() {
                                            break;
                                        }
                                    }
                                });
                            }
                        }
                    }
                } else {
                    let _ = session.write(text.as_bytes()).await;
                }
            }
            Message::Binary(bytes) => {
                let _ = session.write(&bytes).await;
            }
            Message::Close(_) => break,
            _ => {}
        }

        if !session.is_alive() {
            let exit_msg = serde_json::to_string(&TerminalServerMessage::Exit { code: None })
                .unwrap_or_default();
            let _ = out_tx.send(Message::Text(exit_msg)).await;
            break;
        }
    }

    forwarder_task.abort();
    writer_task.abort();
    session.kill();
    info!("In-browser terminal disconnected");
}
