//! Adversarial checks of Forge's Bubblewrap sandbox (#94).
//!
//! A crate whose `build.rs` behaves maliciously is built with `IsolatedWorktree::run_command`,
//! the same path the solver uses for `cargo test`. The build script tries to:
//! - read registry credentials from `CARGO_HOME`,
//! - read and write files in the source repository Forge cloned from,
//! - connect to a listener on the host's loopback interface and to the internet,
//! - leave a process running after the command times out.
//!
//! It records what it could do in `probe.txt` inside the worktree, and the test asserts on
//! that file and on host-side evidence (file hashes, the listener, a marker file).

use std::collections::BTreeMap;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sail_forge::worktree::{IsolatedWorktree, WorktreeError};
use sail_guardian::sandbox::SandboxPolicy;
use uuid::Uuid;

// Tests here change process environment variables; never run two at once in one process.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const SECRET: &str = "sail-test-secret-4f1c";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit_all(dir: &Path, msg: &str) {
    git(dir, &["add", "."]);
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.local",
            "commit",
            "-q",
            "-m",
            msg,
        ],
    );
}

/// SHA-256 of every file under `root` (including `.git`), keyed by relative path.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let digest = ring::digest::digest(&ring::digest::SHA256, &fs::read(&p).unwrap());
                out.insert(
                    p.strip_prefix(root).unwrap().to_path_buf(),
                    digest.as_ref().to_vec(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A fake `$HOME` whose `.cargo` holds secrets, with `.cargo/bin` holding the real
/// toolchain's `cargo` and `rustc`. Returns `(home, cargo_home)`. This is the usual rustup layout, where
/// Guardian resolves `cargo` to `$HOME/.cargo/bin/cargo`.
fn fake_home() -> (PathBuf, PathBuf) {
    let real = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .expect("CARGO_HOME or HOME must be set");
    let home = std::env::temp_dir().join(format!("forge_fake_home_{}", Uuid::new_v4()));
    let fake = home.join(".cargo");
    fs::create_dir_all(&fake).unwrap();
    // Mirror rustup's layout inside the fake home: the `rustup` binary plus `cargo` and
    // `rustc` proxies next to it (rustup picks the tool from the name it was run as). A
    // symlink to the real bin directory would point outside the sandbox's mounts.
    let bin = fake.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let rustup = fs::canonicalize(real.join("bin").join("cargo")).unwrap();
    if fs::hard_link(&rustup, bin.join("rustup")).is_err() {
        fs::copy(&rustup, bin.join("rustup")).unwrap();
    }
    for tool in ["cargo", "rustc"] {
        std::os::unix::fs::symlink("rustup", bin.join(tool)).unwrap();
    }
    fs::write(
        fake.join("credentials.toml"),
        format!("[registry]\ntoken = \"{SECRET}\"\n"),
    )
    .unwrap();
    fs::write(
        fake.join("config.toml"),
        format!("[registries.private]\ntoken = \"{SECRET}\"\n"),
    )
    .unwrap();
    (home, fake)
}

/// Commit a crate with the given build script, plus a secret file, and return its path.
fn malicious_repo(build_rs: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("forge_adv_repo_{}", Uuid::new_v4()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n\n[workspace]\n",
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    fs::write(root.join("build.rs"), build_rs).unwrap();
    fs::write(root.join("host_secret.txt"), SECRET).unwrap();
    // The build script reads its source repo's host path from here.
    fs::write(
        root.join("repo_path.txt"),
        root.to_string_lossy().as_bytes(),
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "target/\nCargo.lock\n").unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    commit_all(&root, "init");
    root
}

fn worktree_for(repo: &Path) -> IsolatedWorktree {
    let job = format!("adv-{}", Uuid::new_v4().simple());
    IsolatedWorktree::create(&job, repo, "HEAD", Arc::new(SandboxPolicy::new())).unwrap()
}

fn probe_build_rs(port: u16) -> String {
    format!(
        r#####"use std::io::Write;
fn verdict(ok: bool) -> &'static str {{ if ok {{ "ALLOWED" }} else {{ "denied" }} }}
fn main() {{
    let mut log = String::new();
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_default();
    for f in ["credentials.toml", "config.toml"] {{
        let ok = std::fs::read_to_string(format!("{{cargo_home}}/{{f}}")).is_ok();
        log.push_str(&format!("read {{f}}: {{}}\n", verdict(ok)));
    }}
    let repo = include_str!("repo_path.txt").trim().to_string();
    let ok = std::fs::read_to_string(format!("{{repo}}/host_secret.txt")).is_ok();
    log.push_str(&format!("read source repo: {{}}\n", verdict(ok)));
    let ok = std::fs::write(format!("{{repo}}/pwned.txt"), "x").is_ok();
    log.push_str(&format!("write source repo: {{}}\n", verdict(ok)));
    let ok = std::fs::write(format!("{{repo}}/.git/hooks/pre-commit"), "x").is_ok();
    log.push_str(&format!("write source git: {{}}\n", verdict(ok)));
    let t = std::time::Duration::from_secs(2);
    let host: std::net::SocketAddr = "127.0.0.1:{port}".parse().unwrap();
    let ok = std::net::TcpStream::connect_timeout(&host, t).is_ok();
    log.push_str(&format!("connect host loopback: {{}}\n", verdict(ok)));
    let ext: std::net::SocketAddr = "1.1.1.1:443".parse().unwrap();
    let ok = std::net::TcpStream::connect_timeout(&ext, t).is_ok();
    log.push_str(&format!("connect internet: {{}}\n", verdict(ok)));
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    std::fs::File::create(format!("{{dir}}/probe.txt")).unwrap().write_all(log.as_bytes()).unwrap();
}}
"#####
    )
}

#[test]
fn malicious_build_script_cannot_reach_secrets_source_or_network() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, cargo_home) = fake_home();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();

    let repo = malicious_repo(&probe_build_rs(port));
    let before = snapshot(&repo);

    let saved: Vec<_> = ["HOME", "CARGO_HOME"]
        .iter()
        .map(|k| (*k, std::env::var_os(k)))
        .collect();
    // SAFETY: serialized by ENV_LOCK; nothing else in this process reads these now.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("CARGO_HOME", &cargo_home);
    }
    let wt = worktree_for(&repo);
    let out = wt.run_command("cargo", &["build", "--offline"], 300);
    for (k, v) in saved {
        match v {
            Some(v) => unsafe { std::env::set_var(k, v) },
            None => unsafe { std::env::remove_var(k) },
        }
    }
    let out = out.expect("sandboxed build must run");
    assert_eq!(
        out.status, 0,
        "build failed:\n{}\n{}",
        out.stdout, out.stderr
    );

    let probe = wt
        .read_file(Path::new("probe.txt"))
        .expect("build script wrote no probe");
    assert!(
        !probe.contains("ALLOWED"),
        "sandbox let the build script through:\n{probe}"
    );
    for line in [
        "read credentials.toml: denied",
        "read config.toml: denied",
        "read source repo: denied",
        "write source repo: denied",
        "write source git: denied",
        "connect host loopback: denied",
        "connect internet: denied",
    ] {
        assert!(probe.contains(line), "missing `{line}` in:\n{probe}");
    }
    assert!(!out.stdout.contains(SECRET) && !out.stderr.contains(SECRET));
    assert!(
        listener.accept().is_err(),
        "the host listener received a connection"
    );
    assert_eq!(snapshot(&repo), before, "the source repository changed");

    let _ = fs::remove_dir_all(&repo);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn timeout_kills_the_command_and_every_process_it_started() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // The build script starts a detached shell (no inherited pipes, so cargo does not wait
    // for it) that writes a marker 25 s later, after the 20 s timeout, then hangs.
    let build_rs = r#"fn main() {
    use std::process::Stdio;
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("sleep 25; echo alive > {dir}/survivor.txt"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::fs::write(format!("{dir}/started.txt"), "1").unwrap();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
"#;
    let repo = malicious_repo(build_rs);
    let wt = worktree_for(&repo);

    // Leave time to compile the build script, then time out while it hangs.
    let start = Instant::now();
    let res = wt.run_command("cargo", &["build", "--offline"], 20);
    let elapsed = start.elapsed();
    assert!(
        matches!(res, Err(WorktreeError::CommandTimeout { .. })),
        "expected a timeout, got {res:?}"
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "run_command took {elapsed:?} to return"
    );
    assert!(
        wt.root_dir.join("started.txt").exists(),
        "build script never started; the test proves nothing"
    );

    // Wait until well past the moment the background shell would have written.
    while start.elapsed() < Duration::from_secs(40) {
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        !wt.root_dir.join("survivor.txt").exists(),
        "a process started by the timed-out command kept running"
    );

    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn background_processes_end_when_the_command_finishes() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // The build script starts a detached shell (no inherited pipes) and returns at once.
    let build_rs = r#"fn main() {
    use std::process::Stdio;
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("sleep 5; echo alive > {dir}/survivor.txt"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
}
"#;
    let repo = malicious_repo(build_rs);
    let wt = worktree_for(&repo);
    let out = wt
        .run_command("cargo", &["build", "--offline"], 300)
        .unwrap();
    assert_eq!(out.status, 0, "{}", out.stderr);

    std::thread::sleep(Duration::from_secs(9));
    assert!(
        !wt.root_dir.join("survivor.txt").exists(),
        "a background process outlived the sandboxed command"
    );
    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn file_size_limit_applies_inside_the_sandbox() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // The build script writes 2 MiB. It reruns whenever trigger.txt changes, so the second
    // run reuses the compiled script and only the 2 MiB write meets the 1 MiB limit.
    let build_rs = r#"fn main() {
    println!("cargo:rerun-if-changed=trigger.txt");
    let dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(format!("{dir}/big.bin"), vec![0u8; 2 * 1024 * 1024]).unwrap();
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    std::fs::write(format!("{manifest}/fsize.txt"), "wrote 2 MiB").unwrap();
}
"#;
    let repo = malicious_repo(build_rs);
    fs::write(repo.join("trigger.txt"), "1").unwrap();
    commit_all(&repo, "trigger");
    let wt = worktree_for(&repo);

    let warm = wt
        .run_command("cargo", &["build", "--offline"], 300)
        .unwrap();
    assert_eq!(warm.status, 0, "{}", warm.stderr);
    assert!(
        wt.root_dir.join("fsize.txt").exists(),
        "default limits must allow a 2 MiB file"
    );

    fs::remove_file(wt.root_dir.join("fsize.txt")).unwrap();
    fs::write(wt.root_dir.join("trigger.txt"), "2").unwrap();
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::set_var("SAIL_FORGE_SANDBOX_MAX_FILE_BYTES", "1048576") };
    let res = wt.run_command("cargo", &["build", "--offline"], 300);
    unsafe { std::env::remove_var("SAIL_FORGE_SANDBOX_MAX_FILE_BYTES") };
    let out = res.expect("command must run");

    assert_ne!(out.status, 0, "a 2 MiB write must fail under a 1 MiB limit");
    assert!(
        out.stderr.contains("signal: 25") || out.stderr.contains("SIGXFSZ"),
        "expected a file-size-limit failure:\n{}",
        out.stderr
    );
    assert!(!wt.root_dir.join("fsize.txt").exists());

    let _ = fs::remove_dir_all(&repo);
}
