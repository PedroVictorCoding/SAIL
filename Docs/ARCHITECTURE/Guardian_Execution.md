# Guardian execution and tool scheduling

This implementation addresses the execution-boundary gaps in #30–#32 and adds
initial services/contracts for #43 and #44. It does not certify the deployment or
close those issues: the target Ubuntu host still needs namespace/cgroup escape
checks, credential key lifecycle review, and Android approval integration.

## Workspace filesystem access (#30)

`SAIL_TOOL_ROOT` is read when Guardian starts (default: its working directory).
There are no implicit `/home/main/SAIL`, credential-store or `/tmp` grants.
`ConfinedRoot` pins the configured directory descriptor. Linux `openat2` enforces
`RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV`;
unsupported kernels fail closed. Special files and hardlinked regular files are
rejected. Private stores such as `.git`, `.ssh`, `.aws`, `.local` and `.env` are
excluded even if nested under the workspace. An administrator must configure a
root containing only permitted workspace data, separate from production secrets.

The filesystem adapter receives the authorized descriptor. It obtains metadata,
bounded directory entries and bounded regular-file contents from that descriptor,
never by reopening a previously checked caller pathname. Legacy `validate_path`
is an advisory compatibility API, not an execution permit. The generic broker
rejects filesystem callbacks and all host CLI commands. Filesystem writes and
exports need separate reviewed operations; command-name allowlists cannot enable
host shell execution.

## Owner approvals (#31)

Every non-low-risk action requires an explicit trusted grant or a single-use
approval. There is no implicit Core/Dialogue/Bridge medium-risk allowance and no
camera grant at boot. Native grants also expire within 60 seconds and bind the originating correlation. Token validation, consumption and revocation share one
mutex, validate before consuming, bind exact capability plus task correlation,
and expire after 60 seconds. Native administrative APIs are trusted Rust code;
EventBus source IDs are not authentication, so `UserApprovalResolved` events are
ignored.

Set `SAIL_APPROVAL_TOKEN` to a securely generated owner token of at least 32 bytes
in the daemon's protected service configuration. If missing, owner approval HTTP
routes are disabled; invalid/short configuration fails startup. Use HTTPS for LAN
clients. Settings → Owner approvals accepts the token in memory only. It lists
pending actor/resource/risk/expiry, permits deny/revoke, and retries a remembered
local tool call once with its exact grant. Approval is not itself execution.
Tokens and pending calls are cleared when the page or voice connection closes.
Server-created session and turn correlations prevent use in another connection
or after the turn changes. Requests created by other callers cannot be retried
from this browser; native/mobile callers must implement their own authenticated
approval transport using the same contract. A running side effect cannot be undone
by revocation; cancellation must be supported by the adapter.

Endpoints: GET `/api/guardian/approvals`, POST `/api/guardian/resolve`
`{session_id, approved}`, POST `/api/guardian/revoke` `{session_id}`. All require
Bearer owner authentication and return no-store responses. Do not put owner
tokens into URLs, model context, Git or logs. Generated SVG is rendered in an
image document, never inserted as active markup into the owner page.

## Credential isolation (#32)

With `SAIL_VAULT_DIR`, Guardian opens an AES-256-GCM persistent vault or fails
startup. Without it, credentials are ephemeral. The directory must be owner-owned
0700, with private regular single-link 0600 key/data files; symlinks are refused.
Writes use unique exclusive temporary files, fsync and atomic rename. The key is
protected by Unix ownership/mode, not a hardware keystore; its backup, rotation
and service-user isolation remain deployment responsibilities.

Raw getter and prompt-substring leakage APIs are removed. Only trusted Bridge
connectors can obtain/use exact-scope opaque handles. Every use checks the
server-issued handle record, expiry, caller and revocation. Rotation, overwrite,
revocation and recreation invalidate old handles. Secrets/keys/transient plaintext
buffers use zeroizing storage. Connector callbacks are trusted code and must not
return credentials; arbitrary code in the daemon address space is outside this
boundary.

