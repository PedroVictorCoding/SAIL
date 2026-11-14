//! Pure parsing of chat input: commands, skill invocations and plain text.
//!
//! Telegram command words are case-insensitive and may carry an `@BotName` suffix. A
//! leading `/` that is not one of SAIL's commands is treated as a skill when its name
//! matches an installed skill (Telegram cannot send `-` in a command, so `_` is mapped
//! back to `-`); otherwise it is [`Command::Unknown`] and the caller sends [`help_text`].
use uuid::Uuid;

/// One parsed chat message.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Start a fresh conversation for this chat.
    New,
    /// Ask the running turn to stop.
    Stop,
    /// Report gateway and job status.
    Status,
    /// List scheduled jobs.
    Jobs,
    /// List installed skills.
    Skills,
    /// Approve a pending Guardian request, optionally naming its short id.
    Approve(Option<String>),
    /// Deny a pending Guardian request, optionally naming its short id.
    Deny(Option<String>),
    /// Run an installed skill with optional input.
    Skill { name: String, input: String },
    /// `/something` that is neither a command nor a skill.
    Unknown(String),
    /// Not a command.
    Text(String),
}

/// Pure. Parses one message. Leading/trailing whitespace is ignored; empty → `Text("")`.
pub fn parse(text: &str, skills: &[String]) -> Command {
    let text = text.trim();
    if text.is_empty() {
        return Command::Text(String::new());
    }
    let Some(rest) = text.strip_prefix('/') else {
        return Command::Text(text.to_string());
    };
    let (head, arguments) = match rest.split_once(char::is_whitespace) {
        Some((head, arguments)) => (head, arguments.trim()),
        None => (rest, ""),
    };
    let command_word = head.split_once('@').map_or(head, |(word, _)| word);
    if command_word.is_empty() {
        return Command::Unknown(String::new());
    }
    match command_word.to_ascii_lowercase().as_str() {
        "new" => Command::New,
        "stop" => Command::Stop,
        "status" => Command::Status,
        "jobs" => Command::Jobs,
        "skills" => Command::Skills,
        "approve" => Command::Approve(first_token(arguments)),
        "deny" => Command::Deny(first_token(arguments)),
        _ => {
            let normalized = command_word.replace('_', "-");
            let skill = skills
                .iter()
                .find(|skill| skill.eq_ignore_ascii_case(&normalized));
            match skill {
                Some(name) => Command::Skill {
                    name: name.clone(),
                    input: arguments.to_string(),
                },
                None => Command::Unknown(command_word.to_string()),
            }
        }
    }
}

fn first_token(arguments: &str) -> Option<String> {
    arguments
        .split_whitespace()
        .next()
        .map(|token| token.to_string())
}

/// Pure. Short, stable approval ids shown on chat cards: the first 6 hex chars of the
/// approval session's UUID (simple form).
pub fn short_id(session: &Uuid) -> String {
    session.simple().to_string().chars().take(6).collect()
}

/// Pure. Compares a card's short id with an approval session, case-insensitively.
pub fn matches(short: &str, session: &Uuid) -> bool {
    short.eq_ignore_ascii_case(&short_id(session))
}

/// Pure. The `/help` and unknown-command text, in one short message.
pub fn help_text() -> String {
    [
        "SAIL commands:",
        "/new — start a new conversation",
        "/stop — stop the current turn",
        "/status — connection and job status",
        "/jobs — list scheduled jobs",
        "/skills — list installed skills",
        "/approve [id] — approve a request",
        "/deny [id] — deny a request",
        "/<skill> [input] — run a skill",
    ]
    .join("\n")
}
