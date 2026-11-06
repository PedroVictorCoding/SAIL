# Disposable nono enforcement worker

`sail-nono-worker` is a Linux, one-command worker that applies Guardian's existing
capability set with `nono::Sandbox::apply_auto` before executing the target. It
runs in a fresh process, not a thread or a `pre_exec` callback inside SAIL. An
application error exits with status 125 and never launches the target. A successful
exec replaces the worker; the target's exit status is preserved.

Build with `cargo build -p sail-guardian --bin sail-nono-worker`. A trusted
supervisor starts the binary and writes one JSON object to stdin, then closes the
pipe. The object has `action` (a serialized `AuthorizedAction`), `program` (an
absolute executable path), `args` (an array of strings), and `cwd` (an absolute
working directory). The request is limited to 64 KiB. Expired authorizations are
refused. Standard input becomes an already-open null device, the target environment
is cleared, and stdout/stderr remain connected to the supervisor. The supervisor
must supply only intended inherited descriptors and must bound output, lifetime,
and cancellation. Do not expose this input to untrusted clients: deserializing an
`AuthorizedAction` is not authorization, token consumption, or approval checking.

This is an enforcement building block and verification harness. Forge and the
command service are not routed through it by this change. It does not change the
security status endpoint or establish production deployment. The existing builder
also grants read access to system binaries/libraries and existing Cargo/Rustup
roots under the worker's inherited HOME; it is not a credential mount policy.
Apply it only under a controlled supervisor environment.

Resource-limit metadata is not applied by nono's sandbox application API. This
worker therefore refuses actions with memory/process limits rather than silently
ignoring them. Cgroup quotas, dedicated UID provisioning, approved export, audit
reconciliation, and integration with the dedicated runner remain separate work.
Proxy-only requests are refused until supervised proxy-filter integration exists.
Network verification here covers TCP connect denial, not every networking
primitive, UDP, inherited sockets, or a complete network namespace boundary.

Run the enforcement checks on a Linux host that supports nono's required Landlock
features:

```sh
cargo test -p sail-guardian --test nono_worker
```

These checks intentionally fail rather than skip when enforcement is unavailable.
They launch a disposable worker for each operation and verify allowed reads and
writes, denied reads/writes outside grants, read-only enforcement, denied execution
of an ungranted binary, and TCP denial against an independently reachable loopback
listener. They also check environment clearing, expired/malformed/oversized request
refusal, quota/proxy refusal, sentinel preservation, and continued parent filesystem
access. They do not demonstrate integration of every SAIL execution path.

On 2026-11-02 the host enforcement checks passed. The outer coding-tool sandbox
prohibited the listener; the complete enforcement run used the host execution
context. A failed early run also caught `/dev/null` being opened after restriction;
the worker now opens its null input before enforcement without adding a filesystem
grant for that device.

## Relationship to the unified sandbox runtime

The new `sail-sandbox` crate now defines the typed `ExecutionPlan`, checked policy profiles, the
default-deny network policy and the CONNECT broker core for issue #379. See
[`SANDBOX_RUNTIME.md`](SANDBOX_RUNTIME.md). This worker remains an **optional additive**
Landlock/nono layer; it does not replace the mandatory Bubblewrap/runner boundary and is not yet
wired into every execution path. Do not route network or credential-bearing operations through
this worker.
