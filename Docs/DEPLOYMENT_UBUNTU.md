# SAIL Deployment & Host Security Guide (Ubuntu Linux)

This guide documents the host prerequisites and runtime verification for running SAIL with nono and Landlock LSM on Ubuntu Linux.

## 1. Operating System & Kernel Requirements

- **Supported Distributions**: Ubuntu 22.04 LTS, Ubuntu 24.04 LTS, or newer.
- **Kernel Version**: Linux kernel 5.13 or newer (kernel 6.x / 7.x recommended for Landlock ABI V4+ network filtering and V6 signal/abstract socket scoping).
- **Architecture**: `x86_64` or `aarch64`.

To verify your kernel and Landlock support:
```bash
uname -r
cat /sys/kernel/security/lsm
```
Ensure `landlock` is listed in `/sys/kernel/security/lsm` (enabled by default on standard Ubuntu kernels).

---

## 2. Required System Packages

Install Bubblewrap for namespace defense-in-depth isolation:
```bash
sudo apt update
sudo apt install -y bubblewrap libseccomp-dev
```

Verify Bubblewrap:
```bash
bwrap --version
```

---

## 3. User Namespaces & Cgroups v2

1. **Unprivileged User Namespaces**:
   Bubblewrap and nono leverage unprivileged namespaces:
   ```bash
   cat /proc/sys/kernel/unprivileged_userns_clone
   # Expected output: 1
   ```
   If set to `0`, enable via sysctl:
   ```bash
   sudo sysctl -w kernel.unprivileged_userns_clone=1
   ```
   On Ubuntu 24.04 and later, AppArmor also restricts unprivileged user namespaces. With the
   restriction active, Bubblewrap starts but cannot configure its network namespace and fails
   with `bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted`, so Forge's sandboxed
   builds are refused (fail closed). Check it:
   ```bash
   cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns
   # 1 = restricted
   ```
   Prefer an AppArmor profile that grants `userns` to `/usr/bin/bwrap` only (Ubuntu ships an
   example, `bwrap-userns-restrict`, in the `apparmor` package's extra profiles). Lifting the
   restriction system-wide also works but widens it for every program:
   ```bash
   sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
   ```

2. **Cgroups v2 (for process and memory limits)**:
   Verify cgroups v2 controllers:
   ```bash
   cat /sys/fs/cgroup/cgroup.controllers
   # Should include: cpu memory pids
   ```

---

## 4. Running the SAIL Server

Start the core daemon with the unified runner (which automatically provisions TLS certificates in `.local/tls/`, puts Cargo's target directory on the runtime SSD — `${SAIL_RUNTIME:-/srv/sail}/cargo-target` when writable — and binds to port 8088):
```bash
./run.sh
```

Or run directly with cargo:
```bash
CARGO_TARGET_DIR="${SAIL_RUNTIME:-/srv/sail}/cargo-target" SAIL_VOICE_BIND=0.0.0.0:8088 \
  SAIL_TLS_CERT=.local/tls/voice-cert.pem \
  SAIL_TLS_KEY=.local/tls/voice-key.pem \
  cargo run -p sail-core
```

---

## 5. Verifying Security Status

1. In the web interface, open **Settings → Guardian Security**.
2. Verify the status pill displays **Protected** (with `Landlock V6/V8 + Bubblewrap · nono 0.81.0`).
3. Or query the status endpoint directly:
   ```bash
   curl -k https://127.0.0.1:8088/api/guardian/status
   ```

## 6. Persistent data (`/srv/sail-data`)

All persistent SAIL data lives under one root, resolved by `sail_common::storage`:

1. `SAIL_DATA`, when set;
2. `/srv/sail-data`, **only when it is a mounted filesystem** and writable. When the data disk is not mounted, the empty mount point is on the root SSD, and SAIL refuses to fill it. Set `SAIL_DATA_ALLOW_UNMOUNTED=1` on a machine that has no separate data disk;
3. otherwise `~/.local/share/sail`. The daemon logs this at startup as an error, naming the reason.

| Path under the root | Contents |
|---|---|
| `state/continuity.db`, `state/forge-checkpoints/` | Durable background tasks and Forge checkpoints |
| `forge/` | Nightly state, digests, eval results, Codex review ledger |
| `arena/arena.db` | Memory and knowledge |
| `guardian/` | Learned rules, safety-review findings |
| `logs/runner.log` | Daemon log (previously `.local/runner.log` in the working directory) |

Hot paths stay on the runtime SSD: Forge worktrees, Cargo `target/` and active GGUF models in `/srv/sail`.

**Ownership: not root-only.** After mounting the disk, run once:

```bash
sudo scripts/setup-sail-data.sh --user <account that runs sail-core>
```

It checks that the path is mounted. It creates a `sail-data` group, adds the user to it, and makes the root and its directories `<user>:sail-data` with mode `2770` (setgid, so new directories keep the group; nothing for other users). It gives existing data to the group, adds a default ACL when `setfacl` is installed (`apt install acl`), and writes `/etc/tmpfiles.d/sail-data.conf`. Log out and back in for the group to apply. Use `--dry-run` to see each command first.

The sandbox runner user (`sail-runner`) must **not** be in `sail-data`: sandboxed code must not be able to write SAIL's own data. The script refuses to continue if it is. Some files stay owner-only by design, such as the review ledger and the failure memory (`0600`).

**Migration.** On first start, data at the old defaults is moved into the root: `/srv/sail/state/continuity.db` (copied with SQLite `VACUUM INTO`), `~/.local/state/sail/forge`, `~/.sail/forge/review_ledger.json`, and `.local/guardian_*.json`. Old copies are renamed `*.migrated-to-sail-data`, never deleted. Remove them once SAIL runs correctly. An explicit override (`SAIL_CONTINUITY_DB`, `SAIL_STATE_DIR`, `SAIL_FORGE_STATE_DIR`, `SAIL_DATA_DIR`, `SAIL_ARENA_DB`, `SAIL_LEARNED_RULES_PATH`, `SAIL_SAFETY_REVIEW_PATH`) is used as given and never migrated.

For a systemd unit, add `RequiresMountsFor=/srv/sail-data` so SAIL does not start before the disk is mounted.

