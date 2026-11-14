# Scheduled jobs in SAIL

SAIL can do things later or regularly: "every weekday at 9am, summarise my day", "in
30 minutes, check the build", "fridays at 5pm, follow the weekly-report skill". This is
phase P3 of the [external models, gateway, skills and scheduling plan](SAIL_EXTERNAL_MODELS_GATEWAY_SKILLS.md),
SAIL's version of Hermes Agent's cron.

## Using it

Open **SAIL Agents › Schedule**.

- **New job:** a name, **When** in plain words (a live preview shows how SAIL read it and
  the next five runs), a time zone, and **What**: ask SAIL, follow a skill, or run a Flow
  workflow. Results are shown in SAIL or kept silently in the job's history.
- **Jobs SAIL proposes:** when you ask SAIL in conversation for something regular, it can
  propose a job (`schedule_create`). The job waits under **Proposed by SAIL** with SAIL's
  reason, and runs only after you approve it.
- Each job has **Run now**, **Pause / Resume**, **Delete**, and its run history.

### What "When" understands

| You write | It means |
| --- | --- |
| `in 30m`, `in 2 hours`, `tomorrow at 9am`, `2026-11-20 09:00` | once |
| `every 15 minutes`, `every 2h`, `every day` | at that interval from when you created it (at least 5 minutes) |
| `daily at 7am`, `weekdays at 9am`, `weekends at 10:30`, `mondays at 18:30`, `mon,thu at 8pm`, `daily at 9am and 6pm` | those local times |
| `30 9 * * 1-5`, `@daily`, `@weekly`, `@monthly` | a cron expression |

Times are in the job's time zone, including across daylight-saving changes: a time that
doesn't exist that day (spring forward) runs at the next valid minute, and a time that
happens twice (fall back) runs once.

## How it runs

- **Once per slot.** Every 30 seconds SAIL checks for due jobs. A run is recorded as
  started before it begins, so a restart never runs the same slot twice. A run cut short
  by a restart is marked failed.
- **After downtime.** If SAIL was off, the latest missed slot still runs when it is at
  most 2 hours old; older slots are recorded as one "missed" entry.
- **Your attention comes first.** Nothing starts while you are talking to SAIL. Quiet
  hours (Pulse) hold the *result*, not the run: it is delivered to your inbox when quiet
  hours end. The status strip on the Schedule section says when this is happening.
- **Nobody is watching a run.** Actions that need your approval are not approved; the
  result lists what it couldn't do, so you can do it yourself. A run can't create new
  scheduled jobs.
- **Failures.** Three failed runs in a row pause the job and put a note in your inbox.

## Where things are

| What | Where |
| --- | --- |
| Jobs and run history (last 50 per job) | `$SAIL_DATA/schedule/jobs.json`, `runs.json` |
| Grammar, planner, store | `crates/sail-flow/src/schedule/` |
| Runner, delivery, model executor | `crates/sail-dialogue/src/schedule_runner.rs`, `server.rs` (`headless_turn`) |
| API (`/api/agents/schedule`, owner token) | `crates/sail-dialogue/src/schedule_http.rs` |
| UI | `crates/sail-dialogue/web/schedule.js` |

## Not yet done

- Delivery to Telegram (P2, round 22) and event triggers (a Forge job finishing, a file
  changing).
- Runs are not Continuity tasks. The job store's start record and crash recovery give
  the same once-per-slot guarantee; moving runs onto Continuity leases would let other
  subsystems see and cancel them.
