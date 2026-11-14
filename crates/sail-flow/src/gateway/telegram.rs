//! The Telegram Bot API, behind a trait, plus pure parsing of its JSON.
//!
//! Nothing in this module opens a socket. Production code implements [`BotApi`] with an
//! HTTP client; tests and the demo use [`FakeBot`], which records calls and replays queued
//! responses. Parsing is deliberately tolerant of unknown fields and drops anything SAIL
//! does not handle, so a Telegram schema addition cannot break the gateway.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Mutex;

/// The bot token. Never printed: Debug/Display show `BotToken(…)` and it is not Serialize.
#[derive(Clone, PartialEq, Eq)]
pub struct BotToken(String);

impl BotToken {
    /// Telegram's shape: `<digits>:<35+ chars of [A-Za-z0-9_-]>`. Anything else → Err.
    pub fn parse(text: &str) -> Result<BotToken, String> {
        let (digits, secret) = text
            .split_once(':')
            .ok_or_else(|| "bot token must look like <digits>:<secret>".to_string())?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("bot token must start with its numeric id".to_string());
        }
        if secret.len() < 35
            || !secret
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err("bot token secret has the wrong shape".to_string());
        }
        Ok(BotToken(text.to_string()))
    }

    /// For the HTTP client only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BotToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BotToken(…)")
    }
}

impl fmt::Display for BotToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BotToken(…)")
    }
}

/// One incoming update, reduced to what SAIL uses. Everything else is dropped by [`parse_updates`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Incoming {
    /// Telegram's monotonic update id.
    pub update_id: i64,
    /// The private chat the message belongs to.
    pub chat_id: i64,
    /// Telegram user id of the sender.
    pub sender_id: i64,
    /// Display name for the pairing request: first + last name, else @username, else "Telegram user"; ≤ 64 chars.
    pub sender_name: String,
    /// The message (or callback message) id.
    pub message_id: i64,
    /// What kind of input this is.
    pub kind: IncomingKind,
    /// Telegram's Unix timestamp for the message.
    pub date: i64,
}

/// The kinds of input SAIL handles, plus a marker for the ones it cannot read yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IncomingKind {
    /// Plain text, cut to Telegram's 4096-character limit on a char boundary.
    Text { text: String },
    /// A voice note or audio file; SAIL transcribes it locally.
    Voice {
        file_id: String,
        duration_secs: u32,
        mime: String,
    },
    /// Callback from an inline button (approval cards): `data` ≤ 64 bytes.
    Button { callback_id: String, data: String },
    /// Photos, stickers, files, edits…: SAIL replies that it can't read these yet.
    Unsupported { what: String },
}

/// Pure. Parses a `getUpdates` response body (`{"ok":true,"result":[…]}`). Only private
/// chats (`chat.type == "private"`) are kept; group, channel and bot senders are dropped.
/// `{"ok":false,…}` → Err with Telegram's `description`. Returns the updates and the
/// next offset (max update_id + 1, or the given offset when there are none).
pub fn parse_updates(body: &[u8], offset: i64) -> Result<(Vec<Incoming>, i64), String> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| format!("invalid Telegram response: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "invalid Telegram response: expected an object".to_string())?;
    match object.get("ok").and_then(Value::as_bool) {
        Some(true) => {}
        Some(false) => {
            return Err(description(object).to_string());
        }
        None => return Err("invalid Telegram response: missing ok".to_string()),
    }
    let result = object
        .get("result")
        .and_then(Value::as_array)
        .ok_or_else(|| "invalid Telegram response: result is not a list".to_string())?;
    let mut updates = Vec::new();
    let mut next = offset;
    for update in result {
        if let Some(update_id) = update.get("update_id").and_then(Value::as_i64) {
            next = next.max(update_id.saturating_add(1));
        }
        if let Some(incoming) = parse_update(update) {
            updates.push(incoming);
        }
    }
    Ok((updates, next))
}

fn description(object: &Map<String, Value>) -> &str {
    object
        .get("description")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .unwrap_or("Telegram request failed")
}

fn parse_update(update: &Value) -> Option<Incoming> {
    let update_id = update.get("update_id").and_then(Value::as_i64)?;
    if let Some(query) = update.get("callback_query") {
        return parse_callback(query, update_id);
    }
    let message = update.get("message")?.as_object()?;
    parse_message(message, update_id)
}

