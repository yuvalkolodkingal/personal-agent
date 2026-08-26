//! Provenance-first memory records and trust transitions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Retrieval tier with different retention and trust semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    Working,
    Episodic,
    Semantic,
    Procedural,
    Project,
    Relationship,
}

/// Whether a memory can be treated as a user-authored fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTrust {
    TrustedUser,
    ProposedInference,
    BackgroundObservation,
    Recalled,
}

/// Durable memory with conflict-preserving provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    pub id: Uuid,
    pub tier: MemoryTier,
    pub trust: MemoryTrust,
    pub content: String,
    pub source_event_ids: Vec<String>,
    pub confidence: f32,
    pub sensitivity: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub conflicts_with: Vec<Uuid>,
}

impl Memory {
    /// Explicit remember requests are the only direct path to trusted facts.
    pub fn explicit_user(
        content: impl Into<String>,
        tier: MemoryTier,
        source_event_id: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            tier,
            trust: MemoryTrust::TrustedUser,
            content: content.into(),
            source_event_ids: vec![source_event_id.into()],
            confidence: 1.0,
            sensitivity: "private".into(),
            created_at: Utc::now(),
            expires_at: None,
            conflicts_with: Vec::new(),
        }
    }

    /// Model-extracted content enters review and never silently becomes trusted.
    pub fn proposed(
        content: impl Into<String>,
        tier: MemoryTier,
        source_event_ids: Vec<String>,
        confidence: f32,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            tier,
            trust: MemoryTrust::ProposedInference,
            content: content.into(),
            source_event_ids,
            confidence: confidence.clamp(0.0, 1.0),
            sensitivity: "private".into(),
            created_at: Utc::now(),
            expires_at: None,
            conflicts_with: Vec::new(),
        }
    }

    /// Mark recalled text so extraction does not recursively duplicate it.
    #[must_use]
    pub fn recalled(&self) -> Self {
        let mut recalled = self.clone();
        recalled.trust = MemoryTrust::Recalled;
        recalled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inference_is_never_trusted_without_review() {
        let memory = Memory::proposed("likes tea", MemoryTier::Semantic, vec!["e1".into()], 0.8);
        assert_eq!(memory.trust, MemoryTrust::ProposedInference);
        assert_eq!(memory.recalled().trust, MemoryTrust::Recalled);
    }
}