Structured diagnostics, observations and audit fields redact registered secrets
before truncation/serialization. Known credential field names are also removed.
This is defense in depth; raw secrets must not be passed to model/tool arguments.

## Dedicated command service (#43)

`sail-runner` is a separate socket-activated executable, never an agent command
spawned as the SAIL daemon user. Its peer must have the configured daemon UID;
the service must use a different non-root runner UID. The protected Unix socket
and service installation are administered outside the assistant. See
`deploy/runner/` for installation templates; do not activate them without a host
review. They require systemd support for `DelegateSubgroup` and delegated cgroup
v2 CPU/memory/PID controllers. Provision an immutable root-owned minimal image
and all its ancestors without group/world write access. Include only approved
utilities and dependencies, add a root-owned `.sail-runner-image-v1` containing
`SAIL isolated root image v1` followed by a newline, and never include SAIL production binaries, source, credentials,
models or databases. This patch does not download or install an image.

Each request enters its own cgroup before exec, then Bubblewrap unshares user,
mount, PID, IPC, network, UTS and cgroup namespaces, disables nested user
namespaces, drops capabilities, clears the environment and mounts the image
read-only. Only fresh bounded `/work` and `/tmp` tmpfs, private `/proc` and a
minimal `/dev` are added. No host workspace, home, socket, process namespace or
network is shared. Cgroup memory/swap/PID/CPU limits and rlimit CPU/fd/file-size
limits are mandatory. No namespace, identity, image or cgroup fallback exists.

The v1 `CommandPlan` includes executable/argv, request/task IDs and bounded
budgets, not host mounts or an arbitrary environment. Guardian approvals bind a
SHA-256 digest of the entire request, alongside its exact plan for owner review. The daemon verifies the service peer UID and shuts down the socket if a consumed grant is revoked. The service reconciles abandoned command cgroups on restart and limits concurrent jobs and wire
frames, bounds output, kills the entire command cgroup on cancellation, output
flood, timeout or exit, and uses parent-death cleanup. Disconnect cancels a job.
Only isolated execution is implemented: reviewed artifact export, input staging,
PTYs, network grants and privileged administrative helpers remain unavailable.
There is no web/mobile shell endpoint. A privileged host deployment test is still
required to demonstrate production-file hash invariance, mount/network escape
rejection, resource exhaustion handling and crash cleanup on the actual machine.

## Tool registry/scheduler (#44)

Trusted adapters register versioned descriptors. V1 uses a bounded JSON Schema
subset: closed objects, required fields, bounded strings/arrays, integers,
booleans, null and enums. Unknown keywords/references fail registration; it does
not claim full JSON Schema support. Input and output validation happen around the
adapter. A lexical fast path returns at most eight relevant schemas within a
16 KiB maximum budget, without a model preselection call.

Every invocation is authorized by Guardian against exact tool ID, canonical JSON
arguments and originating correlation. Registered requirements must match that
invocation; descriptors cannot invent exemptions. Global, background, resource
class and per-tool semaphores bound concurrency; mutating actions serialize and
never automatically retry. Only declared read-only operations may retry transient
failures (maximum three attempts). Deadlines include queuing; cancellation drops
pending work and signals the backend. Adapters must honor cancellation when they
spawn their own work. Request IDs cache terminal observations and reject
conflicting/in-flight duplicates for five minutes. Dependency plans reject cycles
before any dispatch and skip dependents of failed operations.

Sequenced typed events distinguish queued, policy-pending, executing, completed,
failed and cancelled operations. Outputs above 4 KiB become expiring,
correlation-scoped references in an 8 MiB memory store; output above 1 MiB is
rejected. Bridge uses this scheduler and registers an actual local clock backend;
unregistered tools report unavailable rather than fabricated success. Web/data,
Canvas, book playback and command-runner adapters, full Dialogue turn coordination,
and Android event consumers remain follow-up integrations.
