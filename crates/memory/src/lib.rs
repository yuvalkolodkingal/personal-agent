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
    #[error("memory namespace cannot be blank")]
    MissingNamespace,
    #[error("project graph node does not exist: {0}")]
    MissingProjectNode(Uuid),
}

/// Explicit isolation boundary for global, profile, project, and conversation memory.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum MemoryNamespace {
    Global,
    Profile(String),
    Project(String),
    Conversation(String),
}

impl MemoryNamespace {
    /// Validate the namespace identifier used to isolate durable memories.
    ///
    /// # Errors
    ///
    /// Returns a [`MemoryError`] when a scoped identifier is blank.
    pub fn validate(&self) -> Result<(), MemoryError> {
        let identifier = match self {
            Self::Global => return Ok(()),
            Self::Profile(value) | Self::Project(value) | Self::Conversation(value) => value,
        };
        if identifier.trim().is_empty() {
            Err(MemoryError::MissingNamespace)
        } else {
            Ok(())
        }
    }
}

/// A writing-style observation remains reviewable and traceable to examples.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StylePreference {
    pub id: Uuid,
    pub namespace: MemoryNamespace,
    pub description: String,
    pub examples: Vec<String>,
    pub source_event_ids: Vec<String>,
    pub confidence: f32,
    pub reviewed: bool,
}

/// Typed project graph node for repositories, people, services, and documents.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectNode {
    pub id: Uuid,
    pub namespace: MemoryNamespace,
    pub kind: String,
    pub name: String,
    pub attributes: BTreeMap<String, String>,
    pub source_event_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectRelation {
    pub from: Uuid,
    pub relation: String,
    pub to: Uuid,
    pub source_event_ids: Vec<String>,
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

/// Namespace-aware memory, writing style, and project graph index. This wraps
/// the provenance store so older encrypted snapshots remain readable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PersistentMemory {
    pub store: MemoryStore,
    scopes: BTreeMap<Uuid, MemoryNamespace>,
    style: BTreeMap<Uuid, StylePreference>,
    project_nodes: BTreeMap<Uuid, ProjectNode>,
    project_relations: Vec<ProjectRelation>,
}

impl PersistentMemory {
    #[must_use]
    pub fn new(embedding_model: EmbeddingModel) -> Self {
        Self {
            store: MemoryStore::new(embedding_model),
            scopes: BTreeMap::new(),
            style: BTreeMap::new(),
            project_nodes: BTreeMap::new(),
            project_relations: Vec::new(),
        }
    }

    /// Upgrade a legacy fact/vector store without losing existing records.
    #[must_use]
    pub fn from_store(store: MemoryStore) -> Self {
        Self {
            store,
            scopes: BTreeMap::new(),
            style: BTreeMap::new(),
            project_nodes: BTreeMap::new(),
            project_relations: Vec::new(),
        }
    }

    /// Return every style preference, including unreviewed proposals, for the review UI.
    #[must_use]
    pub fn style_preferences(&self) -> Vec<&StylePreference> {
        self.style.values().collect()
    }

    /// Return all project nodes across namespaces for explicit user review/export.
    #[must_use]
    pub fn project_nodes(&self) -> Vec<&ProjectNode> {
        self.project_nodes.values().collect()
    }

    /// Return all project relations across namespaces for explicit user review/export.
    #[must_use]
    pub fn project_relations(&self) -> &[ProjectRelation] {
        &self.project_relations
    }

    /// Store a record within an explicit namespace.
    ///
    /// # Errors
    ///
    /// Returns a [`MemoryError`] when the namespace, record, or embedding is invalid.
    pub fn remember(
        &mut self,
        namespace: MemoryNamespace,
        memory: Memory,
        embedding: Option<Vec<f32>>,
    ) -> Result<(), MemoryError> {
        namespace.validate()?;
        let id = memory.id;
        self.store.upsert(memory, embedding)?;
        self.scopes.insert(id, namespace);
        Ok(())
    }

