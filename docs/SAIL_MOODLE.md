# SAIL Moodle connector

Status: round 1 integrated, November 2026. Crate: `crates/sail-moodle`. UI: `/moodle` (sidebar **School**).

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
     `launch.php?service=moodle_mobile_app&passport=<random>&urlscheme=sailmoodle&confirmed=1`.
     The owner opens it in their own browser and signs in through ISCTE's SSO.
     `confirmed=1` makes Moodle end on a page with a "launch the app" link instead of a
     bare redirect, which desktop browsers drop silently. Moodle carries the flag across
     SSO in its `tool_mobile_launch` cookie. The owner copies that link's address
     (`sailmoodle://token=<base64>`) back into SAIL. SAIL decodes
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
8. **Conservative use of the school's server.** Every request goes through `pacer.rs`:
   - **Pacing:**
     - one request at a time, at least 1.5 s apart;
     - at most 250 requests per sync and 1,200 per UTC day, counted in the store so a
       restart doesn't reset them;
     - at most 1 GiB of downloads per day.

     A sync that runs out of budget stops cleanly and resumes next time.
   - **Scheduling:**
     - a sync every 3 h by default (`SAIL_MOODLE_SYNC_MINUTES`, never under 60);
     - exponential backoff on failure, up to 6 h;
     - manual "Check now" refused within 10 min of the last attempt;
     - `Unauthorized` stops syncing until the owner signs in again.
   - **What a sync reads:**
     - Assignments, events and announcements are bulk calls covering all courses at once.
     - Grades are re-read at most once a day per course.
     - Submission status is read only for assignments due within the last 14 days or the
       next 90 days, at most 40 per sync.
     - Courses that ended more than 14 days ago are re-read weekly, and their files aren't
       mirrored.
     - At most 40 downloads per sync, smallest first.

   A steady-state sync of a typical semester (about 8 courses) is roughly 30 requests,
   which is about 240 a day. The official Moodle app makes more than that each time it
   opens.
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
| Service | `service.rs`, `pacer.rs`, `credentials.rs` | Poll loop, orchestration, request budgets, token file |
| HTTP | `sail-dialogue/src/moodle_http.rs` | `/api/moodle`, owner token |
| Tools | `sail-dialogue/src/moodle_tools.rs` | `moodle:upcoming`, `moodle:changes`, `moodle:course`, `moodle:search` (read-only, answer from the last sync) |
| UI | `sail-dialogue/web/moodle.{html,js,css}` | Sign-in, courses, dated items with their source quotes, changes, grading, files, usage |

## Known limits

- Model-extracted items whose page text later changes are dropped with it. Items from
  files are kept, because a file is read once per content hash.
- PDFs are read only through the `sail-runner` sandbox, and only after the owner allows it
  once (**Reads PDFs → Allow** on the School page).
  - **Install:** `cargo build --release -p sail-runner`, then
    `sudo scripts/setup-sail-runner.sh --user <you>`. Start SAIL again from a new login
    shell; `run.sh` sets `SAIL_RUNNER_SOCKET`/`SAIL_RUNNER_UID` when the socket is
    reachable.
  - **Permission:** Allow stores one Guardian standing rule for the fixed `pdf-text-v1`
    operation. It is listed and revocable under Guardian's rules and covers Course
    Factory PDFs too.
  - **Limits:**
    - without the rule, syncs never attempt a PDF, so no approval prompts appear;
    - at most 15 PDFs per sync and the first 40 pages of each;
    - each file is read once per content hash;
    - right after Allow, the files already mirrored are read locally, with no Moodle
      requests.
  - Without the sandbox, only text and HTML files are read.
- Forum attachments carry the forum instance id as `module_id`, because the discussions
  API has no course-module id.

## Out of scope for now

- Writing to Moodle (submitting, posting, enrolling).
- Reading files that aren't in the course contents API: forum attachments are handled,
  but embedded media and external links are not.
- Office formats (DOCX/PPTX) for extraction. These files are mirrored but not read yet.
