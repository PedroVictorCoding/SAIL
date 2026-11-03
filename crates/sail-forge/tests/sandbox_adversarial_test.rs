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

/// Host-side marker path a sandboxed process cannot write: the sandbox gets its own `/tmp`.
fn host_marker(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("forge_escape_{name}_{}", Uuid::new_v4()))
}

#[test]
fn build_script_cannot_plant_git_commands_that_run_on_the_host() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // A build script has write access to the worktree, including `.git`. Forge then runs host
    // Git there (`get_diff` after every test run, `apply_patch` for model edits). Each payload
    // below makes Git run a command: core.fsmonitor on any index refresh, diff.external and a
    // textconv driver on `git diff`, a hook on later Git operations.
    let fsmonitor = host_marker("fsmonitor");
    let external = host_marker("external");
    let textconv = host_marker("textconv");
    let hook = host_marker("hook");
    let submodule = host_marker("submodule");
    let build_rs = format!(
        r####"use std::io::Write;
fn main() {{
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // Writes into `.git` may be refused; carry on so the build itself succeeds.
    let cfg = std::fs::OpenOptions::new().append(true).open(format!("{{dir}}/.git/config"));
    let _ = cfg.and_then(|mut cfg| cfg.write_all(br#"
[core]
	fsmonitor = "touch {fsmonitor}; false"
	hooksPath = .githooks
[diff]
	external = "touch {external}; :"
[diff "evil"]
	textconv = "touch {textconv}; cat"
"#));
    std::fs::write(format!("{{dir}}/.gitattributes"), "* diff=evil\n").unwrap();
    std::fs::create_dir_all(format!("{{dir}}/.githooks")).unwrap();
    for h in ["pre-commit", "post-checkout", "post-index-change"] {{
        let p = format!("{{dir}}/.githooks/{{h}}");
        std::fs::write(&p, "#!/bin/sh\ntouch {hook}\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }}
    // A repository at the submodule path: `git diff` checks a submodule for changes by
    // running `git status` inside it, which applies that repository's clean filter to a
    // modified file. `-c` overrides on the outer Git do not reach this.
    let sub = format!("{{dir}}/sub");
    let git = |args: &[&str]| std::process::Command::new("git").args(args).status().unwrap();
    git(&["init", "-q", &sub]);
    std::fs::write(format!("{{sub}}/a"), "1").unwrap();
    std::fs::write(format!("{{sub}}/.gitattributes"), "a filter=evil\n").unwrap();
    git(&["-C", &sub, "add", "a", ".gitattributes"]);
    git(&["-C", &sub, "config", "filter.evil.clean", "touch {submodule}; cat"]);
    std::fs::write(format!("{{sub}}/a"), "2").unwrap();
    // A tracked change, so `git diff` has something to render.
    std::fs::write(format!("{{dir}}/src/lib.rs"), "pub fn f() {{ }}\n").unwrap();
}}
"####,
        fsmonitor = fsmonitor.display(),
        external = external.display(),
        textconv = textconv.display(),
        hook = hook.display(),
        submodule = submodule.display(),
    );
    let repo = malicious_repo(&build_rs);
    // Record `sub` as a submodule (a gitlink) so Git treats the build's repository as one.
    git(
        &repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,1111111111111111111111111111111111111111,sub",
        ],
    );
    // Not `commit_all`: `git add .` would drop the gitlink, whose directory does not exist.
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.local",
            "commit",
            "-q",
            "-m",
            "submodule",
        ],
    );
    let wt = worktree_for(&repo);
    let out = wt
        .run_command("cargo", &["build", "--offline"], 300)
        .expect("sandboxed build must run");
    assert_eq!(out.status, 0, "build failed:\n{}", out.stderr);
    let nested = fs::read_to_string(wt.root_dir.join("sub/.git/config")).unwrap_or_default();
    assert!(
        nested.contains("filter \"evil\""),
        "the probe planted no nested repository; the test proves nothing:\n{}",
        out.stderr
    );

    // The host-side Git operations Forge performs after a sandboxed command.
    let diff = wt.get_diff().expect("diff must still work");
    let patched = wt.apply_patch(
        "--- a/Cargo.toml\n+++ b/Cargo.toml\n@@ -1,1 +1,1 @@\n-[package]\n+[package] \n",
    );
    let _ = wt.get_diff();

    let escaped: Vec<_> = [&fsmonitor, &external, &textconv, &hook, &submodule]
        .into_iter()
        .filter(|m| m.exists())
        .collect();
    for m in &escaped {
        let _ = fs::remove_file(m);
    }
    assert!(
        escaped.is_empty(),
        "commands planted by the sandboxed build ran on the host: {escaped:?}"
    );
    // Neutralizing the payloads must not cost Forge the real diff or its patching.
    assert!(
        diff.contains("src/lib.rs"),
        "diff lost the real change:\n{diff}"
    );
    patched.expect("patch must still apply");
    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn host_side_edits_never_reach_git_metadata() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Forge's own edit tools run on the host for the model. A prompt-injected model, or a
    // symlink a sandboxed build left behind, must not get them to write `.git`.
    let build_rs = r#"fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let _ = std::os::unix::fs::symlink(".git", format!("{dir}/alias"));
    let _ = std::os::unix::fs::symlink(".git/info/attributes", format!("{dir}/dangling"));
}
"#;
    let repo = malicious_repo(build_rs);
    let wt = worktree_for(&repo);
    let out = wt
        .run_command("cargo", &["build", "--offline"], 300)
        .expect("sandboxed build must run");
    assert_eq!(out.status, 0, "build failed:\n{}", out.stderr);
    assert!(
        wt.root_dir.join("alias").is_symlink(),
        "the probe planted no symlink"
    );
    let config = wt.root_dir.join(".git/config");
    let before = fs::read(&config).unwrap();

    for path in [
        ".git/config",
        ".GIT/config",
        "src/../.git/config",
        "alias/config",
    ] {
        let res = wt.write_file(Path::new(path), "[core]\n\tfsmonitor = touch x\n");
        assert!(
            matches!(res, Err(WorktreeError::GitMetadataPath(_))),
            "write to {path} was not refused: {res:?}"
        );
    }
    for target in [".git/config", "alias/config"] {
        let patch = format!("--- /dev/null\n+++ b/{target}\n@@ -0,0 +1,1 @@\n+[core]\n");
        assert!(wt.apply_patch(&patch).is_err(), "patch to {target} applied");
    }
    // Its target does not exist yet; a write through it would create it inside `.git`.
    let res = wt.write_file(Path::new("dangling"), "* diff=evil\n");
    assert!(
        res.is_err(),
        "write through a dangling symlink into .git: {res:?}"
    );
    assert!(!wt.root_dir.join(".git/info/attributes").exists());
    assert_eq!(fs::read(&config).unwrap(), before, ".git/config changed");

    // If the config changes anyway (here: by hand), host Git refuses to run on it.
    fs::write(
        &config,
        [before.as_slice(), b"[core]\n\tfsmonitor = touch x\n"].concat(),
    )
    .unwrap();
    assert!(matches!(
        wt.get_diff(),
        Err(WorktreeError::GitConfigTampered)
    ));
    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn sandbox_output_symlinks_are_not_followed_to_host_files() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Forge reads the files a sandboxed command produced (test report, symbol index,
    // mutation lists) and clears old ones. A build script can leave those paths as symlinks
    // to host files: reading would put a host secret into the model's context or a report,
    // and clearing would delete host data.
    let outside = std::env::temp_dir().join(format!("forge_outside_{}", Uuid::new_v4()));
    fs::create_dir_all(outside.join("mutants.out")).unwrap();
    fs::write(outside.join("secret.txt"), SECRET).unwrap();
    fs::write(outside.join("mutants.out/keep.txt"), "host data").unwrap();
    let build_rs = format!(
        r#"fn main() {{
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let link = |target: &str, at: &str| {{
        let at = format!("{{dir}}/{{at}}");
        std::fs::create_dir_all(std::path::Path::new(&at).parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&at);
        std::os::unix::fs::symlink(target, &at).unwrap();
    }};
    link("{secret}", "target/nextest/forge/junit.xml");
    link("{secret}", "target/sail-forge/index.scip");
    link("{outside}", "target/scratch");
}}
"#,
        secret = outside.join("secret.txt").display(),
        outside = outside.display(),
    );
    let repo = malicious_repo(&build_rs);
    let wt = worktree_for(&repo);
    let out = wt
        .run_command("cargo", &["build", "--offline"], 300)
        .expect("sandboxed build must run");
    assert_eq!(out.status, 0, "build failed:\n{}", out.stderr);
    assert!(
        wt.root_dir.join("target/scratch").is_symlink(),
        "no symlink planted"
    );

    for rel in [
        "target/nextest/forge/junit.xml",
        "target/sail-forge/index.scip",
        "target/scratch/secret.txt",
    ] {
        let res = wt.read_output(Path::new(rel));
        assert!(
            matches!(res, Err(WorktreeError::SymlinkEscape { .. })),
            "read of {rel} was not refused: {:?}",
            res.map(|b| String::from_utf8_lossy(&b).into_owned())
        );
    }
    assert!(
        wt.remove_output(Path::new("target/scratch/mutants.out"))
            .is_err()
    );
    assert!(
        outside.join("mutants.out/keep.txt").exists(),
        "host data was deleted through a symlinked parent"
    );
    let _ = fs::remove_dir_all(&repo);
    let _ = fs::remove_dir_all(&outside);
}