    /// Recall only from selected namespaces. Global memory is not implicitly
    /// mixed into a project or conversation unless the caller selects it.
    #[must_use]
    pub fn recall_scoped(
        &self,
        namespaces: &BTreeSet<MemoryNamespace>,
        query: &str,
        query_embedding: Option<&[f32]>,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Vec<RecallResult> {
        let mut results = self
            .store
            .recall(query, query_embedding, self.store.memories.len(), now)
            .into_iter()
            .filter(|result| {
                self.scopes
                    .get(&result.memory.id)
                    .is_some_and(|scope| namespaces.contains(scope))
            })
            .collect::<Vec<_>>();
        results.truncate(limit);
        results
    }

    /// Add a reviewable style preference with explicit provenance.
    ///
    /// # Errors
    ///
    /// Returns a [`MemoryError`] when the namespace or provenance is invalid.
    pub fn propose_style(&mut self, preference: StylePreference) -> Result<(), MemoryError> {
        preference.namespace.validate()?;
        if preference.source_event_ids.is_empty() {
            return Err(MemoryError::MissingProvenance);
        }
        self.style.insert(preference.id, preference);
        Ok(())
    }

    /// Accept or reject one proposed style preference.
    ///
    /// # Errors
    ///
    /// Returns a [`MemoryError`] when the preference does not exist.
    pub fn review_style(&mut self, id: Uuid, accept: bool) -> Result<(), MemoryError> {
        let preference = self.style.get_mut(&id).ok_or(MemoryError::Missing(id))?;
        if accept {
            preference.reviewed = true;
        } else {
            self.style.remove(&id);
        }
        Ok(())
    }

    #[must_use]
    pub fn style_for(&self, namespace: &MemoryNamespace) -> Vec<&StylePreference> {
        self.style
            .values()
            .filter(|preference| preference.reviewed && &preference.namespace == namespace)
            .collect()
    }

    /// Insert or replace a project-graph node.
    ///
    /// # Errors
    ///
    /// Returns a [`MemoryError`] when its namespace or provenance is invalid.
    pub fn upsert_project_node(&mut self, node: ProjectNode) -> Result<(), MemoryError> {
        node.namespace.validate()?;
        if node.source_event_ids.is_empty() {
            return Err(MemoryError::MissingProvenance);
        }
        self.project_nodes.insert(node.id, node);
        Ok(())
    }

    /// Add a traceable relation between existing project nodes.
    ///
    /// # Errors
    ///
    /// Returns a [`MemoryError`] when an endpoint is missing or provenance is absent.
    pub fn link_project_nodes(&mut self, relation: ProjectRelation) -> Result<(), MemoryError> {
        for id in [relation.from, relation.to] {
            if !self.project_nodes.contains_key(&id) {
                return Err(MemoryError::MissingProjectNode(id));
            }
        }
        if relation.source_event_ids.is_empty() {
            return Err(MemoryError::MissingProvenance);
        }
        if !self.project_relations.contains(&relation) {
            self.project_relations.push(relation);
        }
        Ok(())
    }

    #[must_use]
    pub fn project_graph(
        &self,
        namespace: &MemoryNamespace,
    ) -> (Vec<&ProjectNode>, Vec<&ProjectRelation>) {
        let nodes = self
            .project_nodes
            .values()
            .filter(|node| &node.namespace == namespace)
            .collect::<Vec<_>>();
        let node_ids = nodes.iter().map(|node| node.id).collect::<BTreeSet<_>>();
        let relations = self
            .project_relations
            .iter()
            .filter(|relation| node_ids.contains(&relation.from) && node_ids.contains(&relation.to))
            .collect();
        (nodes, relations)
    }

    /// Export one namespace for user review or deletion without leaking others.
    #[must_use]
    pub fn export_namespace(&self, namespace: &MemoryNamespace) -> Vec<Memory> {
        self.store
            .memories
            .values()
            .filter(|memory| self.scopes.get(&memory.id) == Some(namespace))
            .cloned()
            .collect()
    }

    pub fn delete_namespace(&mut self, namespace: &MemoryNamespace) -> usize {
        let ids = self
            .scopes
            .iter()
            .filter_map(|(id, scope)| (scope == namespace).then_some(*id))
            .collect::<Vec<_>>();
        for id in &ids {
            let _ = self.store.delete(*id);
            self.scopes.remove(id);
        }
        self.style
            .retain(|_, preference| &preference.namespace != namespace);
        let removed_nodes = self
            .project_nodes
            .iter()
            .filter_map(|(id, node)| (&node.namespace == namespace).then_some(*id))
            .collect::<BTreeSet<_>>();
        self.project_nodes
            .retain(|id, _| !removed_nodes.contains(id));
        self.project_relations.retain(|relation| {
            !removed_nodes.contains(&relation.from) && !removed_nodes.contains(&relation.to)
        });
        ids.len()
    }
}

impl Default for PersistentMemory {
    fn default() -> Self {
        Self::new(EmbeddingModel::default())
    }
}

/// Embedding boundary used by local ONNX, `CoreML`, `DirectML`, or remote adapters.
pub trait TextEmbedder: Send + Sync {
    fn model(&self) -> EmbeddingModel;
    /// Embed one non-empty text value into the model's fixed dimensions.
    ///
    /// # Errors
    ///
    /// Returns a [`MemoryError`] when input or vector generation is invalid.
    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError>;
}

/// Offline deterministic feature-hashing fallback. It is available everywhere,
/// requires no network or model download, and is intentionally labeled so the
/// UI never presents it as a neural semantic model.
#[derive(Clone, Debug)]
pub struct FeatureHashEmbedder {
    dimensions: usize,
}

impl FeatureHashEmbedder {
    #[must_use]
    pub fn new(dimensions: usize) -> Self {
        Self {
            dimensions: dimensions.max(8),
        }
    }
}

impl TextEmbedder for FeatureHashEmbedder {
    fn model(&self) -> EmbeddingModel {
        EmbeddingModel {
            id: "feature-hash-local".into(),
            version: "1".into(),
            dimensions: self.dimensions,
            license: "Apache-2.0".into(),
        }
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        let mut vector = vec![0.0_f32; self.dimensions];
        for token in tokenize(text) {
            let mut hash = 0xcbf2_9ce4_8422_2325_u64;
            for byte in token.bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            let index = usize::try_from(hash % self.dimensions as u64).unwrap_or(0);
            vector[index] += 1.0;
        }
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > f32::EPSILON {
            for value in &mut vector {
                *value /= norm;
            }
        }
        Ok(vector)
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

    #[test]
    fn namespaces_isolate_projects_and_can_be_deleted() {
        let embedder = FeatureHashEmbedder::new(32);
        let mut persistent = PersistentMemory::new(embedder.model());
        let atlas = MemoryNamespace::Project("atlas".into());
        let other = MemoryNamespace::Project("other".into());
        let first = Memory::explicit_user("Atlas uses Rust", MemoryTier::Project, "e1");
        let second = Memory::explicit_user("Other uses Python", MemoryTier::Project, "e2");
        persistent
            .remember(
                atlas.clone(),
                first,
                Some(embedder.embed("Atlas uses Rust").unwrap()),
            )
            .unwrap();
        persistent
            .remember(
                other.clone(),
                second,
                Some(embedder.embed("Other uses Python").unwrap()),
            )
            .unwrap();
        let selected = [atlas.clone()].into_iter().collect();
        let query = embedder.embed("Atlas Rust").unwrap();
        let results =
            persistent.recall_scoped(&selected, "Atlas Rust", Some(&query), 10, Utc::now());
        assert_eq!(results.len(), 1);
        assert_eq!(persistent.delete_namespace(&atlas), 1);
        assert!(persistent.export_namespace(&atlas).is_empty());
    }

    #[test]
    fn writing_style_and_project_graph_require_review_and_provenance() {
        let namespace = MemoryNamespace::Project("agent".into());
        let mut persistent = PersistentMemory::default();
        let style_id = Uuid::now_v7();
        persistent
            .propose_style(StylePreference {
                id: style_id,
                namespace: namespace.clone(),
                description: "Prefer concise release notes".into(),
                examples: vec!["Fixed session selection.".into()],
                source_event_ids: vec!["e1".into()],
                confidence: 0.9,
                reviewed: false,
            })
            .unwrap();
        assert!(persistent.style_for(&namespace).is_empty());
        persistent.review_style(style_id, true).unwrap();
        assert_eq!(persistent.style_for(&namespace).len(), 1);

        let repo = ProjectNode {
            id: Uuid::now_v7(),
            namespace: namespace.clone(),
            kind: "repository".into(),
            name: "personal-agent".into(),
            attributes: BTreeMap::new(),
            source_event_ids: vec!["e2".into()],
        };
        let service = ProjectNode {
            id: Uuid::now_v7(),
            namespace: namespace.clone(),
            kind: "service".into(),
            name: "desktop".into(),
            attributes: BTreeMap::new(),
            source_event_ids: vec!["e3".into()],
        };
        persistent.upsert_project_node(repo.clone()).unwrap();
        persistent.upsert_project_node(service.clone()).unwrap();
        persistent
            .link_project_nodes(ProjectRelation {
                from: repo.id,
                relation: "contains".into(),
                to: service.id,
                source_event_ids: vec!["e4".into()],
            })
            .unwrap();
        let (nodes, relations) = persistent.project_graph(&namespace);
        assert_eq!((nodes.len(), relations.len()), (2, 1));
    }
}
