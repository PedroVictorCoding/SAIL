use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;
use tracing::info;

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub struct ArenaSubsystem {
    bus: Option<EventBus>,
    memory_store: RwLock<HashMap<String, String>>,
}

impl ArenaSubsystem {
    pub fn new() -> Self {
        Self {
            bus: None,
            memory_store: RwLock::new(HashMap::new()),
        }
    }

    pub fn len(&self) -> usize {
        self.memory_store.read().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
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
            if let Ok(mut store) = self.memory_store.write() {
                store.insert(record_id.clone(), format!("{collection}:{summary}"));
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
    /// Retrieve relevant records within a conversation; other sessions are excluded.
    pub fn retrieve_voice_context(&self, session: &str, query: &str, limit: usize) -> Vec<String> {
        let terms: Vec<_> = query
            .to_lowercase()
            .split_whitespace()
            .filter(|s| s.len() > 2)
            .map(str::to_owned)
            .collect();
        let prefix = format!("voice:{session}:");
        let Ok(store) = self.memory_store.read() else {
            return Vec::new();
        };
        let mut scored: Vec<_> = store
            .iter()
            .filter(|(id, _)| id.starts_with(&prefix))
            .map(|(_, text)| {
                let lower = text.to_lowercase();
                (
                    terms.iter().filter(|t| lower.contains(t.as_str())).count(),
                    text.clone(),
                )
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(limit.min(5))
            .map(|(_, s)| s.chars().take(2000).collect())
            .collect()
    }
    pub fn clear_voice_session(&self, session: &str) {
        if let Ok(mut store) = self.memory_store.write() {
            let prefix = format!("voice:{session}:");
            store.retain(|id, _| !id.starts_with(&prefix));
        }
    }
    pub fn remember_voice_turn(&self, session: &str, turn: u64, text: String) {
        if let Ok(mut store) = self.memory_store.write() {
            store.insert(
                format!("voice:{session}:{turn}"),
                text.chars().take(6000).collect(),
            );
            // Bounded voice history per session.
            if turn > 100 {
                store.remove(&format!("voice:{session}:{}", turn - 100));
            }
        }
    }
}
