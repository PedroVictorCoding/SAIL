//! SAIL's Telegram gateway (P2, round 22): talk to SAIL from a phone.
//!
//! * **No inbound port.** The bot is long-polled (`getUpdates`); the update offset is
//!   persisted, so every update is handled once across restarts.
//! * **Default deny.** A sender who is not paired gets a one-time pairing code and nothing
//!   else; the owner approves the code in SAIL Agents › Messaging (`sail_flow::gateway::
//!   PairingStore`). Group chats, channels and bots are ignored.
//! * **Sessions.** One chat is one SAIL conversation (the same turn path as a typed web
//!   turn, with Arena's bounded context and memory rules) until `/new`.
//! * **Approvals.** A tool that needs approval becomes a chat card naming the action, with
//!   Approve / Deny buttons and `/approve <id>`. Approving mints the same single-use
//!   Guardian approval as the web UI, then SAIL continues. Critical-risk actions are never
//!   approvable from chat: they need the web UI.
//! * **Voice notes** are decoded (OGG/Opus) and transcribed by SAIL's local Whisper.
//! * **Delivery** goes through the outbox: at least once, deduplicated by a stable key per
//!   reply part, with back-off. The bot token is stored `0600` in `$SAIL_DATA/gateway`.
use std::{
    collections::BTreeMap,
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use sail_flow::gateway::{
    Admission, BotApi, BotToken, Command, Incoming, IncomingKind, OutMsg, Outbox, Outgoing,
    PairingStore, PairingView, commands, parse_sent, parse_updates, request, text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

/// Longest wait of one `getUpdates` call.
const POLL_TIMEOUT_SECS: u64 = 25;
/// Largest voice note downloaded (Telegram's bot download limit is 20 MB).
const MAX_VOICE_BYTES: u64 = 20 * 1024 * 1024;
const CHUNK_CHARS: usize = 4000;

/// A tool call waiting for the owner's approval, as a turn reported it.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalAsk {
    pub session_id: uuid::Uuid,
    pub tool: String,
    pub rationale: String,
}

/// What one turn produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatReply {
    pub text: String,
    pub approvals: Vec<ApprovalAsk>,
}

/// What the gateway needs from the rest of SAIL. The daemon runs real turns; tests and the
/// demo use fakes.
#[async_trait]
pub trait ChatRunner: Send + Sync {
    /// One conversation turn in `session` (created on first use).
    async fn turn(&self, session: &str, text: &str) -> Result<ChatReply, String>;
    /// Local speech-to-text of 16 kHz mono samples.
    async fn transcribe(&self, pcm: Vec<i16>) -> Result<String, String>;
    /// Skill names `/<skill>` may use.
    async fn skills(&self) -> Vec<String>;
    /// `/status`: one short paragraph.
    async fn status(&self) -> String;
    /// `/jobs`: the scheduled jobs, one per line.
    async fn jobs(&self) -> String;
    /// Approve or deny a Guardian approval session from chat. Err for unknown, expired or
    /// critical-risk sessions (with a message for the owner).
    fn resolve(&self, session: uuid::Uuid, approved: bool) -> Result<(), String>;
}

/// Gateway status for the Messaging section.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct GwStatus {
    pub connected: bool,
    pub bot_username: Option<String>,
    pub error: Option<String>,
    pub last_poll_at: Option<i64>,
}

/// One chat's conversation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ChatState {
    pub chat_id: i64,
    pub name: String,
    pub session: String,
    pub last_message_at: i64,
    pub turns: u64,
    /// Approval cards still open in this chat: short id → (session, tool).
    #[serde(default)]
    pub approvals: BTreeMap<String, (uuid::Uuid, String)>,
}

type BotFactory = dyn Fn(&BotToken) -> Arc<dyn BotApi> + Send + Sync;

