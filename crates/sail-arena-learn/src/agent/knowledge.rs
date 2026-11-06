//! Adapter from the knowledge port to Arena's learning graph (#248-#251). All access uses
//! Guardian's `MemoryPurpose::Learn` grant, never a model-chosen scope.
use crate::core::ports::{
    KnowledgeChain, KnowledgeGraph, KnowledgeHit, KnowledgeItem, KnowledgeSource,
};
use sail_arena::MemoryStore;
use sail_arena::knowledge::learning::{ChainRelation, NewChain, NewProposition};
use sail_arena::knowledge::model::Access;
use sail_guardian::memory::memory_grant;
use sail_protocol::memory::MemoryPurpose;
use std::sync::Arc;

pub struct ArenaKnowledge {
    store: Arc<MemoryStore>,
    access: Access,
}

impl ArenaKnowledge {
    pub fn new(store: Arc<MemoryStore>) -> Result<Self, String> {
        let grant = memory_grant(&MemoryPurpose::Learn).map_err(|e| e.to_string())?;
        Ok(Self {
            store,
            access: Access::from_grant(&grant),
        })
    }
}

impl KnowledgeGraph for ArenaKnowledge {
    fn search(
        &self,
        topic: Option<&str>,
        query: &str,
        limit: usize,
        budget_bytes: usize,
    ) -> Result<Vec<KnowledgeHit>, String> {
        self.store
            .learning()
            .search(&self.access, query, topic, limit, budget_bytes)
            .map(|hits| {
                hits.into_iter()
                    .map(|h| KnowledgeHit {
                        id: h.id,
                        statement: h.statement,
                        topic: h.topic,
                        quote: h.quote,
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    }

    fn store_extraction(
        &self,
        topic: &str,
        session: Option<&str>,
        source_id: Option<i64>,
        items: &[KnowledgeItem],
        chains: &[KnowledgeChain],
    ) -> Result<usize, String> {
        let learning = self.store.learning();
        let mut ids = Vec::with_capacity(items.len());
        for item in items {
            let prop = learning
                .add_proposition(
                    &self.access,
                    NewProposition {
                        statement: &item.statement,
                        topic,
                        quote: &item.quote,
                        session,
                        source_id,
                    },
                )
                .map_err(|e| e.to_string())?;
            ids.push(prop.id);
        }
        for chain in chains {
            if chain.conclusion >= ids.len()
                || chain.premises.iter().any(|p| *p >= ids.len())
                || chain.premises.is_empty()
            {
                return Err("a chain refers to an item outside the extraction batch".into());
            }
            let premises: Vec<i64> = chain.premises.iter().map(|i| ids[*i]).collect();
            learning
                .add_chain(
                    &self.access,
                    NewChain {
                        conclusion: ids[chain.conclusion],
                        premises: &premises,
                        relation: ChainRelation::Entails,
                        session,
                    },
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(items.len())
    }

    fn covered_through(&self, session: &str) -> Result<i64, String> {
        self.store
            .learning()
            .covered_through(session)
            .map_err(|e| e.to_string())
    }

    fn set_covered_through(&self, session: &str, seq: i64) -> Result<(), String> {
        self.store
            .learning()
            .set_covered_through(session, seq)
            .map_err(|e| e.to_string())
    }

    fn add_source(
        &self,
        topic: &str,
        name: &str,
        content: &str,
    ) -> Result<KnowledgeSource, String> {
        self.store
            .learning()
            .add_source(&self.access, name, content, topic)
            .map(|s| KnowledgeSource {
                id: s.id,
                name: s.name,
                topic: s.topic,
            })
            .map_err(|e| e.to_string())
    }

    fn delete_source(&self, id: i64) -> Result<(), String> {
        self.store
            .learning()
            .delete_source(&self.access, id)
            .map_err(|e| e.to_string())
    }

    fn proposition_count(&self, topic: Option<&str>) -> Result<usize, String> {
        self.store
            .learning()
            .propositions(&self.access, topic)
            .map(|p| p.len())
            .map_err(|e| e.to_string())
    }

    fn sources(&self, topic: Option<&str>) -> Result<Vec<KnowledgeSource>, String> {
        self.store
            .learning()
            .sources(&self.access, topic)
            .map(|sources| {
                sources
                    .into_iter()
                    .map(|s| KnowledgeSource {
                        id: s.id,
                        name: s.name,
                        topic: s.topic,
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    }
}
