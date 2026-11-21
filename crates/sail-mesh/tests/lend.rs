//! Pure lending decisions, window arithmetic and `lend.json` persistence.
use chrono::{DateTime, Utc};
use sail_mesh::compute::lend::{
    LendInputs, decide, load_mode, next_window_start, parse_duration, parse_window, save_mode,
    window_end,
};
use sail_protocol::mesh::{LendMode, LendState, LendWindow, MeshBusy};
use std::time::Duration;

fn window(text: &str) -> LendWindow {
    parse_window(text).expect("test window")
}

fn local(text: &str) -> chrono::NaiveDateTime {
    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").expect("test local time")
}

fn utc(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .expect("test UTC time")
        .with_timezone(&Utc)
}

fn busy(full: bool) -> MeshBusy {
    MeshBusy {
        reason: if full {
            "making music (REAPER)".into()
        } else {
            "other programs use 70% CPU".into()
        },
        full,
        retry_after_secs: (!full).then_some(60),
    }
}

fn run(
    mode: LendMode,
    now_local: &str,
    now_utc: &str,
    busy: Option<&MeshBusy>,
    rpc_up: bool,
    rpc_error: Option<&str>,
) -> LendState {
    let window = window("01:00-08:00");
    let inputs = LendInputs {
        mode: &mode,
        window: &window,
        local_now: local(now_local),
        utc_now: utc(now_utc),
        busy,
        rpc_up,
        rpc_error,
    };
    let first = decide(&inputs);
    let second = decide(&inputs);
    assert_eq!(first, second, "decide is pure");
    first
}

#[test]
fn rules_apply_in_order_and_are_pure() {
    let night = ("2026-11-21T02:00:00", "2026-11-21T00:00:00Z");
    assert_eq!(
        run(
            LendMode::Pause,
            night.0,
            night.1,
            Some(&busy(true)),
            true,
            None
        ),
        LendState::Paused,
        "pause beats the rest rules"
    );
    assert_eq!(
        run(
            LendMode::Play {
                until: Some("2026-11-21T08:00:00Z".into())
            },
            night.0,
            night.1,
            Some(&busy(true)),
            true,
            None
        ),
        LendState::Resting {
            reason: "making music (REAPER)".into(),
            full: true
        },
        "a game or music app ends lending even in Play"
    );
    assert_eq!(
        run(
            LendMode::Play { until: None },
            "2026-11-21T12:00:00",
            "2026-11-21T10:00:00Z",
            None,
            true,
            None
        ),
        LendState::Lending,
        "Play lends outside the window"
    );
    assert_eq!(
        run(
            LendMode::Play {
                until: Some("2026-11-21T00:00:00Z".into())
            },
            "2026-11-21T12:00:00",
            "2026-11-21T10:00:00Z",
            None,
            true,
            None
        ),
        LendState::OutsideWindow {
            next_start: "2026-11-21T23:00:00Z".into()
        },
        "an expired Play is evaluated as Auto"
    );
    assert_eq!(
        run(
            LendMode::Play {
                until: Some("not a time".into())
            },
            "2026-11-21T12:00:00",
            "2026-11-21T10:00:00Z",
            None,
            true,
            None
        ),
        LendState::OutsideWindow {
            next_start: "2026-11-21T23:00:00Z".into()
        },
        "an unreadable Play never extends lending"
    );
    assert_eq!(
        run(
            LendMode::Auto,
            night.0,
            night.1,
            Some(&busy(false)),
            true,
            None
        ),
        LendState::Resting {
            reason: "other programs use 70% CPU".into(),
            full: false
        }
    );
    assert_eq!(
        run(
            LendMode::Auto,
            night.0,
            night.1,
            None,
            true,
            Some("ggml-rpc-server exited (code 1)")
        ),
        LendState::Unavailable {
            reason: "ggml-rpc-server exited (code 1)".into()
        }
    );
    assert_eq!(
        run(LendMode::Auto, night.0, night.1, None, false, None),
        LendState::Loading
    );
    assert_eq!(
        run(LendMode::Auto, night.0, night.1, None, true, None),
        LendState::Lending
    );
    assert_eq!(
        run(
            LendMode::Auto,
            "2026-11-21T12:00:00",
            "2026-11-21T10:00:00Z",
            None,
            true,
            None
        ),
        LendState::OutsideWindow {
            next_start: "2026-11-21T23:00:00Z".into()
        }
    );
}

