//! Per-role model slots (#253). SAIL normally runs one resident model; a deployment may
//! map a role to another registered Mind model without changing the turn contracts.
//!
//! Mapping never falls back silently. A configured name is sent to Mind verbatim; Mind
//! refuses an unregistered model with `UnknownModel`, and the turn records that failure.
//! There is no hosted-provider fallback. Token usage is recorded per role in `TurnSettled`.
use crate::core::events::AgentRole;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleModels {
    slots: BTreeMap<&'static str, String>,
}

/// Every configurable slot, in a stable order for the owner view.
pub const SLOTS: [&str; 5] = ["prober", "teacher", "practitioner", "judge", "extractor"];

fn env_name(slot: &str) -> String {
    format!("SAIL_LEARN_MODEL_{}", slot.to_ascii_uppercase())
}

impl RoleModels {
    /// Reads `SAIL_LEARN_MODEL_<ROLE>` for each slot. Absent means the resident model.
    pub fn from_env() -> Self {
        let mut slots = BTreeMap::new();
        for slot in SLOTS {
            if let Ok(model) = std::env::var(env_name(slot))
                && !model.trim().is_empty()
            {
                slots.insert(slot, model.trim().to_string());
            }
        }
        Self { slots }
    }

    pub fn set(&mut self, slot: &'static str, model: impl Into<String>) {
        self.slots.insert(slot, model.into());
    }

    pub fn for_role(&self, role: AgentRole) -> Option<&str> {
        let slot = match role {
            AgentRole::Prober => "prober",
            AgentRole::Teacher => "teacher",
            AgentRole::Practitioner => "practitioner",
        };
        self.slot(slot)
    }

    pub fn judge(&self) -> Option<&str> {
        self.slot("judge")
    }

    pub fn extractor(&self) -> Option<&str> {
        self.slot("extractor")
    }

    pub fn slot(&self, slot: &str) -> Option<&str> {
        self.slots.get(slot).map(String::as_str)
    }

    /// Stable `slot -> model` pairs for an owner endpoint.
    pub fn configured(&self) -> Vec<(&'static str, String)> {
        self.slots
            .iter()
            .map(|(slot, model)| (*slot, model.clone()))
            .collect()
    }

    /// The request model field: `None` asks Mind for the resident default.
    pub fn request_model(&self, slot: &str) -> Option<String> {
        self.slot(slot).map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_and_auxiliary_slots_are_distinct() {
        let mut models = RoleModels::default();
        assert_eq!(models.for_role(AgentRole::Teacher), None);
        models.set("teacher", "qwen-hard");
        models.set("judge", "qwen-small");
        assert_eq!(models.for_role(AgentRole::Teacher), Some("qwen-hard"));
        assert_eq!(models.for_role(AgentRole::Prober), None);
        assert_eq!(models.judge(), Some("qwen-small"));
        assert_eq!(models.extractor(), None);
        assert_eq!(
            models.configured(),
            vec![
                ("judge", "qwen-small".to_string()),
                ("teacher", "qwen-hard".to_string())
            ]
        );
    }
}
