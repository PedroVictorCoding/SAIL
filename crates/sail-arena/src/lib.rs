use async_trait::async_trait;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tracing::{info, warn};

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub mod compaction;
pub mod store;

pub use compaction::{Breadcrumb, ContextBudget, Recall, WorkingContext};
pub use store::{MemoryStore, NewRecord, SearchHit, StoreStats, StoredRecord};

/// Scope for records that belong to live voice/text conversations.
const VOICE_SCOPE: &str = "voice";
/// Scope for records published on the event bus by other subsystems.
const BUS_SCOPE: &str = "bus";
/// Breadcrumbs remembered per session in RAM; older turns stay searchable on disk.
const MAX_LEDGER_TURNS: usize = 256;

/// One exchange still in the verbatim hot window.
#[derive(Debug, Clone, PartialEq)]
pub struct HotTurn {
    pub id: Option<i64>,
    pub turn: u64,
    pub user: String,
    pub assistant: String,
}

#[derive(Debug, Default)]
struct SessionLedger {
    /// Verbatim recent exchanges, oldest first.
    hot: VecDeque<HotTurn>,
    /// Archived turns that left the hot window, newest last.
    crumbs: VecDeque<Breadcrumb>,
}

/// Everything Arena hands to Dialogue for one model request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConversationContext {
    pub hot: Vec<HotTurn>,
    pub memory: WorkingContext,
}

pub struct ArenaSubsystem {
    bus: Option<EventBus>,
    store: Arc<MemoryStore>,
    budget: ContextBudget,
    sessions: RwLock<HashMap<String, SessionLedger>>,
}

impl ArenaSubsystem {
    /// Ephemeral Arena (in-memory store). Nothing survives the process.
    pub fn new() -> Self {
        let store = MemoryStore::open_in_memory().expect("in-memory SQLite is always available");
        Self::with_store(Arc::new(store))
    }

    pub fn with_store(store: Arc<MemoryStore>) -> Self {
        Self {
            bus: None,
            store,
            budget: ContextBudget::from_env(),
            sessions: RwLock::new(HashMap::new()),
        }
    }