#[test]
fn next_start_respects_a_non_utc_local_clock() {
    let window = window("01:00-08:00");
    assert_eq!(
        next_window_start(
            &window,
            local("2026-11-21T12:00:00"),
            utc("2026-11-21T10:00:00Z")
        ),
        utc("2026-11-21T23:00:00Z"),
        "next 01:00 local, two hours ahead of UTC"
    );
    assert_eq!(
        next_window_start(
            &window,
            local("2026-11-21T01:00:00"),
            utc("2026-11-20T23:00:00Z")
        ),
        utc("2026-11-21T23:00:00Z"),
        "strictly after the exact start, the next day"
    );
    assert_eq!(
        next_window_start(
            &window,
            local("2026-11-21T00:30:00"),
            utc("2026-11-20T22:30:00Z")
        ),
        utc("2026-11-20T23:00:00Z"),
        "before the window, it starts today"
    );
}

#[test]
fn a_window_wrapping_midnight_and_an_always_window() {
    let wrap = window("22:00-07:00");
    assert_eq!(
        window_end(
            &wrap,
            local("2026-11-21T23:00:00"),
            utc("2026-11-21T21:00:00Z")
        ),
        Some(utc("2026-11-22T05:00:00Z")),
        "the end is tomorrow morning"
    );
    assert_eq!(
        window_end(
            &wrap,
            local("2026-11-21T03:00:00"),
            utc("2026-11-21T01:00:00Z")
        ),
        Some(utc("2026-11-21T05:00:00Z")),
        "the end is this morning"
    );
    assert_eq!(
        window_end(
            &wrap,
            local("2026-11-21T12:00:00"),
            utc("2026-11-21T10:00:00Z")
        ),
        None
    );
    assert_eq!(
        next_window_start(
            &wrap,
            local("2026-11-21T23:00:00"),
            utc("2026-11-21T21:00:00Z")
        ),
        utc("2026-11-22T20:00:00Z"),
        "inside the wrapped window, the next start is tomorrow evening"
    );
    assert_eq!(
        next_window_start(
            &wrap,
            local("2026-11-21T12:00:00"),
            utc("2026-11-21T10:00:00Z")
        ),
        utc("2026-11-21T20:00:00Z"),
        "outside it, the next start is this evening"
    );

    let always = window("01:00-01:00");
    assert_eq!(
        window_end(
            &always,
            local("2026-11-21T12:00:00"),
            utc("2026-11-21T10:00:00Z")
        ),
        None,
        "an always-open window has no end"
    );
    assert!(matches!(
        decide(&LendInputs {
            mode: &LendMode::Auto,
            window: &always,
            local_now: local("2026-11-21T12:00:00"),
            utc_now: utc("2026-11-21T10:00:00Z"),
            busy: None,
            rpc_up: true,
            rpc_error: None,
        }),
        LendState::Lending
    ));
}

#[test]
fn malformed_windows_are_refused_without_panicking() {
    for text in [
        "",
        "01:00",
        "01:00-",
        "-08:00",
        "24:00-08:00",
        "01:60-08:00",
        "1:2:3-08:00",
        "aa:bb-cc:dd",
        "01:00-08:00-09:00",
        "+1:00-08:00",
    ] {
        assert!(parse_window(text).is_err(), "{text:?} is not a window");
    }
    assert_eq!(window("1:00-8:00").start, "01:00");
    assert_eq!(window("1:00-8:00").end, "08:00");
}

#[test]
fn durations_accept_the_forms_the_cli_documents() {
    assert_eq!(parse_duration("3h").unwrap(), Duration::from_secs(3 * 3600));
    assert_eq!(parse_duration("90m").unwrap(), Duration::from_secs(90 * 60));
    assert_eq!(
        parse_duration("1h30m").unwrap(),
        Duration::from_secs(90 * 60)
    );
    assert_eq!(
        parse_duration(" 1h 30m ").unwrap(),
        Duration::from_secs(90 * 60)
    );
    assert_eq!(parse_duration("45M").unwrap(), Duration::from_secs(45 * 60));
    for text in [
        "", "3", "h", "3x", "0h", "0m", "-1h", "1h30", "1.5h", "h30m",
    ] {
        assert!(parse_duration(text).is_err(), "{text:?} is not a duration");
    }
}

#[test]
fn mode_file_round_trips_and_hostile_input_means_auto() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("lend.json");
    assert_eq!(load_mode(&path), LendMode::Auto, "missing means Auto");
    for mode in [
        LendMode::Auto,
        LendMode::Pause,
        LendMode::Play { until: None },
        LendMode::Play {
            until: Some("2026-11-22T08:00:00Z".into()),
        },
    ] {
        save_mode(&path, &mode).expect("save mode");
        assert_eq!(load_mode(&path), mode);
    }
    for hostile in [
        "",
        "{",
        "null",
        "[]",
        r#"{"mode":"play","until":42}"#,
        r#"{"mode":"dance"}"#,
    ] {
        std::fs::write(&path, hostile).expect("write hostile mode");
        assert_eq!(load_mode(&path), LendMode::Auto, "{hostile:?}");
    }
}