fn parse_callback(query: &Value, update_id: i64) -> Option<Incoming> {
    let query = query.as_object()?;
    let (sender_id, sender_name) = sender(query.get("from")?)?;
    let message = query.get("message")?.as_object()?;
    let chat = message.get("chat")?.as_object()?;
    if !is_private(chat) {
        return None;
    }
    let chat_id = chat.get("id").and_then(Value::as_i64)?;
    let message_id = message.get("message_id").and_then(Value::as_i64)?;
    let callback_id = query.get("id").and_then(Value::as_str)?.to_string();
    let data = cut_bytes(query.get("data").and_then(Value::as_str).unwrap_or(""), 64);
    let date = message.get("date").and_then(Value::as_i64).unwrap_or(0);
    Some(Incoming {
        update_id,
        chat_id,
        sender_id,
        sender_name,
        message_id,
        kind: IncomingKind::Button { callback_id, data },
        date,
    })
}

fn parse_message(message: &Map<String, Value>, update_id: i64) -> Option<Incoming> {
    let (sender_id, sender_name) = sender(message.get("from")?)?;
    let chat = message.get("chat")?.as_object()?;
    if !is_private(chat) {
        return None;
    }
    let chat_id = chat.get("id").and_then(Value::as_i64)?;
    let message_id = message.get("message_id").and_then(Value::as_i64)?;
    let date = message.get("date").and_then(Value::as_i64).unwrap_or(0);
    let kind = if let Some(text) = message.get("text").and_then(Value::as_str) {
        IncomingKind::Text {
            text: cut_chars(text, 4096),
        }
    } else if let Some(kind) = voice_kind(message) {
        kind
    } else {
        IncomingKind::Unsupported {
            what: unsupported_what(message),
        }
    };
    Some(Incoming {
        update_id,
        chat_id,
        sender_id,
        sender_name,
        message_id,
        kind,
        date,
    })
}

fn sender(from: &Value) -> Option<(i64, String)> {
    let from = from.as_object()?;
    if from.get("is_bot").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let sender_id = from.get("id").and_then(Value::as_i64)?;
    let first = from.get("first_name").and_then(Value::as_str).unwrap_or("");
    let last = from.get("last_name").and_then(Value::as_str).unwrap_or("");
    let name = if first.trim().is_empty() && last.trim().is_empty() {
        match from.get("username").and_then(Value::as_str) {
            Some(username) if !username.trim().is_empty() => {
                format!("@{}", username.trim().trim_start_matches('@'))
            }
            _ => "Telegram user".to_string(),
        }
    } else {
        format!("{} {}", first.trim(), last.trim())
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    Some((sender_id, cut_chars(&name, 64)))
}

fn is_private(chat: &Map<String, Value>) -> bool {
    chat.get("type").and_then(Value::as_str) == Some("private")
}

fn voice_kind(message: &Map<String, Value>) -> Option<IncomingKind> {
    for key in ["voice", "audio"] {
        let Some(media) = message.get(key).and_then(Value::as_object) else {
            continue;
        };
        let Some(file_id) = media
            .get("file_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            return Some(IncomingKind::Unsupported {
                what: "voice".to_string(),
            });
        };
        let duration_secs = media
            .get("duration")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32;
        let mime = media
            .get("mime_type")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("audio/ogg")
            .to_string();
        return Some(IncomingKind::Voice {
            file_id: file_id.to_string(),
            duration_secs,
            mime,
        });
    }
    None
}

fn unsupported_what(message: &Map<String, Value>) -> String {
    const KINDS: &[(&str, &str)] = &[
        ("photo", "photo"),
        ("sticker", "sticker"),
        ("document", "file"),
        ("video", "video"),
        ("video_note", "video"),
        ("animation", "video"),
        ("location", "location"),
        ("venue", "location"),
        ("contact", "contact"),
        ("poll", "poll"),
        ("dice", "dice"),
        ("game", "game"),
        ("invoice", "invoice"),
        ("successful_payment", "payment"),
        ("passport_data", "passport"),
    ];
    for (key, what) in KINDS {
        if message.get(*key).is_some() {
            return (*what).to_string();
        }
    }
    "unsupported".to_string()
}

fn cut_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

fn cut_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// One outgoing call. Pure builders: method name + JSON body. Long texts are split by
/// [`crate::gateway::text::chunks`] before they get here.
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    /// Send a text message, optionally replying to another and carrying inline buttons.
    Send {
        chat_id: i64,
        text: String,
        reply_to: Option<i64>,
        buttons: Vec<(String, String)>,
    },
    /// Show Telegram's typing indicator.
    Typing { chat_id: i64 },
    /// Answer an inline button callback.
    AnswerButton { callback_id: String, text: String },
    /// Replace the card's text and remove its buttons.
    EditButtons {
        chat_id: i64,
        message_id: i64,
        text: String,
    },
}

