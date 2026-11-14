use async_trait::async_trait;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU64, Ordering},
};
use tracing::{info, warn};

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub mod compaction;
pub mod knowledge;
pub mod store;

pub use compaction::{Breadcrumb, CompactionReceipt, ContextBudget, Recall, WorkingContext};
pub use store::{MemoryStore, NewRecord, NewToolLog, SearchHit, StoreStats, StoredRecord, ToolLogEntry};

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
    /// Consent is ephemeral and never restored on reconnect.
    learning_epoch: u64,
    learning: bool,
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
    /// What memory was searched with this turn (see `compaction::retrieval_query`), so
    /// knowledge retrieval uses the same query; `None` when the turn needs no retrieval.
    pub query: Option<String>,
}

/// An archived hit is recalled only if its relevance is at least this fraction of the best
/// hit's, so one shared word does not pull a whole unrelated turn into the prompt.
const RECALL_RELATIVE_FLOOR: f64 = 0.5;

pub struct ArenaSubsystem {
    bus: Option<EventBus>,
    store: Arc<MemoryStore>,
    budget: ContextBudget,
    sessions: RwLock<HashMap<String, SessionLedger>>,
    consent_epoch: AtomicU64,
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
            consent_epoch: AtomicU64::new(1),
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
        if std::env::var_os("SAIL_ARENA_DB").is_none()
            && matches!(
                sail_common::storage::resolve().source,
                sail_common::storage::DataRootSource::DataDisk
            )
        {
            for legacy in legacy_store_paths() {
                if legacy != path {
                    let _ = sail_common::storage::migrate_legacy(&legacy, &path);
                }
            }
        }
        match MemoryStore::open(&path) {
            Ok(store) => {
                info!(path = %path.display(), "SAIL Arena memory store opened");
                if let Ok(generation) = store.knowledge().deletion_generation() {
                    let _ = store.knowledge().consolidate(
                        VOICE_SCOPE,
                        &knowledge::Access::owner(),
                        generation,
                        256,
                    );
                }
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

/// Where the durable store lives: `SAIL_ARENA_DB`, else `arena/arena.db` under the SAIL
/// data root (`/srv/sail-data` when mounted; see `sail_common::storage`).
pub fn default_store_path() -> PathBuf {
    if let Ok(path) = std::env::var("SAIL_ARENA_DB") {
        return PathBuf::from(path);
    }
    sail_common::storage::data_path("arena").join("arena.db")
}

fn legacy_store_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(".local/arena.db")];
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        paths.push(xdg.join("sail/arena/arena.db"));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        paths.push(home.join(".local/share/sail/arena/arena.db"));
    }
    paths
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
    /// Consolidated knowledge relevant to `text`, limited to what `access` may see and to
    /// `token_budget` bytes of complete JSON records. Returns `None` when nothing is visible
    /// or the store is unavailable; a memory failure never blocks the conversation.
    pub fn knowledge_context(
        &self,
        text: &str,
        access: &knowledge::Access,
        token_budget: usize,
    ) -> Option<sail_protocol::memory::MemoryReply> {
        self.knowledge_lookup(text, access, 8, token_budget)
    }

    /// As `knowledge_context`, returning at most `limit` facts.
    pub fn knowledge_lookup(
        &self,
        text: &str,
        access: &knowledge::Access,
        limit: usize,
        token_budget: usize,
    ) -> Option<sail_protocol::memory::MemoryReply> {
        let query = sail_protocol::memory::MemoryQuery {
            version: sail_protocol::memory::VERSION,
            text: compaction::clip(text, 4000),
            limit,
            token_budget,
            expand_graph: true,
        };
        match self
            .store
            .knowledge()
            .search(&query, access, None, store::now_ms())
        {
            Ok(reply) if !reply.hits.is_empty() && !reply.context.is_empty() => Some(reply),
            Ok(_) => None,
            Err(error) => {
                warn!(%error, "SAIL Arena knowledge retrieval failed; continuing without it");
                None
            }
        }
    }

    /// Check whether a concept or webpage (`name` / `source_ref`) has already been added to memory.
    pub fn has_learned_knowledge(&self, scope: &str, name: &str, source_ref: Option<&str>) -> bool {
        self.store
            .knowledge()
            .has_learned_knowledge(scope, name, source_ref)
            .unwrap_or(false)
    }

