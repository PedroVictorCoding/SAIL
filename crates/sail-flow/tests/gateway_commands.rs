use sail_flow::gateway::commands::{Command, help_text, matches, parse, short_id};
use uuid::Uuid;

fn skills() -> Vec<String> {
    vec!["weekly-report".to_string(), "summarize".to_string()]
}

#[test]
fn command_table() {
    let table: &[(&str, Command)] = &[
        ("", Command::Text(String::new())),
        ("   ", Command::Text(String::new())),
        ("hello", Command::Text("hello".to_string())),
        ("  hello  ", Command::Text("hello".to_string())),
        ("/new", Command::New),
        ("/NEW", Command::New),
        ("/new@SailBot", Command::New),
        ("/new now", Command::New),
        ("/stop", Command::Stop),
        ("/status", Command::Status),
        ("/jobs", Command::Jobs),
        ("/skills", Command::Skills),
        ("/approve", Command::Approve(None)),
        (
            "/approve a1b2c3",
            Command::Approve(Some("a1b2c3".to_string())),
        ),
        (
            "/APPROVE@SailBot a1b2c3",
            Command::Approve(Some("a1b2c3".to_string())),
        ),
        ("/deny", Command::Deny(None)),
        (
            "/deny@SailBot 0f0f0f",
            Command::Deny(Some("0f0f0f".to_string())),
        ),
        (
            "/weekly-report",
            Command::Skill {
                name: "weekly-report".to_string(),
                input: String::new(),
            },
        ),
        (
            "/weekly_report",
            Command::Skill {
                name: "weekly-report".to_string(),
                input: String::new(),
            },
        ),
        (
            "/weekly_report last week",
            Command::Skill {
                name: "weekly-report".to_string(),
                input: "last week".to_string(),
            },
        ),
        (
            "/Weekly_Report  last   week ",
            Command::Skill {
                name: "weekly-report".to_string(),
                input: "last   week".to_string(),
            },
        ),
        (
            "/summarize this",
            Command::Skill {
                name: "summarize".to_string(),
                input: "this".to_string(),
            },
        ),
        ("/unknown", Command::Unknown("unknown".to_string())),
        ("/unknown@Bot arg", Command::Unknown("unknown".to_string())),
        ("/help", Command::Unknown("help".to_string())),
        ("/", Command::Unknown(String::new())),
        ("/@Bot", Command::Unknown(String::new())),
        ("/new_thing", Command::Unknown("new_thing".to_string())),
        (" /new ", Command::New),
    ];
    for (text, expected) in table {
        assert_eq!(&parse(text, &skills()), expected, "input: {text:?}");
    }
}

#[test]
fn unknown_skill_when_none_installed() {
    assert_eq!(
        parse("/weekly_report", &[]),
        Command::Unknown("weekly_report".to_string())
    );
    assert_eq!(
        parse("/summarize", &[]),
        Command::Unknown("summarize".to_string())
    );
}

#[test]
fn parse_is_pure() {
    let first = parse("/Weekly_Report last week", &skills());
    let second = parse("/Weekly_Report last week", &skills());
    assert_eq!(first, second);
}

#[test]
fn short_ids_match_case_insensitively() {
    let session = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").expect("uuid");
    assert_eq!(short_id(&session), "550e84");
    assert!(matches("550e84", &session));
    assert!(matches("550E84", &session));
    assert!(!matches("550e85", &session));
    assert!(!matches("550e8400", &session));
    assert_eq!(short_id(&session), short_id(&session));
}

#[test]
fn help_lists_every_command() {
    let help = help_text();
    for command in [
        "/new", "/stop", "/status", "/jobs", "/skills", "/approve", "/deny", "/<skill>",
    ] {
        assert!(help.contains(command), "help is missing {command}");
    }
    assert!(help.len() < 4096);
    assert_eq!(help, help_text());
}