/// Pure. Builds the Bot API method and JSON body for one outgoing call.
pub fn request(out: &Outgoing) -> (&'static str, Value) {
    match out {
        Outgoing::Send {
            chat_id,
            text,
            reply_to,
            buttons,
        } => {
            let mut body = json!({"chat_id": chat_id, "text": text});
            if let Some(map) = body.as_object_mut() {
                if let Some(reply_to) = reply_to {
                    map.insert("reply_to_message_id".to_string(), json!(reply_to));
                }
                if !buttons.is_empty() {
                    let row: Vec<Value> = buttons
                        .iter()
                        .map(|(label, data)| json!({"text": label, "callback_data": data}))
                        .collect();
                    map.insert(
                        "reply_markup".to_string(),
                        json!({"inline_keyboard": [row]}),
                    );
                }
            }
            ("sendMessage", body)
        }
        Outgoing::Typing { chat_id } => (
            "sendChatAction",
            json!({"chat_id": chat_id, "action": "typing"}),
        ),
        Outgoing::AnswerButton { callback_id, text } => (
            "answerCallbackQuery",
            json!({"callback_query_id": callback_id, "text": text}),
        ),
        Outgoing::EditButtons {
            chat_id,
            message_id,
            text,
        } => (
            "editMessageText",
            json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "text": text,
                "reply_markup": {"inline_keyboard": []},
            }),
        ),
    }
}

/// Pure. `{"ok":true,"result":{"message_id":…}}` → Some(message_id); ok without one → None; ok=false → Err(description).
pub fn parse_sent(body: &[u8]) -> Result<Option<i64>, String> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| format!("invalid Telegram response: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "invalid Telegram response: expected an object".to_string())?;
    match object.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(object
            .get("result")
            .and_then(|result| result.get("message_id"))
            .and_then(Value::as_i64)),
        Some(false) => Err(description(object).to_string()),
        None => Err("invalid Telegram response: missing ok".to_string()),
    }
}

/// The HTTP side. Claude implements it with reqwest; tests use [`FakeBot`].
#[async_trait]
pub trait BotApi: Send + Sync {
    /// POST https://api.telegram.org/bot<token>/<method> with a JSON body; returns the raw body.
    async fn call(&self, method: &str, body: Value) -> Result<Vec<u8>, String>;
    /// Download a file by `file_id` (getFile + file download), at most `max_bytes`.
    async fn download(&self, file_id: &str, max_bytes: u64) -> Result<Vec<u8>, String>;
}

/// Records calls and replays queued responses, for tests (and Claude's demo).
#[derive(Default)]
pub struct FakeBot {
    calls: Mutex<Vec<(String, Value)>>,
    responses: Mutex<VecDeque<(String, Value)>>,
    files: Mutex<HashMap<String, Vec<u8>>>,
}

impl FakeBot {
    /// Queue one response for `method`. `"*"` answers any method.
    pub fn push_response(&self, method: &str, body: Value) {
        if let Ok(mut responses) = self.responses.lock() {
            responses.push_back((method.to_string(), body));
        }
    }

    /// Make `file_id` downloadable.
    pub fn add_file(&self, file_id: &str, bytes: Vec<u8>) {
        if let Ok(mut files) = self.files.lock() {
            files.insert(file_id.to_string(), bytes);
        }
    }

    /// Everything sent so far, in order.
    pub fn calls(&self) -> Vec<(String, Value)> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl BotApi for FakeBot {
    async fn call(&self, method: &str, body: Value) -> Result<Vec<u8>, String> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((method.to_string(), body));
        }
        let response = {
            let mut responses = self
                .responses
                .lock()
                .map_err(|_| "fake bot responses are poisoned".to_string())?;
            let position = responses
                .iter()
                .position(|(queued, _)| queued == method || queued == "*");
            position.and_then(|position| responses.remove(position).map(|(_, body)| body))
        };
        match response {
            Some(body) => {
                serde_json::to_vec(&body).map_err(|error| format!("fake bot response: {error}"))
            }
            None => Ok(b"{\"ok\":true,\"result\":{}}".to_vec()),
        }
    }

    async fn download(&self, file_id: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
        let files = self
            .files
            .lock()
            .map_err(|_| "fake bot files are poisoned".to_string())?;
        let bytes = files
            .get(file_id)
            .ok_or_else(|| format!("no fake file {file_id}"))?;
        if bytes.len() as u64 > max_bytes {
            return Err("file is larger than the limit".to_string());
        }
        Ok(bytes.clone())
    }
}