    pub fn with_budget(mut self, budget: ContextBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Durable Arena at [`default_store_path`], unless `SAIL_ARENA_PERSIST=0`. Falls back to
    /// an in-memory store (with a warning) if the database cannot be opened.
    pub fn open_default() -> Self {
        if std::env::var("SAIL_ARENA_PERSIST").is_ok_and(|v| v == "0" || v == "false") {
            info!("SAIL Arena persistence disabled; memory lasts only for this process");
            return Self::new();
        }
        let path = default_store_path();
        match MemoryStore::open(&path) {
            Ok(store) => {
                info!(path = %path.display(), "SAIL Arena memory store opened");
                Self::with_store(Arc::new(store))
            }
            Err(error) => {
                warn!(path = %path.display(), %error, "SAIL Arena store unavailable; using in-memory memory for this process");
                Self::new()
            }
        }
    }

    pub fn store(&self) -> &Arc<MemoryStore> {
        &self.store
    }

    pub fn budget(&self) -> &ContextBudget {
        &self.budget
    }

    /// Number of live sessions with in-RAM context.
    pub fn len(&self) -> usize {
        self.sessions.read().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Where the durable store lives: `SAIL_ARENA_DB`, else the large data disk
/// (`/srv/sail-data/arena/arena.db`) when it is writable, else `~/.local/share/sail/arena`.
pub fn default_store_path() -> PathBuf {
    if let Ok(path) = std::env::var("SAIL_ARENA_DB") {
        return PathBuf::from(path);
    }
    let data_disk = PathBuf::from("/srv/sail-data/arena");
    if writable_dir(&data_disk) {
        return data_disk.join("arena.db");
    }
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("sail/arena/arena.db")
}

fn writable_dir(dir: &std::path::Path) -> bool {
    let probe = dir.join(format!(".sail-write-probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

impl Default for ArenaSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Subsystem for ArenaSubsystem {
    fn id(&self) -> SubsystemId {
        SubsystemId::Arena
    }

    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        info!("SAIL Arena initialized with associative memory engine");
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Arena memory subsystem started");
        if let Some(bus) = &self.bus {
            let event = SailEvent::new(
                self.id(),
                EventPayload::SystemReady {
                    subsystem: self.id(),
                },
            );
            let _ = bus.publish(event);
        }
        Ok(())
    }

    async fn handle_event(&self, event: &SailEvent) -> Result<()> {
        if let EventPayload::MemoryRecordStored {
            record_id,
            collection,
            summary,
        } = &event.payload
        {
            let provenance = serde_json::json!({"record_id": record_id, "source": format!("{:?}", event.source)}).to_string();
            if let Err(error) = self.store.put(&NewRecord {
                scope: BUS_SCOPE,
                session: collection,
                kind: "fact",
                turn: None,
                gist: &compaction::clip(summary, self.budget.gist_chars),
                body: summary,
                provenance: Some(&provenance),
            }) {
                warn!(%error, record_id = %record_id, "SAIL Arena could not archive memory record");
            }
            info!(
                record_id = %record_id,
                collection = %collection,
                summary = %summary,
                "SAIL Arena stored memory record"
            );
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL Arena subsystem shutting down");
        Ok(())
    }
}

impl ArenaSubsystem {
    /// Archive a finished exchange and move it into the session's hot window. Exchanges that
    /// no longer fit the hot budget become breadcrumbs. Archival failures are logged and the
    /// turn still enters the hot window, so the conversation keeps working.
    pub fn record_turn(
        &self,
        session: &str,
        turn: u64,
        user: &str,
        assistant: &str,
    ) -> Option<i64> {
        let gist = compaction::turn_gist(user, assistant, self.budget.gist_chars);
        let body = format!("User: {user}\nSAIL: {assistant}");
        let id = self
            .store
            .put(&NewRecord {
                scope: VOICE_SCOPE,
                session,
                kind: "turn",
                turn: Some(turn),
                gist: &gist,
                body: &body,
                provenance: None,
            })
            .map_err(|error| warn!(%error, session, turn, "SAIL Arena could not archive turn"))
            .ok();
        let Ok(mut sessions) = self.sessions.write() else {
            return id;
        };
        let ledger = sessions.entry(session.to_string()).or_default();
        ledger.hot.push_back(HotTurn {
            id,
            turn,
            user: user.to_string(),
            assistant: assistant.to_string(),
        });
        let hot_chars = |hot: &VecDeque<HotTurn>| {
            hot.iter()
                .map(|t| t.user.chars().count() + t.assistant.chars().count())
                .sum::<usize>()
        };
        while ledger.hot.len() > 1
            && (ledger.hot.len() > self.budget.hot_turns
                || hot_chars(&ledger.hot) > self.budget.hot_chars)
        {
            let old = ledger.hot.pop_front().expect("len > 1");
            if let Some(id) = old.id {
                ledger.crumbs.push_back(Breadcrumb {
                    id,
                    turn: old.turn,
                    gist: compaction::turn_gist(&old.user, &old.assistant, self.budget.gist_chars),
                });
            }
        }
        while ledger.crumbs.len() > MAX_LEDGER_TURNS {
            ledger.crumbs.pop_front();
        }
        id
    }

    /// Hot window, breadcrumbs and recalled excerpts for the next request, within budget.
    pub fn conversation_context(&self, session: &str, query: &str) -> ConversationContext {
        let (hot, crumbs) = match self.sessions.read() {
            Ok(sessions) => sessions
                .get(session)
                .map(|l| (l.hot.iter().cloned().collect::<Vec<_>>(), l.crumbs.clone()))
                .unwrap_or_default(),
            Err(_) => Default::default(),
        };
        let mut memory = WorkingContext::default();
        let mut used = 0;
        for crumb in crumbs.iter().rev() {
            let len = crumb.gist.chars().count() + 16;
            if used + len > self.budget.digest_chars {
                break;
            }
            used += len;
            memory.breadcrumbs.push(crumb.clone());
        }
        memory.recalled = self.recall_excerpts(session, query, &hot);
        ConversationContext { hot, memory }
    }

    fn recall_excerpts(&self, session: &str, query: &str, hot: &[HotTurn]) -> Vec<Recall> {
        if self.budget.recall_hits == 0 || self.budget.recall_chars == 0 {
            return Vec::new();
        }
        let exclude: Vec<i64> = hot.iter().filter_map(|t| t.id).collect();
        let hits = match self.store.search(
            VOICE_SCOPE,
            session,
            query,
            &exclude,
            self.budget.recall_hits,
        ) {
            Ok(hits) => hits,
            Err(error) => {
                warn!(%error, session, "SAIL Arena recall failed; continuing without archived context");
                return Vec::new();
            }
        };
        let terms = store::query_terms(query);
        let per_hit = self.budget.recall_chars / self.budget.recall_hits;
        let mut out = Vec::new();
        let mut used = 0;
        for hit in hits {
            let remaining = self.budget.recall_chars.saturating_sub(used);
            if remaining < 64 {
                break;
            }
            let Ok(Some(record)) = self.store.get(hit.id) else {
                continue;
            };
            let excerpt =
                compaction::relevant_windows(&record.body, &terms, 240, per_hit.min(remaining));
            used += excerpt.chars().count();
            out.push(Recall {
                id: hit.id,
                turn: hit.turn,
                kind: hit.kind,
                excerpt,
            });
        }
        out
    }

    /// The user's most recent prompt in the hot window.
    pub fn last_user_prompt(&self, session: &str) -> Option<String> {
        self.sessions
            .read()
            .ok()?
            .get(session)?
            .hot
            .back()
            .map(|t| t.user.clone())
    }

    /// Keep small tool results inline; archive large ones and return a preview stub with a
    /// `mem:<id>` handle the model can pass to `memory_recall`.
    pub fn offload_tool_result(
        &self,
        session: &str,
        turn: u64,
        tool: &str,
        success: bool,
        content: &str,
    ) -> String {
        if content.chars().count() <= self.budget.tool_inline_chars {
            return content.to_string();
        }
        let gist = compaction::tool_gist(tool, success, content, self.budget.gist_chars);
        match self.store.put(&NewRecord {
            scope: VOICE_SCOPE,
            session,
            kind: "tool_result",
            turn: Some(turn),
            gist: &gist,
            body: content,
            provenance: Some(tool),
        }) {
            Ok(id) => {
                info!(
                    tool,
                    id,
                    chars = content.chars().count(),
                    "Archived large tool result; prompt carries a preview"
                );
                compaction::tool_stub(id, tool, success, content, &self.budget)
            }
            Err(error) => {
                warn!(%error, tool, "SAIL Arena could not archive tool result; truncating");
                compaction::tool_truncated(tool, success, content, &self.budget)
            }
        }
    }

    /// The `memory_recall` tool. With `id`: read that archived record of this session,
    /// either the parts matching `query` or a page from `offset`. Without `id`: search the
    /// session's archive for `query`. Returns `(success, output)`.
    pub fn recall_tool(
        &self,
        session: &str,
        params: &serde_json::Value,
    ) -> (bool, serde_json::Value) {
        use serde_json::json;
        let query = params["query"].as_str().unwrap_or_default();
        let max = params["max_chars"]
            .as_u64()
            .map(|v| v as usize)
            .unwrap_or(self.budget.recall_tool_max_chars)
            .clamp(200, self.budget.recall_tool_max_chars);
        let id = params["id"]
            .as_str()
            .and_then(compaction::parse_handle)
            .or_else(|| params["id"].as_i64());
        let Some(id) = id else {
            if query.trim().is_empty() {
                return (
                    false,
                    json!({"error": "Pass id (e.g. \"mem:12\") and/or query"}),
                );
            }
            return match self.store.search(VOICE_SCOPE, session, query, &[], 8) {
                Ok(hits) => (
                    true,
                    json!({
                        "matches": hits.iter().map(|h| json!({"id": format!("mem:{}", h.id), "turn": h.turn, "kind": h.kind, "gist": h.gist})).collect::<Vec<_>>(),
                        "note": if hits.is_empty() { "No archived records match." } else { "Call memory_recall with an id to read a record." },
                    }),
                ),
                Err(error) => (false, json!({"error": error.to_string()})),
            };
        };
        let record = match self.store.get(id) {
            Ok(Some(r)) if r.scope == VOICE_SCOPE && r.session == session => r,
            Ok(_) => {
                return (
                    false,
                    json!({"error": format!("mem:{id} is not in this conversation's memory")}),
                );
            }
            Err(error) => return (false, json!({"error": error.to_string()})),
        };
        let total = record.body.chars().count();
        let terms = store::query_terms(query);
        let (text, offset, next) = if !terms.is_empty() && params["offset"].is_null() {
            (
                compaction::relevant_windows(&record.body, &terms, 800, max),
                None,
                None,
            )
        } else {
            let offset = params["offset"].as_u64().unwrap_or(0) as usize;
            let text: String = record.body.chars().skip(offset).take(max).collect();
            let end = offset + text.chars().count();
            (text, Some(offset), (end < total).then_some(end))
        };
        (
            true,
            json!({
                "id": format!("mem:{id}"),
                "kind": record.kind,
                "turn": record.turn,
                "total_chars": total,
                "offset": offset,
                "next_offset": next,
                "text": text,
            }),
        )
    }

    /// Forget a conversation completely: in-RAM context, archived records and index entries.
    pub fn clear_voice_session(&self, session: &str) {
        self.release_session(session);
        if let Err(error) = self.store.delete_session(VOICE_SCOPE, session) {
            warn!(%error, session, "SAIL Arena could not delete archived session records");
        }
    }

    /// Drop a disconnected session's in-RAM context; its archive stays on disk.
    pub fn release_session(&self, session: &str) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.remove(session);
        }
    }

    /// Archive free-form text for a session; it is reachable by recall and as a breadcrumb.
    pub fn remember_voice_turn(&self, session: &str, turn: u64, text: String) {
        let gist = compaction::clip(&text, self.budget.gist_chars);
        let stored = self.store.put(&NewRecord {
            scope: VOICE_SCOPE,
            session,
            kind: "turn",
            turn: Some(turn),
            gist: &gist,
            body: &text,
            provenance: None,
        });
        match stored {
            Ok(id) => {
                if let Ok(mut sessions) = self.sessions.write() {
                    let ledger = sessions.entry(session.to_string()).or_default();
                    ledger.crumbs.push_back(Breadcrumb { id, turn, gist });
                    while ledger.crumbs.len() > MAX_LEDGER_TURNS {
                        ledger.crumbs.pop_front();
                    }
                }
            }
            Err(error) => warn!(%error, session, "SAIL Arena could not archive text"),
        }
    }

    /// Relevant archived records for a session (at most 5, 2000 chars each); other
    /// sessions are excluded.
    pub fn retrieve_voice_context(&self, session: &str, query: &str, limit: usize) -> Vec<String> {
        let Ok(hits) = self
            .store
            .search(VOICE_SCOPE, session, query, &[], limit.min(5))
        else {
            return Vec::new();
        };
        hits.into_iter()
            .filter_map(|h| self.store.get(h.id).ok().flatten())
            .map(|r| compaction::clip(&r.body, 2000))
            .collect()
    }
}
