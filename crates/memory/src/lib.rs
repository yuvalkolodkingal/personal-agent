//! Provenance-first memory records and trust transitions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
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
    ReviewedInference,
    ProposedInference,
    BackgroundObservation,
    Recalled,
}

/// Pinned embedding implementation metadata persisted beside vectors.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingModel {
    pub id: String,
    pub version: String,
    pub dimensions: usize,
    pub license: String,
}

impl Default for EmbeddingModel {
    fn default() -> Self {
        Self {
            id: "intfloat-multilingual-e5-small-onnx".into(),
            version: "1".into(),
            dimensions: 384,
            license: "MIT".into(),
        }
    }
}

/// Memory-store failure with explicit review/provenance semantics.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum MemoryError {
    #[error("memory does not exist: {0}")]
    Missing(Uuid),
    #[error("only proposed inferences can be approved or rejected")]
    NotProposed,
    #[error("recalled content cannot be recursively extracted into memory")]
    RecursiveExtraction,
    #[error("embedding dimensions do not match the pinned model")]
    EmbeddingDimensions,
    #[error("memory source provenance cannot be empty")]
    MissingProvenance,
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

/// Ranked hybrid retrieval result. Content is marked recalled before returning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecallResult {
    pub memory: Memory,
    pub lexical_score: f32,
    pub vector_score: f32,
    pub combined_score: f32,
}

/// Inspectable provenance-first memory index. Persistence adapters can serialize
/// the complete state or materialize it into SQLCipher/FTS5.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryStore {
    pub embedding_model: EmbeddingModel,
    memories: BTreeMap<Uuid, Memory>,
    embeddings: BTreeMap<Uuid, Vec<f32>>,
    rejected: BTreeSet<Uuid>,
}

impl MemoryStore {
    #[must_use]
    pub fn new(embedding_model: EmbeddingModel) -> Self {
        Self {
            embedding_model,
            memories: BTreeMap::new(),
            embeddings: BTreeMap::new(),
            rejected: BTreeSet::new(),
        }
    }

    /// Insert or replace a memory and optional persisted embedding.
    ///
    /// # Errors
    ///
    /// Rejects missing provenance, recalled records, or mismatched vector dimensions.
    pub fn upsert(
        &mut self,
        memory: Memory,
        embedding: Option<Vec<f32>>,
    ) -> Result<(), MemoryError> {
        if memory.source_event_ids.is_empty() {
            return Err(MemoryError::MissingProvenance);
        }
        if memory.trust == MemoryTrust::Recalled {
            return Err(MemoryError::RecursiveExtraction);
        }
        if embedding
            .as_ref()
            .is_some_and(|vector| vector.len() != self.embedding_model.dimensions)
        {
            return Err(MemoryError::EmbeddingDimensions);
        }
        if let Some(vector) = embedding {
            self.embeddings.insert(memory.id, vector);
        }
        self.rejected.remove(&memory.id);
        self.memories.insert(memory.id, memory);
        Ok(())
    }

    /// Approve a queued inference without rewriting it as user-authored.
    ///
    /// # Errors
    ///
    /// Returns `Missing` or `NotProposed` for invalid transitions.
    pub fn approve(&mut self, id: Uuid) -> Result<(), MemoryError> {
        let memory = self.memories.get_mut(&id).ok_or(MemoryError::Missing(id))?;
        if memory.trust != MemoryTrust::ProposedInference {
            return Err(MemoryError::NotProposed);
        }
        memory.trust = MemoryTrust::ReviewedInference;
        Ok(())
    }

    /// Reject a proposal while retaining its identifier for audit/deduplication.
    ///
    /// # Errors
    ///
    /// Returns `Missing` or `NotProposed` for invalid transitions.
    pub fn reject(&mut self, id: Uuid) -> Result<(), MemoryError> {
        let memory = self.memories.get(&id).ok_or(MemoryError::Missing(id))?;
        if memory.trust != MemoryTrust::ProposedInference {
            return Err(MemoryError::NotProposed);
        }
        self.memories.remove(&id);
        self.embeddings.remove(&id);
        self.rejected.insert(id);
        Ok(())
    }

    /// Link a conflict bidirectionally without deleting or superseding either fact.
    ///
    /// # Errors
    ///
    /// Returns `Missing` when either side is absent.
    pub fn link_conflict(&mut self, left: Uuid, right: Uuid) -> Result<(), MemoryError> {
        if !self.memories.contains_key(&left) {
            return Err(MemoryError::Missing(left));
        }
        if !self.memories.contains_key(&right) {
            return Err(MemoryError::Missing(right));
        }
        if left == right {
            return Ok(());
        }
        let left_memory = self
            .memories
            .get_mut(&left)
            .ok_or(MemoryError::Missing(left))?;
        if !left_memory.conflicts_with.contains(&right) {
            left_memory.conflicts_with.push(right);
        }
        let right_memory = self
            .memories
            .get_mut(&right)
            .ok_or(MemoryError::Missing(right))?;
        if !right_memory.conflicts_with.contains(&left) {
            right_memory.conflicts_with.push(left);
        }
        Ok(())
    }

