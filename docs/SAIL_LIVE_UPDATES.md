# Opt-in local rebuild after a PR merge

This is a *poll-based* deployment mechanism: a user-level systemd timer fetches
`main` every two minutes. Whenever its commit changes, it checks out the exact
commit in a separate immutable release worktree, tests and builds SAIL, and only
then switches an atomic `current` symlink and restarts the managed service.
It does not use GitHub webhooks, expose a port, or touch the owner's development
checkout. Commits pushed directly to `main` also trigger it; it cannot distinguish
merged PRs from other updates. The updater does **not** merge PRs itself.

## Setup on the SAIL machine

1. Ensure `git`, `flock`, `curl`, `cargo`, `systemctl --user` and the Rust toolchain
   are installed. Have the SAIL runtime disk mounted and a writable
   `/srv/sail/cargo-target` directory (or explicitly set `CARGO_TARGET_DIR`).
2. Stop any existing SAIL process launched from the owner checkout. Two copies
   must not contend for port 8088. Preserve its local `.local/` credentials and
   TLS settings separately: fresh release worktrees do not copy these. Set stable
   `SAIL_TLS_CERT`, `SAIL_TLS_KEY`, `SAIL_DATA`, and other required absolute paths
   through the environment file as appropriate.
3. From the SAIL repository run `bash scripts/install-sail-live.sh`. It only
   installs units; it does not enable or restart them.
4. Edit `~/.config/sail-live/environment` (file mode 600) to choose the release
   root and any runtime settings. The updater defaults to
   `~/.local/share/sail-live`; its bare Git repository and releases are kept
   separate from the owner's checkout. For an HTTPS endpoint health check, set
   `SAIL_LIVE_HEALTH_URL` and configure the host's trust store for its TLS cert.
5. Run `systemctl --user enable --now sail-live-update.timer` to opt in. The
   initial successful build will start `sail-live.service`. Enable login lingering
   only if you deliberately want updates while logged out.

## Monitor / disable

```bash
systemctl --user status sail-live.service sail-live-update.timer
journalctl --user -u sail-live-update.service -n 100 --no-pager
~/.local/bin/sail-live-update status
systemctl --user disable --now sail-live-update.timer
```

The previously deployed release is kept intact and restored when service restart
or an optional health check fails. Without a configured health URL, a successful
`systemctl restart` is the only startup check; it is **not** proof that the web
UI or voice pipeline is healthy. Keep the former release for manual recovery.
The updater deliberately does not delete old releases or build artifacts.

This is a deployment option, not a claim that the timer has been enabled on the
owner's host. Do not turn it on until service ownership, credentials and TLS
have been migrated from any currently running SAIL process.