/// The gateway: pairing, sessions, the poll loop and delivery.
pub struct GatewayService {
    pub dir: PathBuf,
    pub pairing: PairingStore,
    pub outbox: Outbox,
    runner: Arc<dyn ChatRunner>,
    make_bot: Arc<BotFactory>,
    bot: Mutex<Option<Arc<dyn BotApi>>>,
    status: Mutex<GwStatus>,
    chats: Mutex<BTreeMap<i64, ChatState>>,
    /// Running turn per chat, so `/stop` can end it.
    running: Mutex<BTreeMap<i64, tokio::task::AbortHandle>>,
    /// One handler at a time per chat (updates of one chat stay in order).
    chat_locks: Mutex<BTreeMap<i64, Arc<tokio::sync::Mutex<()>>>>,
    failed_recently: AtomicUsize,
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn random_code_bytes() -> [u8; sail_flow::gateway::CODE_LEN] {
    let mut out = [0u8; sail_flow::gateway::CODE_LEN];
    let bytes = uuid::Uuid::new_v4().into_bytes();
    out.copy_from_slice(&bytes[..sail_flow::gateway::CODE_LEN]);
    out
}

/// Session ids are UUIDs, like web sessions (turn correlation ids are derived from them).
fn new_session(_chat_id: i64) -> String {
    uuid::Uuid::new_v4().to_string()
}

impl GatewayService {
    pub fn new(dir: PathBuf, runner: Arc<dyn ChatRunner>, make_bot: Arc<BotFactory>) -> Self {
        let chats = std::fs::read(dir.join("chats.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<ChatState>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.chat_id, c))
            .collect();
        Self {
            pairing: PairingStore { dir: dir.clone() },
            outbox: Outbox { dir: dir.clone() },
            dir,
            runner,
            make_bot,
            bot: Mutex::new(None),
            status: Mutex::new(GwStatus::default()),
            chats: Mutex::new(chats),
            running: Mutex::new(BTreeMap::new()),
            chat_locks: Mutex::new(BTreeMap::new()),
            failed_recently: AtomicUsize::new(0),
        }
    }

    fn token_path(&self) -> PathBuf {
        self.dir.join("telegram.token")
    }

    fn save_chats(&self) {
        let list: Vec<ChatState> = self.chats.lock().unwrap().values().cloned().collect();
        if let Ok(bytes) = serde_json::to_vec_pretty(&list)
            && let Err(error) = write_private(&self.dir.join("chats.json"), &bytes)
        {
            warn!(%error, "gateway chats not saved");
        }
    }

    pub fn status(&self) -> GwStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn pairing_view(&self) -> Result<PairingView, String> {
        self.pairing.snapshot(Utc::now().timestamp())
    }

    pub fn chats(&self) -> Vec<Value> {
        self.chats
            .lock()
            .unwrap()
            .values()
            .map(|c| {
                json!({"chat_id": c.chat_id, "name": c.name, "last_message_at": c.last_message_at, "turns": c.turns})
            })
            .collect()
    }

    pub fn outbox_view(&self) -> Value {
        let queued = self
            .outbox
            .due(i64::MAX, 10_000)
            .map(|v| v.len())
            .unwrap_or(0);
        json!({"queued": queued, "failed_recently": self.failed_recently.load(Ordering::SeqCst)})
    }

    /// Check a token with `getMe`, then keep it (0600) and start using it.
    pub async fn connect(&self, token: &str) -> Result<GwStatus, String> {
        let token = BotToken::parse(token.trim())?;
        let bot = (self.make_bot)(&token);
        let me = bot
            .call("getMe", json!({}))
            .await
            .map_err(|e| format!("Telegram didn't accept the token: {e}"))?;
        let me: Value =
            serde_json::from_slice(&me).map_err(|_| "Telegram's reply was unreadable")?;
        if me["ok"] != true {
            return Err(me["description"]
                .as_str()
                .unwrap_or("Telegram refused the token")
                .to_string());
        }
        let username = me["result"]["username"].as_str().map(String::from);
        write_private(&self.token_path(), token.expose().as_bytes())?;
        *self.bot.lock().unwrap() = Some(bot);
        *self.status.lock().unwrap() = GwStatus {
            connected: true,
            bot_username: username.clone(),
            error: None,
            last_poll_at: None,
        };
        info!(bot = ?username, "Telegram gateway connected by owner");
        Ok(self.status())
    }

    /// Forget the token and stop polling.
    pub fn disconnect(&self) -> GwStatus {
        *self.bot.lock().unwrap() = None;
        let _ = std::fs::remove_file(self.token_path());
        *self.status.lock().unwrap() = GwStatus::default();
        info!("Telegram gateway disconnected by owner");
        self.status()
    }

    /// Reconnect with the saved token (start-up).
    pub async fn resume(&self) {
        let Ok(token) = std::fs::read_to_string(self.token_path()) else {
            return;
        };
        if let Err(error) = self.connect(&token).await {
            warn!(%error, "Telegram gateway could not reconnect");
            self.status.lock().unwrap().error = Some(error);
        }
    }

    fn bot(&self) -> Option<Arc<dyn BotApi>> {
        self.bot.lock().unwrap().clone()
    }

    /// Start a fresh conversation in a chat (`/new` or the owner's button).
    pub fn new_conversation(&self, chat_id: i64) -> bool {
        let mut chats = self.chats.lock().unwrap();
        let Some(chat) = chats.get_mut(&chat_id) else {
            return false;
        };
        chat.session = new_session(chat_id);
        chat.turns = 0;
        chat.approvals.clear();
        drop(chats);
        self.save_chats();
        true
    }

    /// Queue a message to every paired chat (scheduled results). `key` dedups it.
    pub fn broadcast(&self, key: &str, body: &str) {
        let Ok(view) = self.pairing.snapshot(Utc::now().timestamp()) else {
            return;
        };
        if self.bot().is_none() {
            return;
        }
        for paired in view.paired {
            self.queue(
                paired.chat_id,
                &format!("{key}:{}", paired.chat_id),
                body,
                None,
                Vec::new(),
            );
        }
    }

    /// Welcome a newly paired chat.
    pub fn broadcast_one(&self, chat_id: i64) {
        self.queue(
            chat_id,
            &format!("paired:{chat_id}"),
            "You're paired with SAIL. Send a message or a voice note; /help lists the commands.",
            None,
            Vec::new(),
        );
    }

    fn queue(
        &self,
        chat_id: i64,
        key: &str,
        body: &str,
        reply_to: Option<i64>,
        buttons: Vec<(String, String)>,
    ) {
        let now = Utc::now().timestamp();
        let parts = text::chunks(&text::plain(body), CHUNK_CHARS);
        let last = parts.len().saturating_sub(1);
        for (i, part) in parts.into_iter().enumerate() {
            let msg = OutMsg {
                key: format!("{key}:{i}"),
                chat_id,
                text: part,
                reply_to: if i == 0 { reply_to } else { None },
                buttons: if i == last {
                    buttons.clone()
                } else {
                    Vec::new()
                },
                attempts: 0,
                next_attempt_at: now,
                created_at: now,
            };
            if let Err(error) = self.outbox.enqueue(msg) {
                warn!(%error, "reply not queued");
            }
        }
    }

    /// Start the poll and delivery loops (once per process).
    pub fn start(self: &Arc<Self>) {
        let poller = self.clone();
        tokio::spawn(async move {
            poller.resume().await;
            loop {
                if !poller.poll_once().await {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        });
        let sender = self.clone();
        tokio::spawn(async move {
            loop {
                sender.deliver_once().await;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }

    /// One `getUpdates` round. False when there is no bot or the call failed.
    pub async fn poll_once(self: &Arc<Self>) -> bool {
        let Some(bot) = self.bot() else {
            return false;
        };
        let offset = self.outbox.offset().unwrap_or(0);
        let body = json!({"offset": offset, "timeout": POLL_TIMEOUT_SECS, "allowed_updates": ["message", "callback_query"]});
        let result = bot
            .call("getUpdates", body)
            .await
            .and_then(|raw| parse_updates(&raw, offset));
        let now = Utc::now().timestamp();
        let (updates, next) = match result {
            Ok(r) => r,
            Err(error) => {
                let mut status = self.status.lock().unwrap();
                status.error = Some(error);
                return false;
            }
        };
        {
            let mut status = self.status.lock().unwrap();
            status.last_poll_at = Some(now);
            status.error = None;
        }
        // Persist the offset first: an update is handled at most once even if SAIL stops
        // mid-turn; its reply is then simply not sent (the owner can ask again).
        if let Err(error) = self.outbox.set_offset(next) {
            warn!(%error, "update offset not saved");
        }
        for update in updates {
            let this = self.clone();
            tokio::spawn(async move { this.handle(update).await });
        }
        true
    }

    /// Send what is due.
    pub async fn deliver_once(&self) {
        let Some(bot) = self.bot() else {
            return;
        };
        let now = Utc::now().timestamp();
        for msg in self.outbox.due(now, 20).unwrap_or_default() {
            let out = Outgoing::Send {
                chat_id: msg.chat_id,
                text: msg.text.clone(),
                reply_to: msg.reply_to,
                buttons: msg.buttons.clone(),
            };
            let (method, body) = request(&out);
            match bot.call(method, body).await.and_then(|b| parse_sent(&b)) {
                Ok(_) => {
                    let _ = self.outbox.sent(&msg.key, now);
                }
                Err(error) => {
                    warn!(%error, key = %msg.key, "Telegram delivery failed; will retry");
                    if let Ok(true) = self.outbox.failed(&msg.key, now) {
                        self.failed_recently.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        }
        let _ = self.outbox.prune(now);
    }

    async fn send_now(&self, out: Outgoing) {
        if let Some(bot) = self.bot() {
            let (method, body) = request(&out);
            let _ = bot.call(method, body).await;
        }
    }

    fn chat_lock(&self, chat_id: i64) -> Arc<tokio::sync::Mutex<()>> {
        self.chat_locks
            .lock()
            .unwrap()
            .entry(chat_id)
            .or_default()
            .clone()
    }

    /// Handle one update: admission, then the command or turn.
    pub async fn handle(self: Arc<Self>, update: Incoming) {
        let now = Utc::now().timestamp();
        let key = format!("{}:{}", update.chat_id, update.update_id);
        let admission = match self.pairing.admit(
            update.sender_id,
            update.chat_id,
            &update.sender_name,
            now,
            random_code_bytes(),
        ) {
            Ok(a) => a,
            Err(error) => {
                warn!(%error, "pairing store unreadable; message ignored");
                return;
            }
        };
        match admission {
            Admission::Allowed => {}
            Admission::NewCode(code) => {
                info!(sender = update.sender_id, "Telegram pairing code issued");
                self.queue(
                    update.chat_id,
                    &key,
                    &format!(
                        "Hi! This is a private SAIL assistant. To talk to it, ask its owner to approve this code in SAIL › Agents › Messaging:\n\n{code}\n\nThe code is valid for 1 hour."
                    ),
                    None,
                    Vec::new(),
                );
                return;
            }
            Admission::Decline => {
                self.queue(
                    update.chat_id,
                    &key,
                    "This SAIL assistant is private and isn't accepting new contacts.",
                    None,
                    Vec::new(),
                );
                return;
            }
            Admission::Ignore => return,
        }
        let lock = self.chat_lock(update.chat_id);
        // `/stop` must not wait behind the turn it stops.
        if let IncomingKind::Text { text: t } = &update.kind
            && matches!(commands::parse(t, &[]), Command::Stop)
        {
            let stopped = self
                .running
                .lock()
                .unwrap()
                .remove(&update.chat_id)
                .map(|h| h.abort())
                .is_some();
            self.queue(
                update.chat_id,
                &key,
                if stopped {
                    "Stopped."
                } else {
                    "Nothing is running."
                },
                Some(update.message_id),
                Vec::new(),
            );
            return;
        }
        let _guard = lock.lock().await;
        {
            let mut chats = self.chats.lock().unwrap();
            let chat = chats.entry(update.chat_id).or_insert_with(|| ChatState {
                chat_id: update.chat_id,
                name: update.sender_name.clone(),
                session: new_session(update.chat_id),
                ..ChatState::default()
            });
            chat.last_message_at = now;
        }
        match update.kind.clone() {
            IncomingKind::Text { text: t } => self.text(&update, &key, &t).await,
            IncomingKind::Voice { file_id, .. } => self.voice(&update, &key, &file_id).await,
            IncomingKind::Button { callback_id, data } => {
                let (approved, short) = match data.split_once(':') {
                    Some(("ap", s)) => (true, s.to_string()),
                    Some(("dn", s)) => (false, s.to_string()),
                    _ => return,
                };
                self.send_now(Outgoing::AnswerButton {
                    callback_id,
                    text: if approved { "Approved" } else { "Denied" }.into(),
                })
                .await;
                self.decide(&update, &key, Some(short), approved).await;
            }
            IncomingKind::Unsupported { what } => self.queue(
                update.chat_id,
                &key,
                &format!("SAIL can't read {what} here yet. Send text or a voice note."),
                Some(update.message_id),
                Vec::new(),
            ),
        }
        self.save_chats();
    }

    async fn text(self: &Arc<Self>, update: &Incoming, key: &str, input: &str) {
        let skills = self.runner.skills().await;
        let reply =
            |s: &str| self.queue(update.chat_id, key, s, Some(update.message_id), Vec::new());
        match commands::parse(input, &skills) {
            Command::New => {
                self.new_conversation(update.chat_id);
                reply("New conversation started.");
            }
            Command::Stop => reply("Nothing is running."),
            Command::Status => reply(&self.runner.status().await),
            Command::Jobs => reply(&self.runner.jobs().await),
            Command::Skills => reply(&if skills.is_empty() {
                "No skills yet.".to_string()
            } else {
                format!(
                    "Skills (use /name):\n{}",
                    skills
                        .iter()
                        .map(|s| format!("/{}", s.replace('-', "_")))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            }),
            Command::Approve(short) => self.decide(update, key, short, true).await,
            Command::Deny(short) => self.decide(update, key, short, false).await,
            Command::Tonight(ask) => reply(&tonight(&ask)),
            Command::Unknown(_) => reply(&commands::help_text()),
            Command::Skill { name, input } => {
                let prompt = format!(
                    "Use the skill \"{name}\": load it with skill_view first and follow it.\n{input}"
                );
                self.turn(update, key, &prompt).await;
            }
            Command::Text(t) if t.trim().is_empty() => {}
            Command::Text(t) => self.turn(update, key, &t).await,
        }
    }

    async fn voice(self: &Arc<Self>, update: &Incoming, key: &str, file_id: &str) {
        let Some(bot) = self.bot() else { return };
        let heard = async {
            let bytes = bot.download(file_id, MAX_VOICE_BYTES).await?;
            let pcm = tokio::task::spawn_blocking(move || crate::ogg_opus::decode(&bytes))
                .await
                .map_err(|e| e.to_string())??;
            self.runner.transcribe(pcm).await
        }
        .await;
        match heard {
            Ok(t) if !t.trim().is_empty() => {
                self.queue(
                    update.chat_id,
                    &format!("{key}:heard"),
                    &format!("🎙 {t}"),
                    Some(update.message_id),
                    Vec::new(),
                );
                self.turn(update, key, &t).await;
            }
            Ok(_) => self.queue(
                update.chat_id,
                key,
                "I couldn't hear any words in that voice note.",
                Some(update.message_id),
                Vec::new(),
            ),
            Err(error) => self.queue(
                update.chat_id,
                key,
                &format!("I couldn't transcribe that voice note: {error}"),
                Some(update.message_id),
                Vec::new(),
            ),
        }
    }

    async fn turn(self: &Arc<Self>, update: &Incoming, key: &str, prompt: &str) {
        let session = {
            let chats = self.chats.lock().unwrap();
            chats.get(&update.chat_id).map(|c| c.session.clone())
        };
        let Some(session) = session else { return };
        self.send_now(Outgoing::Typing {
            chat_id: update.chat_id,
        })
        .await;
        let runner = self.runner.clone();
        let (s, p) = (session.clone(), prompt.to_string());
        let task = tokio::spawn(async move { runner.turn(&s, &p).await });
        self.running
            .lock()
            .unwrap()
            .insert(update.chat_id, task.abort_handle());
        let result = task.await;
        self.running.lock().unwrap().remove(&update.chat_id);
        let reply = match result {
            Ok(Ok(reply)) => reply,
            Ok(Err(error)) => ChatReply {
                text: format!("Something went wrong: {error}"),
                approvals: Vec::new(),
            },
            Err(_) => return, // stopped with /stop
        };
        {
            let mut chats = self.chats.lock().unwrap();
            if let Some(chat) = chats.get_mut(&update.chat_id) {
                chat.turns += 1;
                for ask in &reply.approvals {
                    chat.approvals.insert(
                        commands::short_id(&ask.session_id),
                        (ask.session_id, ask.tool.clone()),
                    );
                }
            }
        }
        if !reply.text.trim().is_empty() {
            self.queue(
                update.chat_id,
                key,
                &reply.text,
                Some(update.message_id),
                Vec::new(),
            );
        }
        for ask in reply.approvals {
            let short = commands::short_id(&ask.session_id);
            self.queue(
                update.chat_id,
                &format!("{key}:ap:{short}"),
                &text::approval_card(&ask.tool, &ask.rationale, &short),
                None,
                vec![
                    ("Approve".into(), format!("ap:{short}")),
                    ("Deny".into(), format!("dn:{short}")),
                ],
            );
        }
    }

    /// `/approve`, `/deny` and the card buttons.
    async fn decide(
        self: &Arc<Self>,
        update: &Incoming,
        key: &str,
        short: Option<String>,
        approved: bool,
    ) {
        let found = {
            let chats = self.chats.lock().unwrap();
            chats.get(&update.chat_id).and_then(|c| match &short {
                Some(s) => c
                    .approvals
                    .iter()
                    .find(|(_, (id, _))| commands::matches(s, id))
                    .map(|(k, v)| (k.clone(), v.clone())),
                None if c.approvals.len() == 1 => c
                    .approvals
                    .iter()
                    .next()
                    .map(|(k, v)| (k.clone(), v.clone())),
                None => None,
            })
        };
        let Some((short_id, (session, tool))) = found else {
            self.queue(
                update.chat_id,
                key,
                "I don't have that approval open. Use /approve <id> with the id on the card.",
                Some(update.message_id),
                Vec::new(),
            );
            return;
        };
        if let Some(chat) = self.chats.lock().unwrap().get_mut(&update.chat_id) {
            chat.approvals.remove(&short_id);
        }
        match self.runner.resolve(session, approved) {
            Ok(()) if approved => {
                info!(tool = %tool, "Approval granted from Telegram");
                self.turn(
                    update,
                    &format!("{key}:after"),
                    &format!("The owner approved {tool}. Do it now and report the result."),
                )
                .await;
            }
            Ok(()) => self.queue(
                update.chat_id,
                key,
                &format!("Denied: {tool}."),
                Some(update.message_id),
                Vec::new(),
            ),
            Err(error) => self.queue(
                update.chat_id,
                key,
                &error,
                Some(update.message_id),
                Vec::new(),
            ),
        }
    }
}

static GLOBAL: OnceLock<Arc<GatewayService>> = OnceLock::new();

/// The daemon's gateway, once started.
pub fn global() -> Option<Arc<GatewayService>> {
    GLOBAL.get().cloned()
}

/// Install and start the daemon's gateway.
pub fn start(service: Arc<GatewayService>) -> Arc<GatewayService> {
    let service = GLOBAL.get_or_init(|| service).clone();
    if tokio::runtime::Handle::try_current().is_ok() {
        service.start();
    }
    service
}

/// The real Bot API over HTTPS.
pub struct ReqwestBot {
    client: reqwest::Client,
    base: String,
    file_base: String,
}

impl ReqwestBot {
    pub fn new(token: &BotToken) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(POLL_TIMEOUT_SECS + 20))
                .build()
                .unwrap_or_default(),
            base: format!("https://api.telegram.org/bot{}", token.expose()),
            file_base: format!("https://api.telegram.org/file/bot{}", token.expose()),
        }
    }
}

/// Errors from reqwest include the URL, which contains the token: never pass them on.
fn scrub(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "Telegram did not answer in time".into()
    } else if error.is_connect() {
        "could not reach Telegram".into()
    } else {
        "the request to Telegram failed".into()
    }
}

#[async_trait]
impl BotApi for ReqwestBot {
    async fn call(&self, method: &str, body: Value) -> Result<Vec<u8>, String> {
        let response = self
            .client
            .post(format!("{}/{method}", self.base))
            .json(&body)
            .send()
            .await
            .map_err(scrub)?;
        let bytes = response.bytes().await.map_err(scrub)?;
        Ok(bytes.to_vec())
    }

    async fn download(&self, file_id: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
        let meta = self.call("getFile", json!({"file_id": file_id})).await?;
        let meta: Value = serde_json::from_slice(&meta).map_err(|_| "unreadable getFile reply")?;
        let path = meta["result"]["file_path"]
            .as_str()
            .filter(|p| !p.contains(".."))
            .ok_or("Telegram has no file for that voice note")?;
        if meta["result"]["file_size"].as_u64().unwrap_or(0) > max_bytes {
            return Err("the voice note is too large".into());
        }
        let mut response = self
            .client
            .get(format!("{}/{path}", self.file_base))
            .send()
            .await
            .map_err(scrub)?;
        let mut out = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(scrub)? {
            out.extend_from_slice(&chunk);
            if out.len() as u64 > max_bytes {
                return Err("the voice note is too large".into());
            }
        }
        Ok(out)
    }
}

/// `/tonight <ask>`: add a Night Shift errand from chat. The work order is drafted in the
/// background and approved in the app, never from this message alone.
fn tonight(ask: &str) -> String {
    if ask.trim().is_empty() {
        return "Tell me what to build, e.g. /tonight make the Capital Today tab show the next dividend date".into();
    }
    match crate::nightshift::global() {
        Some(service) => match service.add(ask, sail_forge::nightshift::ErrandSource::Telegram) {
            Ok(errand) => format!(
                "Got it ({}). I'll draft a work order; approve it in Night Shift before tonight.",
                errand.id.as_str()
            ),
            Err(error) => format!("I couldn't add that: {error}"),
        },
        None => "Night Shift isn't running on SAIL right now.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sail_flow::gateway::FakeBot;

    const TOKEN: &str = "123456789:AAH-fake_token_for_tests_only_0123456789";

    #[derive(Default)]
    struct Runner {
        resolved: Mutex<Vec<(uuid::Uuid, bool)>>,
        prompts: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl ChatRunner for Runner {
        async fn turn(&self, session: &str, text: &str) -> Result<ChatReply, String> {
            self.prompts
                .lock()
                .unwrap()
                .push((session.into(), text.into()));
            let approvals = if text.contains("delete") {
                vec![ApprovalAsk {
                    session_id: uuid::Uuid::from_u128(0xabcdef12_0000_0000_0000_000000000001),
                    tool: "fs:delete".into(),
                    rationale: "Delete notes.txt".into(),
                }]
            } else {
                Vec::new()
            };
            Ok(ChatReply {
                text: format!("**Echo**: {text}"),
                approvals,
            })
        }
        async fn transcribe(&self, _pcm: Vec<i16>) -> Result<String, String> {
            Ok("hello from voice".into())
        }
        async fn skills(&self) -> Vec<String> {
            vec!["weekly-report".into()]
        }
        async fn status(&self) -> String {
            "fine".into()
        }
        async fn jobs(&self) -> String {
            "no jobs".into()
        }
        fn resolve(&self, session: uuid::Uuid, approved: bool) -> Result<(), String> {
            self.resolved.lock().unwrap().push((session, approved));
            Ok(())
        }
    }

    fn update(id: i64, sender: i64, text: &str) -> Incoming {
        Incoming {
            update_id: id,
            chat_id: sender,
            sender_id: sender,
            sender_name: "Ana".into(),
            message_id: id * 10,
            kind: IncomingKind::Text { text: text.into() },
            date: 0,
        }
    }

    async fn setup() -> (
        tempfile::TempDir,
        Arc<GatewayService>,
        Arc<FakeBot>,
        Arc<Runner>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let bot = Arc::new(FakeBot::default());
        let runner = Arc::new(Runner::default());
        let shared = bot.clone();
        let service = Arc::new(GatewayService::new(
            tmp.path().to_path_buf(),
            runner.clone(),
            Arc::new(move |_t: &BotToken| shared.clone() as Arc<dyn BotApi>),
        ));
        bot.push_response(
            "getMe",
            json!({"ok": true, "result": {"username": "sail_test_bot"}}),
        );
        let status = service.connect(TOKEN).await.unwrap();
        assert_eq!(status.bot_username.as_deref(), Some("sail_test_bot"));
        (tmp, service, bot, runner)
    }

    fn sent(bot: &FakeBot) -> Vec<Value> {
        bot.calls()
            .into_iter()
            .filter(|(m, _)| m == "sendMessage")
            .map(|(_, b)| b)
            .collect()
    }

    #[tokio::test]
    async fn strangers_get_a_code_and_paired_chats_get_answers() {
        let (tmp, service, bot, runner) = setup().await;
        // The token is kept private and never shown.
        let token_file = tmp.path().join("telegram.token");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&token_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!format!("{:?}", service.status()).contains("AAH-fake"));

        service.clone().handle(update(1, 42, "hi")).await;
        service.deliver_once().await;
        let code = service.pairing_view().unwrap().pending[0].code.clone();
        assert!(sent(&bot)[0]["text"].as_str().unwrap().contains(&code));
        assert!(
            runner.prompts.lock().unwrap().is_empty(),
            "no turn before pairing"
        );

        service
            .pairing
            .approve(&code, Utc::now().timestamp())
            .unwrap();
        service.clone().handle(update(2, 42, "hello")).await;
        // The same update again (a redelivery) does not produce a second reply.
        service.clone().handle(update(2, 42, "hello")).await;
        service.deliver_once().await;
        let replies: Vec<_> = sent(&bot)
            .into_iter()
            .filter(|b| b["text"] == "Echo: hello")
            .collect();
        assert_eq!(replies.len(), 1, "plain text, sent once");

        // An approval card, then /approve with its short id continues the turn.
        service
            .clone()
            .handle(update(3, 42, "please delete notes"))
            .await;
        service.deliver_once().await;
        let card = sent(&bot)
            .into_iter()
            .find(|b| b["text"].as_str().is_some_and(|t| t.contains("/approve")))
            .expect("approval card");
        assert!(card.to_string().contains("ap:abcdef"));
        service
            .clone()
            .handle(update(4, 42, "/approve abcdef"))
            .await;
        assert_eq!(runner.resolved.lock().unwrap().len(), 1);
        assert!(runner.resolved.lock().unwrap()[0].1);
        assert!(
            runner
                .prompts
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .1
                .contains("approved fs:delete")
        );

        // /new starts a new session.
        let before = runner.prompts.lock().unwrap()[0].0.clone();
        service.clone().handle(update(5, 42, "/new")).await;
        service.clone().handle(update(6, 42, "again")).await;
        assert_ne!(runner.prompts.lock().unwrap().last().unwrap().0, before);

        // A /skill runs a turn that loads the skill.
        service
            .clone()
            .handle(update(7, 42, "/weekly_report this week"))
            .await;
        assert!(
            runner
                .prompts
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .1
                .contains("\"weekly-report\"")
        );

        // Disconnect forgets the token.
        service.disconnect();
        assert!(!token_file.exists());
    }
}