    /// Add learned knowledge to memory at most once (checks if it is already in memory first).
    /// Returns `Some((claim_id, added))` where `added` is `true` if newly inserted and `false`
    /// if it was already present in memory.
    #[allow(clippy::too_many_arguments)]
    pub fn learn_knowledge_once(
        &self,
        scope: &str,
        name: &str,
        extra_aliases: &[String],
        kind: knowledge::EntityKind,
        predicate: &str,
        value: &str,
        source_ref: Option<&str>,
        direct: bool,
    ) -> Option<(i64, bool)> {
        match self.store.knowledge().learn_once(
            scope,
            name,
            extra_aliases,
            kind,
            predicate,
            value,
            source_ref,
            direct,
        ) {
            Ok((claim_id, added)) => {
                if added {
                    info!(
                        scope,
                        name, predicate, claim_id, "SAIL Arena added learned knowledge to memory"
                    );
                }
                Some((claim_id, added))
            }
            Err(error) => {
                warn!(%error, scope, name, "SAIL Arena could not record learned knowledge");
                None
            }
        }
    }

    /// Extract learned concept/webpage knowledge from a completed conversation turn (`user` and
    /// `assistant`) and add it to memory once if not already present.
    pub fn learn_from_conversation_turn(
        &self,
        scope: &str,
        user: &str,
        assistant: &str,
    ) -> Option<(i64, bool)> {
        let body = format!("User: {user}\nSAIL: {assistant}");
        let candidate = knowledge::consolidation::extract_learned_turn(&body)?;
        self.learn_knowledge_once(
            scope,
            &candidate.name,
            &[],
            candidate.kind,
            &candidate.predicate,
            &candidate.value,
            candidate.source_ref.as_deref(),
            candidate.direct,
        )
    }
}