    /// Hybrid lexical/vector retrieval. Expired and unreviewed inference records
    /// are omitted; returned records are marked recalled.
    #[must_use]
    pub fn recall(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Vec<RecallResult> {
        let query_tokens = tokenize(query);
        let mut matches = self
            .memories
            .values()
            .filter(|memory| {
                memory.expires_at.is_none_or(|expiry| expiry > now)
                    && memory.trust != MemoryTrust::ProposedInference
                    && memory.trust != MemoryTrust::BackgroundObservation
            })
            .map(|memory| {
                let content_tokens = tokenize(&memory.content);
                let overlap = query_tokens.intersection(&content_tokens).count();
                let lexical_score = if query_tokens.is_empty() {
                    0.0
                } else {
                    ratio(overlap, query_tokens.len())
                };
                let vector_score = query_embedding
                    .zip(self.embeddings.get(&memory.id))
                    .map_or(0.0, |(query, memory)| cosine(query, memory));
                RecallResult {
                    memory: memory.recalled(),
                    lexical_score,
                    vector_score,
                    combined_score: lexical_score.mul_add(0.55, vector_score * 0.45),
                }
            })
            .filter(|result| result.combined_score > 0.0)
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            right
                .combined_score
                .partial_cmp(&left.combined_score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.memory.id.cmp(&right.memory.id))
        });
        matches.truncate(limit);
        matches
    }

    #[must_use]
    pub fn proposed_queue(&self) -> Vec<&Memory> {
        self.memories
            .values()
            .filter(|memory| memory.trust == MemoryTrust::ProposedInference)
            .collect()
    }

    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<&Memory> {
        self.memories.get(&id)
    }

    #[must_use]
    pub fn export(&self) -> Vec<Memory> {
        self.memories.values().cloned().collect()
    }

    /// Delete one memory and remove its links from surviving records.
    ///
    /// # Errors
    ///
    /// Returns `Missing` when the requested memory is absent.
    pub fn delete(&mut self, id: Uuid) -> Result<Memory, MemoryError> {
        let removed = self.memories.remove(&id).ok_or(MemoryError::Missing(id))?;
        self.embeddings.remove(&id);
        for memory in self.memories.values_mut() {
            memory.conflicts_with.retain(|other| *other != id);
        }
        Ok(removed)
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new(EmbeddingModel::default())
    }
}

fn tokenize(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn ratio(numerator: usize, denominator: usize) -> f32 {
    let numerator = numerator.to_string().parse::<f32>().unwrap_or(f32::MAX);
    let denominator = denominator.to_string().parse::<f32>().unwrap_or(f32::MAX);
    numerator / denominator
}

fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let dot: f32 = left.iter().zip(right).map(|(a, b)| a * b).sum();
    let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    if left_norm <= f32::EPSILON || right_norm <= f32::EPSILON {
        0.0
    } else {
        dot / (left_norm * right_norm)
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

    #[test]
    fn explicit_facts_are_trusted_and_inferences_wait_for_review() {
        let mut store = MemoryStore::new(EmbeddingModel {
            dimensions: 3,
            ..EmbeddingModel::default()
        });
        let explicit = Memory::explicit_user("Prefers green tea", MemoryTier::Semantic, "e1");
        let proposed = Memory::proposed(
            "May prefer coffee",
            MemoryTier::Semantic,
            vec!["e2".into()],
            0.6,
        );
        store
            .upsert(explicit.clone(), Some(vec![1.0, 0.0, 0.0]))
            .expect("explicit");
        store
            .upsert(proposed.clone(), Some(vec![0.0, 1.0, 0.0]))
            .expect("proposal");
        assert_eq!(store.proposed_queue().len(), 1);
        assert_eq!(
            store.recall("coffee", Some(&[0.0, 1.0, 0.0]), 5, Utc::now()),
            vec![]
        );
        store.approve(proposed.id).expect("review");
        assert_eq!(
            store.get(proposed.id).expect("memory").trust,
            MemoryTrust::ReviewedInference
        );
        assert_eq!(
            store.recall("coffee", Some(&[0.0, 1.0, 0.0]), 5, Utc::now())[0]
                .memory
                .trust,
            MemoryTrust::Recalled
        );
    }

    #[test]
    fn conflicts_remain_visible_and_recalled_text_cannot_reenter_index() {
        let mut store = MemoryStore::default();
        let left = Memory::explicit_user("Lives in Haifa", MemoryTier::Relationship, "e1");
        let right = Memory::explicit_user("Lives in Tel Aviv", MemoryTier::Relationship, "e2");
        store.upsert(left.clone(), None).expect("left");
        store.upsert(right.clone(), None).expect("right");
        store.link_conflict(left.id, right.id).expect("conflict");
        assert_eq!(store.get(left.id).unwrap().conflicts_with, vec![right.id]);
        assert_eq!(store.get(right.id).unwrap().conflicts_with, vec![left.id]);
        assert_eq!(
            store.upsert(left.recalled(), None),
            Err(MemoryError::RecursiveExtraction)
        );
    }

    #[test]
    fn hybrid_retrieval_is_ranked_and_delete_cleans_conflict_links() {
        let mut store = MemoryStore::new(EmbeddingModel {
            dimensions: 2,
            ..EmbeddingModel::default()
        });
        let exact = Memory::explicit_user("Project Atlas uses Rust", MemoryTier::Project, "e1");
        let related = Memory::explicit_user("Compiler decision", MemoryTier::Project, "e2");
        store
            .upsert(exact.clone(), Some(vec![1.0, 0.0]))
            .expect("exact");
        store
            .upsert(related.clone(), Some(vec![0.8, 0.2]))
            .expect("related");
        store.link_conflict(exact.id, related.id).expect("link");
        let results = store.recall("Atlas Rust", Some(&[1.0, 0.0]), 2, Utc::now());
        assert_eq!(results[0].memory.id, exact.id);
        store.delete(related.id).expect("delete");
        assert!(store.get(exact.id).unwrap().conflicts_with.is_empty());
    }
}
