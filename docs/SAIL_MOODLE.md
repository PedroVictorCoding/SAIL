# SAIL Moodle connector

Status: M0 (contracts), November 2026. Crate: `crates/sail-moodle`.

SAIL follows the owner's school Moodle (default `https://moodle26.iscte-iul.pt`). It tells
them what changed (new or updated files, moved deadlines, announcements, grades), keeps a
local copy of course files, and pulls out the dates that matter (exams, tests, deliveries)
and how each course is graded. A local model does the reading. Deterministic code checks
every claim it makes.

## What the default site allows (probed 2026-11-08)

`tool_mobile_get_public_config` on `moodle26.iscte-iul.pt` reports:

- `enablewebservices: 1`, `enablemobilewebservice: 1`: the mobile web service is on.
- `typeoflogin: 2` with one identity provider, `login.iscte-iul.pt` (SAML2). Sign-in goes
  through the browser, so SAIL can't sign in with a username and password and doesn't try to.
- `launchurl: /admin/tool/mobile/launch.php`: the mobile SSO launch is available.

## Decisions

1. **Read-only.** Only `core_*`/`mod_*`/`gradereport_*` *get* functions are called.
   Submitting, enrolling, posting and marking things read are out of scope. The client has
   no code path that writes.
2. **Token, not password.** SAIL stores a mobile web-service token. There are two ways to
   get one:
   - **Paste the token.** Copy it from *Preferences → Security keys*
     (`/user/managetoken.php`) if the site shows one.
   - **SSO launch.** SAIL builds
     `launch.php?service=moodle_mobile_app&passport=<random>&urlscheme=<scheme>`. The owner
     opens it in their own browser and signs in through ISCTE's SSO. They then paste back
     the `<scheme>://token=<base64>` address the browser lands on. SAIL decodes
     `signature:::token[:::privatetoken]` and checks the signature, which is
     `md5(site_url + passport)`, before accepting it. The private token is thrown away.
   - The token goes in a `0600` file under the private SAIL data directory. It is never
     logged, never put into a prompt or a returned value, and redacted from every error.
     It is sent only to the configured host.
3. **Clean room.** [loyaniu/moodle-mcp](https://github.com/loyaniu/moodle-mcp) (Python, no
   license file) was read only to see which public Moodle web-service functions a student
   token can use. No code is copied. SAIL is Rust, local-first, and doesn't run a Python
   MCP server.
4. **Snapshots and diffs.** Each sync reads a full `CourseSnapshot` per course. The `sync`
   slice diffs it against the previous one into stable-id `Change`s. Diffing is pure and
   idempotent, so re-running it never duplicates a change.
5. **File mirror.** New and changed files are downloaded into a content-addressed folder
   (`files/<sha256>`). An index maps course, module, path and name to every version SAIL
   has seen. Files are never overwritten and never deleted when Moodle deletes them. A
   change to the size or modified time means a new version. Files over `MAX_FILE_BYTES`
   are recorded but not downloaded.
6. **Extraction: the model proposes, code verifies.**
   - A deterministic finder (PT and EN date forms, plus keywords such as "teste",
     "exame", "frequência", "entrega", "época") runs first.
   - The local model gets the text quoted as data, plus the finder's hits, and must answer
     in JSON. Every item it returns must include a verbatim quote of 300 characters or
     fewer, and the stated date must parse out of that quote. Otherwise the item is
     rejected and kept for review.
   - Structured Moodle dates (assignment due, calendar events) are `Confirmed` without a
     model. Two independent sources that agree make an item `Confirmed`; a lone grounded
     model claim is `Likely`, and a conflict is `Uncertain`.
7. **Local model only, background priority.** Moodle data is private (vision decision
   15). Extraction runs through Mind at `WorkPriority::Background` behind the same
   admission gate as the Course Factory, so a voice turn always wins.
8. **Polling, not push.** One sync every 30 minutes by default, with exponential backoff
   on `Unavailable`. An `Unauthorized` reply stops syncing until the owner signs in again.
9. **Untrusted input.** Course HTML, file names and file text are data. HTML is turned
   into plain text without running anything. File names are never used as paths. PDF text
   comes from the `sail-runner` sandbox (`pdftotext`), never from the daemon's own process.

## Pieces

| Area | Module | What it does |
|---|---|---|
| Contracts | `contracts.rs` | Shared types and the `MoodleApi`/`ExtractModel` ports |
| Client | `client/**` | REST calls, token sign-in parsing, bounded downloads |
| Store | `store/**` | SQLite: snapshots, changes, file index, items, sync state |
| Sync | `sync/**` | Snapshot diff, mirror plan |
| Extract | `extract/**` | HTML to text, date finder, prompt, reply validation, merge |
| Service | `service.rs` | Poll loop, orchestration, Mind adapter |
| HTTP | `sail-dialogue/src/moodle_http.rs` | `/api/moodle`, owner token |
| Tools | `sail-dialogue/src/execution.rs` | `moodle:*` read-only model tools |

## Out of scope for now

- Writing to Moodle (submitting, posting, enrolling).
- Reading files that aren't in the course contents API: forum attachments are handled,
  but embedded media and external links are not.
- Office formats (DOCX/PPTX) for extraction. These files are mirrored but not read yet.