impl ArenaSubsystem {
    /// Register a connection with learning disabled. Reconnection always requires consent.
    pub fn begin_voice_session(&self, session: &str) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.insert(session.to_string(), SessionLedger::default());
        }
    }

    pub fn learning_epoch(&self, session: &str) -> Option<u64> {
        let sessions = self.sessions.read().ok()?;
        let ledger = sessions.get(session)?;
        ledger.learning.then_some(ledger.learning_epoch)
    }

    /// Called only by the authenticated owner HTTP boundary. Serializes revocation with
    /// archival. Clearing pending sources also invalidates in-flight consolidation proposals.
    pub fn set_session_learning(
        &self,
        session: &str,
        enabled: bool,
    ) -> knowledge::KnowledgeResult<()> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| knowledge::KnowledgeError::Invalid("session lock unavailable".into()))?;
        let ledger = sessions
            .get_mut(session)
            .ok_or_else(|| knowledge::KnowledgeError::Invalid("session is disconnected".into()))?;
        if !enabled {
            // Revoke first, even if cleanup fails. An error must never leave learning enabled.
            ledger.learning = false;
            ledger.learning_epoch = self.consent_epoch.fetch_add(1, Ordering::SeqCst);
            self.store.knowledge().write(knowledge::Actor::User, |tx| {
                tx.discard_session_sources(VOICE_SCOPE, session)?;
                Ok(())
            })?;
            ledger.crumbs.clear();
        } else if !ledger.learning {
            ledger.learning_epoch = self.consent_epoch.fetch_add(1, Ordering::SeqCst);
            ledger.learning = true;
        }
        info!(session, enabled, "Conversation learning consent updated");
        Ok(())
    }

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
        self.record_turn_at_epoch(session, turn, user, assistant, self.learning_epoch(session))
    }

    /// A turn may persist only if consent existed at its start and still has the same epoch.
    pub fn record_turn_at_epoch(
        &self,
        session: &str,
        turn: u64,
        user: &str,
        assistant: &str,
        epoch: Option<u64>,
    ) -> Option<i64> {
        self.complete_response_context_at_epoch(session, turn, user, assistant, epoch)
            .and_then(|receipt| receipt.record_id)
    }

    /// Record generated text and immediately compact the session before speech drains.
    /// Returns actual before/after counts even when learning is off (no durable record).
    pub fn complete_response_context_at_epoch(
        &self,
        session: &str,
        turn: u64,
        user: &str,
        assistant: &str,
        epoch: Option<u64>,
    ) -> Option<CompactionReceipt> {
        let Ok(mut sessions) = self.sessions.write() else {
            return None;
        };
        let ledger = sessions.get_mut(session)?;
        let may_store = ledger.learning && epoch == Some(ledger.learning_epoch);
        let gist = compaction::turn_gist(user, assistant, self.budget.gist_chars);
        let body = format!("User: {user}\nSAIL: {assistant}");
        let id = may_store
            .then(|| {
                self.store
                    .put(&NewRecord {
                        scope: VOICE_SCOPE,
                        session,
                        kind: "turn",
                        turn: Some(turn),
                        gist: &gist,
                        body: &body,
                        provenance: None,
                    })
                    .map_err(
                        |error| warn!(%error, session, turn, "SAIL Arena could not archive turn"),
                    )
                    .ok()
            })
            .flatten();
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
        let hot_turns_before = ledger.hot.len();
        let hot_chars_before = hot_chars(&ledger.hot);
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
        Some(CompactionReceipt {
            record_id: id,
            hot_turns_before,
            hot_turns_after: ledger.hot.len(),
            hot_chars_before,
            hot_chars_after: hot_chars(&ledger.hot),
            evicted_turns: hot_turns_before - ledger.hot.len(),
        })
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
        let previous = hot.last().map(|t| t.user.as_str());
        let query = compaction::retrieval_query(query, previous);
        if let Some(query) = &query {
            memory.recalled = self.recall_excerpts(session, query, &hot);
        }
        ConversationContext { hot, memory, query }
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
        // bm25 is negative and lower is better; keep hits close to the best one.
        let best = hits.first().map(|h| h.score).unwrap_or(0.0);
        let hits: Vec<_> = hits
            .into_iter()
            .filter(|h| best >= 0.0 || h.score <= best * RECALL_RELATIVE_FLOOR)
            .collect();
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

    /// Archive one tool execution into the durable Arena SQLite store (`/srv/sail-data/arena/arena.db`).
    pub fn record_tool_log(&self, entry: &NewToolLog<'_>) -> Option<i64> {
        match self.store.record_tool_log(entry) {
            Ok(id) => Some(id),
            Err(error) => {
                warn!(%error, tool = entry.tool, "SAIL Arena could not record tool log");
                None
            }
        }
    }

    /// List persisted tool execution logs in reverse chronological order.
    pub fn list_tool_logs(
        &self,
        tool_filter: Option<&str>,
        query: Option<&str>,
        success_filter: Option<bool>,
        limit: usize,
        before_id: Option<i64>,
    ) -> store::StoreResult<Vec<ToolLogEntry>> {
        self.store
            .list_tool_logs(tool_filter, query, success_filter, limit, before_id)
    }

    /// Clear all persisted tool execution logs.
    pub fn clear_tool_logs(&self) -> store::StoreResult<usize> {
        self.store.clear_tool_logs()
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
        self.offload_tool_result_at_epoch(
            session,
            turn,
            tool,
            success,
            content,
            self.learning_epoch(session),
        )
    }

    pub fn offload_tool_result_at_epoch(
        &self,
        session: &str,
        turn: u64,
        tool: &str,
        success: bool,
        content: &str,
        epoch: Option<u64>,
    ) -> String {
        if content.chars().count() <= self.budget.tool_inline_chars {
            return content.to_string();
        }
        let Ok(sessions) = self.sessions.read() else {
            return compaction::tool_truncated(tool, success, content, &self.budget);
        };
        if !sessions
            .get(session)
            .is_some_and(|ledger| ledger.learning && epoch == Some(ledger.learning_epoch))
        {
            return compaction::tool_truncated(tool, success, content, &self.budget);
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
        // Also works for archived sessions no longer present in the live ledger.
        // Hold the session lock across cleanup so an in-flight turn cannot reinsert text.
        let Ok(mut sessions) = self.sessions.write() else {
            return;
        };
        sessions.remove(session);
        if let Err(error) = self.store.knowledge().write(knowledge::Actor::User, |tx| {
            tx.discard_session_sources(VOICE_SCOPE, session)
        }) {
            warn!(%error, session, "SAIL Arena could not discard pending session sources");
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
        let Ok(mut sessions) = self.sessions.write() else {
            return;
        };
        if !sessions.get(session).is_some_and(|l| l.learning) {
            return;
        }
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
                let ledger = sessions.entry(session.to_string()).or_default();
                ledger.crumbs.push_back(Breadcrumb { id, turn, gist });
                while ledger.crumbs.len() > MAX_LEDGER_TURNS {
                    ledger.crumbs.pop_front();
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
