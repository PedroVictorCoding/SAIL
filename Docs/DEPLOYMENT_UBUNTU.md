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

2. **Cgroups v2 (for process and memory limits)**:
   Verify cgroups v2 controllers:
   ```bash
   cat /sys/fs/cgroup/cgroup.controllers
   # Should include: cpu memory pids
   ```

---

## 4. Running the SAIL Server

Start the core daemon with the unified runner (which automatically provisions TLS certificates in `.local/tls/`, configures `CARGO_TARGET_DIR=target`, and binds to port 8088):
```bash
./run.sh
```

Or run directly with cargo:
```bash
CARGO_TARGET_DIR=target SAIL_VOICE_BIND=0.0.0.0:8088 \
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

