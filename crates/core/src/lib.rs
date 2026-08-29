//! Native composition root. UI and CLI share these handlers.

mod artifacts;
mod cli;
mod config;
mod conversation;
mod release;
mod research;
mod usage;

pub use cli::{CliResponse, run_cli};

pub use artifacts::{
    Artifact, ArtifactError, ArtifactKind, ArtifactRepository, ArtifactVersion, ArtifactWorkspace,
    SourceLink, Whiteboard, WhiteboardCard, sanitized_html_report, terminal_safe_text,
};
pub use config::{
    AgentConfig, AutomationConfig, BrowserConfig, CONFIG_SCHEMA, ConfigError, ConfigFileError,
    ConfigLoad, KeychainAlias, MemoryConfig, NotificationConfig, PersonaConfig,
    PersonalAgentConfig, PrivacyConfig, RiskAcknowledgement, RiskLevel, RuntimeConfig, UiConfig,
    UpdateConfig, VoiceConfig, WorkspaceConfig, default_config_toml, load_or_initialize_config,
    parse_config,
};
pub use conversation::{
    ControlState, ConversationContext, ConversationError, ConversationState, InputModality,
    ListeningMode, MessageDispatch, MicrophonePrivacy, ModelSelection, StopEffects,
};
pub use personal_agent_memory::{
    FeatureHashEmbedder, Memory, MemoryNamespace, MemoryStore, MemoryTier, MemoryTrust,
    PersistentMemory, ProjectNode, ProjectRelation, RecallResult, StylePreference, TextEmbedder,
};
pub use release::{
    ExportDisposition, PersonalDataDisposition, ReleaseArtifact, ReleaseChannel, ReleaseError,
    SignedReleaseManifest, UninstallPlan, UpdateState, UpdateTransaction,
};
pub use research::{
    Citation, Claim, Contradiction, ResearchError, ResearchProject, ResearchReport,
};
pub use usage::{
    BudgetLimits, BudgetResource, BudgetState, CostStatus, EgressRecord, EgressSource,
    ProviderUsageRecord, ReportedCost, TokenUsage, UsageAggregate, UsageError, UsageLedger,
};

use personal_agent_contracts::proto::EventEnvelope;
use personal_agent_runtime::{AgentRuntime, RuntimeError, RuntimeHealth};
use personal_agent_storage::{EventStore, StorageError};
pub use personal_agent_storage::{
    SupervisorActivityCheckpoint, SupervisorCheckpointUpdate, SupervisorRecoveryCheckpoint,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use thiserror::Error;

const PROJECTION_CHECKPOINT_INTERVAL: u64 = 1_000;

/// Rebuildable UI projection derived exclusively from events.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppProjection {
    pub last_sequence: u64,
    pub active_profile: String,
    pub active_session: Option<String>,
    pub goals_total: u64,
    pub tasks_running: u64,
    pub approvals_waiting: u64,
    pub microphone_active: bool,
    pub runtime_healthy: bool,
    pub unclean_shutdowns: u64,
    pub recovered_unclean_run: bool,
    pub recent_events: Vec<ProjectedEvent>,
}

/// Content-free event summary safe for exact activity rendering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectedEvent {
    pub sequence: u64,
    pub event_type: String,
    pub origin: String,
}

impl AppProjection {
    /// Apply one event. Unknown additive event types are intentionally ignored.
    ///
    /// # Errors
    ///
    /// Returns an error for sequence regression or malformed event payload.
    #[allow(clippy::too_many_lines)] // Projection transitions stay exhaustive and visibly ordered.
    pub fn apply(&mut self, event: &EventEnvelope) -> Result<(), CoreError> {
        if event.monotonic_sequence <= self.last_sequence {
            return Err(CoreError::SequenceRegression {
                previous: self.last_sequence,
                next: event.monotonic_sequence,
            });
        }
        self.last_sequence = event.monotonic_sequence;
        self.active_profile.clone_from(&event.profile_id);
        self.recent_events.push(ProjectedEvent {
            sequence: event.monotonic_sequence,
            event_type: event.r#type.clone(),
            origin: event.origin.clone(),
        });
        if self.recent_events.len() > 50 {
            self.recent_events.remove(0);
        }
        match event.r#type.as_str() {
            "goal.created" => self.goals_total += 1,
            "task.started" => self.tasks_running += 1,
            "task.completed" | "task.failed" | "task.cancelled" | "task.paused" => {
                let payload = event.payload()?;
                if payload
                    .get("was_running")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                {
                    self.tasks_running = self.tasks_running.saturating_sub(1);
                }
                if payload
                    .get("approval_resolved")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    self.approvals_waiting = self.approvals_waiting.saturating_sub(1);
                }
            }
            "task.retry_requested" => {
                if event
                    .payload()?
                    .get("approval_resolved")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    self.approvals_waiting = self.approvals_waiting.saturating_sub(1);
                }
            }
            "goal.paused" | "goal.cancelled" | "goal.retry_requested" => {
                let payload = event.payload()?;
                self.tasks_running = self.tasks_running.saturating_sub(
                    payload
                        .get("running_tasks_stopped")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                self.approvals_waiting = self.approvals_waiting.saturating_sub(
                    payload
                        .get("approvals_resolved")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
            }
            "approval.requested" => {
                let payload = event.payload()?;
                self.approvals_waiting += 1;
                if payload.get("task_id").is_some() {
                    self.tasks_running = self.tasks_running.saturating_sub(1);
                }
            }
            "approval.resolved" => {
                self.approvals_waiting = self.approvals_waiting.saturating_sub(1);
                if event
                    .payload()?
                    .get("resumed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    self.tasks_running += 1;
                }
            }
            "audio.privacy_state" => {
                self.microphone_active = event
                    .payload()?
                    .get("active")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            }
            "runtime.health" => {
                self.runtime_healthy = event
                    .payload()?
                    .get("healthy")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            }
            "lifecycle.started" => {
                self.recovered_unclean_run = event
                    .payload()?
                    .get("previous_unclean_run")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if self.recovered_unclean_run {
                    self.unclean_shutdowns += 1;
                }
            }
            _ => {}
        }
        if let Some(session) = &event.session_id {
            self.active_session = Some(session.clone());
        }
        Ok(())
    }
}

/// Encrypted per-profile state owned by native code and exposed through narrow IPC.
pub struct ProfileState {
    store: EventStore,
    projection: AppProjection,
    profile_id: String,
}

impl ProfileState {
    /// Open an encrypted profile database using a key held by the OS secret store.
    ///
    /// A fresh random key is created only when the reference is absent. Secret-store
    /// outages fail closed instead of creating a second, unrecoverable database.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe profile ID, missing parent permissions,
    /// unavailable secret storage, encrypted-store failure, or event corruption.
    pub fn open(
        database_path: &Path,
        profile_id: &str,
        secrets: &dyn personal_agent_platform::SecretStore,
    ) -> Result<Self, CoreError> {
        if profile_id.is_empty()
            || !profile_id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err(CoreError::InvalidProfileId);
        }
        if let Some(parent) = database_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let reference = personal_agent_platform::SecretReference {
            service: "dev.personal-agent.storage".to_owned(),
            account: format!("database-{profile_id}"),
        };
        let key = match secrets.get(&reference) {
            Ok(key) => key,
            Err(personal_agent_platform::SecretStoreError::Missing) => {
                let key = secrecy::SecretString::from(format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                ));
                secrets.put(&reference, &key)?;
                key
            }
            Err(error) => return Err(error.into()),
        };
        let store = EventStore::open(database_path, &key)?;
        let projection = rebuild_projection_from(&store)?;
        Ok(Self {
            store,
            projection,
            profile_id: profile_id.to_owned(),
        })
    }

    #[must_use]
    pub fn projection(&self) -> &AppProjection {
        &self.projection
    }

    /// Persist the current application projection for a clean-shutdown replay boundary.
    ///
    /// # Errors
    /// Returns an encrypted storage or serialization failure.
    pub fn checkpoint_projection(&mut self) -> Result<(), CoreError> {
        self.store
            .save_projection_checkpoint(&self.projection, self.projection.last_sequence)?;
        Ok(())
    }

    fn apply_persisted_event(&mut self, event: &EventEnvelope) -> Result<AppProjection, CoreError> {
        self.projection.apply(event)?;
        if self
            .projection
            .last_sequence
            .is_multiple_of(PROJECTION_CHECKPOINT_INTERVAL)
        {
            self.checkpoint_projection()?;
        }
        Ok(self.projection.clone())
    }

    /// Load the encrypted durable memory index for this profile.
    ///
    /// # Errors
    ///
    /// Returns an encrypted storage or serialization failure.
    pub fn memory_snapshot(&self) -> Result<Option<MemoryStore>, CoreError> {
        Ok(self.store.memory_snapshot(&self.profile_id)?)
    }

    /// Persist the complete encrypted durable memory index for this profile.
    ///
    /// # Errors
    ///
    /// Returns an encrypted storage or serialization failure.
    pub fn save_memory_snapshot(&mut self, memory: &MemoryStore) -> Result<(), CoreError> {
        self.store.save_memory_snapshot(&self.profile_id, memory)?;
        Ok(())
    }

    /// Load the complete encrypted namespaced memory system for this profile.
    ///
    /// # Errors
    ///
    /// Returns an encrypted storage or serialization failure.
    pub fn persistent_memory_snapshot(&self) -> Result<Option<PersistentMemory>, CoreError> {
        Ok(self.store.persistent_memory_snapshot(&self.profile_id)?)
    }

    /// Persist the complete encrypted namespaced memory system and its legacy
    /// fact/vector view atomically per snapshot kind.
    ///
    /// # Errors
    ///
    /// Returns an encrypted storage or serialization failure.
    pub fn save_persistent_memory_snapshot(
        &mut self,
        memory: &PersistentMemory,
    ) -> Result<(), CoreError> {
        self.store
            .save_persistent_memory_snapshot(&self.profile_id, memory)?;
        self.store
            .save_memory_snapshot(&self.profile_id, &memory.store)?;
        Ok(())
    }

    /// Load the encrypted durable automation scheduler for this profile.
    ///
    /// # Errors
    ///
    /// Returns an encrypted storage or serialization failure.
    pub fn scheduler_snapshot(
        &self,
    ) -> Result<Option<personal_agent_automation::SchedulerSnapshot>, CoreError> {
        Ok(self.store.scheduler_snapshot(&self.profile_id)?)
    }

    /// Persist the complete encrypted automation scheduler for this profile.
    ///
    /// # Errors
    ///
    /// Returns an encrypted storage or serialization failure.
    pub fn save_scheduler_snapshot(
        &mut self,
        scheduler: &personal_agent_automation::SchedulerSnapshot,
    ) -> Result<(), CoreError> {
        self.store
            .save_scheduler_snapshot(&self.profile_id, scheduler)?;
        Ok(())
    }

    /// Load one encrypted durable goal supervisor by stable goal ID.
    ///
    /// # Errors
    /// Returns an encrypted storage or serialization failure.
    pub fn supervisor_snapshot(
        &self,
        goal_id: uuid::Uuid,
    ) -> Result<Option<personal_agent_agent::SupervisorSnapshot>, CoreError> {
        Ok(self.store.supervisor_snapshot(goal_id)?)
    }

    /// Load all enriched supervisor snapshots used as goal-replay bases.
    ///
    /// # Errors
    /// Returns an encrypted storage or serialization failure.
    pub fn supervisor_recovery_checkpoints(
        &self,
    ) -> Result<Vec<SupervisorRecoveryCheckpoint>, CoreError> {
        Ok(self.store.supervisor_recovery_checkpoints()?)
    }

    /// Persist a recovered supervisor snapshot without inventing a domain transition.
    ///
    /// # Errors
    /// Returns an encrypted storage or serialization failure.
    pub fn save_supervisor_snapshot(
        &mut self,
        snapshot: &personal_agent_agent::SupervisorSnapshot,
    ) -> Result<(), CoreError> {
        self.store.save_supervisor_snapshot(snapshot)?;
        Ok(())
    }

    /// Atomically persist a supervisor transition and its projection event.
    ///
    /// # Errors
    /// Returns event, serialization, encrypted-storage, or projection failures.
    pub fn record_supervisor_event(
        &mut self,
        snapshot: &personal_agent_agent::SupervisorSnapshot,
        event_type: &str,
        payload: &Value,
    ) -> Result<AppProjection, CoreError> {
        let mut event = EventEnvelope::new(
            self.projection.last_sequence + 1,
            "goal-supervisor",
            &self.profile_id,
            event_type,
            payload,
        )?;
        event.goal_id = Some(snapshot.graph.goal_id.to_string());
        event.task_id = payload
            .get("task_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.store
            .save_supervisor_snapshot_and_event(snapshot, &event)?;
        self.apply_persisted_event(&event)
    }

    /// Append and project a supervisor event without rewriting its recovery
    /// checkpoint. A debounced checkpoint update must follow; until it does,
    /// restart recovery safely replays this durable event from the prior boundary.
    ///
    /// # Errors
    /// Returns event, encrypted-storage, or projection failures.
    pub fn append_supervisor_event(
        &mut self,
        snapshot: &personal_agent_agent::SupervisorSnapshot,
        event_type: &str,
        payload: &Value,
    ) -> Result<(AppProjection, EventEnvelope), CoreError> {
        let mut event = EventEnvelope::new(
            self.projection.last_sequence + 1,
            "goal-supervisor",
            &self.profile_id,
            event_type,
            payload,
        )?;
        event.goal_id = Some(snapshot.graph.goal_id.to_string());
        event.task_id = payload
            .get("task_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.store.append(&event)?;
        let projection = self.apply_persisted_event(&event)?;
        Ok((projection, event))
    }

    /// Atomically advance debounced supervisor checkpoints over already-durable
    /// event tails.
    ///
    /// # Errors
    /// Returns JSON or encrypted-storage failures.
    pub fn save_supervisor_checkpoint_updates(
        &mut self,
        updates: &[SupervisorCheckpointUpdate],
    ) -> Result<(), CoreError> {
        self.store.save_supervisor_checkpoint_updates(updates)?;
        Ok(())
    }

    /// Load encrypted artifact-library and whiteboard metadata.
    ///
    /// # Errors
    /// Returns encrypted storage or serialization failures.
    pub fn artifact_workspace_snapshot(&self) -> Result<Option<ArtifactWorkspace>, CoreError> {
        Ok(self
            .store
            .runtime_snapshot("artifact-workspace-v1", &self.profile_id)?)
    }

    /// Persist encrypted artifact-library and whiteboard metadata.
    ///
    /// # Errors
    /// Returns encrypted storage or serialization failures.
    pub fn save_artifact_workspace_snapshot(
        &mut self,
        workspace: &ArtifactWorkspace,
    ) -> Result<(), CoreError> {
        self.store
            .save_runtime_snapshot("artifact-workspace-v1", &self.profile_id, workspace)?;
        Ok(())
    }

    /// Atomically persist artifact metadata and the domain event that projects it.
    ///
    /// # Errors
    /// Returns event, serialization, encrypted-storage, or projection failures.
    pub fn record_artifact_workspace_event(
        &mut self,
        workspace: &ArtifactWorkspace,
        event_type: &str,
        payload: &Value,
    ) -> Result<AppProjection, CoreError> {
        let event = EventEnvelope::new(
            self.projection.last_sequence + 1,
            "desktop-ui",
            &self.profile_id,
            event_type,
            payload,
        )?;
        self.store.save_runtime_snapshot_and_event(
            "artifact-workspace-v1",
            &self.profile_id,
            workspace,
            &event,
        )?;
        self.apply_persisted_event(&event)
    }

    /// Store immutable artifact content in the encrypted content-addressed blob store.
    ///
    /// # Errors
    /// Returns encrypted storage failures.
    pub fn store_artifact_blob(&mut self, bytes: &[u8]) -> Result<String, CoreError> {
        Ok(self.store.store_blob(bytes)?)
    }

    /// Retrieve immutable artifact content by SHA-256 address.
    ///
    /// # Errors
    /// Returns encrypted storage failures or `BlobMissing`.
    pub fn artifact_blob(&self, hash: &str) -> Result<Vec<u8>, CoreError> {
        Ok(self.store.blob(hash)?)
    }

    /// Persist a typed user message and return the resulting projection.
    ///
    /// # Errors
    ///
    /// Returns an error for blank/oversized text or event/storage failure.
    pub fn submit_user_message(&mut self, text: &str) -> Result<AppProjection, CoreError> {
        let text = text.trim();
        if text.is_empty() || text.len() > 65_536 {
            return Err(CoreError::InvalidUserMessage);
        }
        let event = EventEnvelope::new(
            self.projection.last_sequence + 1,
            "desktop-ui",
            &self.profile_id,
            "message.user",
            &serde_json::json!({"text": text}),
        )?;
        self.store.append(&event)?;
        self.apply_persisted_event(&event)
    }

    /// Persist the normalized runtime health state without exposing its endpoint.
    ///
    /// # Errors
    ///
    /// Returns an event or storage failure.
    pub fn record_runtime_health(
        &mut self,
        health: &RuntimeHealth,
    ) -> Result<AppProjection, CoreError> {
        let event = EventEnvelope::new(
            self.projection.last_sequence + 1,
            "runtime-supervisor",
            &self.profile_id,
            "runtime.health",
            &serde_json::json!({
                "healthy": health.healthy,
                "version": health.version,
                "detail": health.detail,
            }),
        )?;
        self.store.append(&event)?;
        self.apply_persisted_event(&event)
    }

    /// Persist process-start recovery state in the encrypted event stream.
    ///
    /// # Errors
    ///
    /// Returns an event or storage failure.
    pub fn record_lifecycle_start(
        &mut self,
        previous_unclean_run: bool,
    ) -> Result<AppProjection, CoreError> {
        let event = EventEnvelope::new(
            self.projection.last_sequence + 1,
            "desktop-host",
            &self.profile_id,
            "lifecycle.started",
            &serde_json::json!({"previous_unclean_run": previous_unclean_run}),
        )?;
        self.store.append(&event)?;
        self.apply_persisted_event(&event)
    }

    /// Persist one normalized runtime event with a profile-global monotonic
    /// sequence so live sessions survive restart and can rebuild the UI.
    ///
    /// # Errors
    ///
    /// Returns an error when the event cannot be persisted or projected.
    pub fn record_runtime_event(
        &mut self,
        mut event: EventEnvelope,
    ) -> Result<AppProjection, CoreError> {
        event.monotonic_sequence = self.projection.last_sequence + 1;
        event.profile_id.clone_from(&self.profile_id);
        let mut usage = self.usage_snapshot()?;
        usage.ingest_runtime_event(&event)?;
        self.store.save_runtime_snapshot_and_event(
            "usage-ledger-v1",
            &self.profile_id,
            &usage,
            &event,
        )?;
        self.apply_persisted_event(&event)
    }

    /// Load encrypted provider accounting and content-free egress state.
    ///
    /// # Errors
    /// Returns an encrypted storage or serialization failure.
    pub fn usage_snapshot(&self) -> Result<UsageLedger, CoreError> {
        Ok(self
            .store
            .runtime_snapshot("usage-ledger-v1", &self.profile_id)?
            .unwrap_or_default())
    }

    /// Atomically append one content-free egress event and update aggregates.
    ///
    /// # Errors
    /// Returns validation, event, serialization, or encrypted-storage failures.
    pub fn record_egress(&mut self, record: EgressRecord) -> Result<AppProjection, CoreError> {
        let mut usage = self.usage_snapshot()?;
        usage.record_egress(record.clone())?;
        let event = EventEnvelope::new(
            self.projection.last_sequence + 1,
            "egress-gateway",
            &self.profile_id,
            "egress.recorded",
            &serde_json::to_value(record)?,
        )?;
        self.store.save_runtime_snapshot_and_event(
            "usage-ledger-v1",
            &self.profile_id,
            &usage,
            &event,
        )?;
        self.apply_persisted_event(&event)
    }

    /// Return persisted events for history/timeline rendering. Secrets remain
    /// filtered at event creation and the renderer never receives the database.
    ///
    /// # Errors
    ///
    /// Returns an error when the encrypted event store cannot be queried.
    pub fn events_after(
        &self,
        sequence: u64,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, CoreError> {
        Ok(self.store.after(sequence, limit.min(1_000))?)
    }

    /// Import one reviewed legacy plan into the encrypted profile and rebuild
    /// the projection so imported history is immediately visible to native state.
    ///
    /// # Errors
    ///
    /// Returns an error when confirmation is absent, preparation fails, or an
    /// encrypted transaction cannot be committed.
    pub fn import_legacy(
        &mut self,
        plan: &personal_agent_migration::MigrationPlan,
        consent: personal_agent_migration::MigrationConsent,
    ) -> Result<personal_agent_migration::MigrationReport, CoreError> {
        let report = match personal_agent_migration::migrate(plan, consent, &mut self.store) {
            Ok(report) => report,
            Err(personal_agent_migration::MigrationRunError::ConfirmationRequired) => {
                return Err(CoreError::MigrationConfirmationRequired);
            }
            Err(personal_agent_migration::MigrationRunError::Migration(error)) => {
                return Err(error.into());
            }
            Err(personal_agent_migration::MigrationRunError::Sink(error)) => {
                return Err(error.into());
            }
        };
        self.store.record_migration_report(&report)?;
        self.projection = rebuild_projection_from(&self.store)?;
        Ok(report)
    }
}

impl Drop for ProfileState {
    fn drop(&mut self) {
        let _ = self
            .store
            .save_projection_checkpoint(&self.projection, self.projection.last_sequence);
    }
}

/// Composition-root failure.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Usage(#[from] UsageError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Event(#[from] personal_agent_contracts::EventError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    SecretStore(#[from] personal_agent_platform::SecretStoreError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Migration(#[from] personal_agent_migration::MigrationError),
    #[error("legacy migration requires explicit confirmation")]
    MigrationConfirmationRequired,
    #[error("legacy source changed after review; run a new dry run")]
    MigrationPlanChanged,
    #[error("profile ID must contain only ASCII letters, digits, '-' or '_'")]
    InvalidProfileId,
    #[error("user message must contain 1 to 65536 bytes after trimming")]
    InvalidUserMessage,
    #[error("event sequence regressed from {previous} to {next}")]
    SequenceRegression { previous: u64, next: u64 },
}

/// Owned application services. Provider credentials and database handles stop here.
pub struct Core<R: AgentRuntime> {
    store: EventStore,
    runtime: R,
    projection: AppProjection,
}

impl<R: AgentRuntime> Core<R> {
    #[must_use]
    pub fn new(store: EventStore, runtime: R) -> Self {
        Self {
            store,
            runtime,
            projection: AppProjection::default(),
        }
    }

    /// Recover the projection before accepting new work, then start the runtime.
    ///
    /// # Errors
    ///
    /// Returns a storage, projection, event-construction, or runtime error.
    pub async fn start(&mut self) -> Result<RuntimeHealth, CoreError> {
        self.rebuild_projection()?;
        let health = self.runtime.start().await?;
        let event = EventEnvelope::new(
            self.projection.last_sequence + 1,
            "core",
            "default",
            "runtime.health",
            &serde_json::json!({"healthy":health.healthy,"version":health.version}),
        )?;
        self.record(&event)?;
        Ok(health)
    }

    /// Stop the owned runtime.
    ///
    /// # Errors
    ///
    /// Returns a runtime shutdown error.
    pub async fn stop(&mut self) -> Result<(), CoreError> {
        self.runtime.stop().await?;
        self.store
            .save_projection_checkpoint(&self.projection, self.projection.last_sequence)?;
        Ok(())
    }

    /// Append and project as one handler operation.
    ///
    /// # Errors
    ///
    /// Returns a storage or projection error.
    pub fn record(&mut self, event: &EventEnvelope) -> Result<(), CoreError> {
        self.store.append(event)?;
        self.projection.apply(event)?;
        if self
            .projection
            .last_sequence
            .is_multiple_of(PROJECTION_CHECKPOINT_INTERVAL)
        {
            self.store
                .save_projection_checkpoint(&self.projection, self.projection.last_sequence)?;
        }
        Ok(())
    }

    /// Rebuild application state solely from committed events.
    ///
    /// # Errors
    ///
    /// Returns a storage or projection error.
    pub fn rebuild_projection(&mut self) -> Result<(), CoreError> {
        self.projection = rebuild_projection_from(&self.store)?;
        Ok(())
    }

    #[must_use]
    pub fn projection(&self) -> &AppProjection {
        &self.projection
    }
}

/// Rebuild application state solely from committed events.
///
/// # Errors
///
/// Returns a storage or event projection error.
pub fn rebuild_projection_from(store: &EventStore) -> Result<AppProjection, CoreError> {
    let last_event_sequence = store.last_sequence()?;
    let checkpoint = store.projection_checkpoint::<AppProjection>()?;
    let (mut projection, mut after) = checkpoint.map_or_else(
        || (AppProjection::default(), 0),
        |checkpoint| {
            let snapshot = checkpoint.projection_snapshot_blob;
            if snapshot.last_sequence == checkpoint.last_sequence
                && checkpoint.last_sequence <= last_event_sequence
            {
                (snapshot, checkpoint.last_sequence)
            } else {
                (AppProjection::default(), 0)
            }
        },
    );
    loop {
        let batch = store.after(after, 512)?;
        if batch.is_empty() {
            break;
        }
        for event in batch {
            after = event.monotonic_sequence;
            projection.apply(&event)?;
        }
    }
    Ok(projection)
}

/// Read-only diagnostics used by both desktop IPC and CLI.
#[must_use]
pub fn diagnostic_snapshot() -> Value {
    serde_json::json!({
        "product":"Personal Agent", "version":env!("CARGO_PKG_VERSION"),
        "platform":std::env::consts::OS, "arch":std::env::consts::ARCH,
        "capabilities":personal_agent_platform::compile_time_capabilities(),
        "opencode":{"pinned":personal_agent_runtime::OPENCODE_VERSION,"topology":"authenticated-loopback-sidecar"}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_agent_platform::{SecretReference, SecretStore, SecretStoreError};
    use personal_agent_runtime::FakeRuntime;
    use secrecy::{ExposeSecret, SecretString};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSecretStore(Mutex<Option<String>>);

    impl SecretStore for FakeSecretStore {
        fn put(
            &self,
            _reference: &SecretReference,
            value: &SecretString,
        ) -> Result<(), SecretStoreError> {
            *self.0.lock().expect("secret lock") = Some(value.expose_secret().to_owned());
            Ok(())
        }

        fn get(&self, _reference: &SecretReference) -> Result<SecretString, SecretStoreError> {
            self.0
                .lock()
                .expect("secret lock")
                .clone()
                .map(SecretString::from)
                .ok_or(SecretStoreError::Missing)
        }

        fn delete(&self, _reference: &SecretReference) -> Result<(), SecretStoreError> {
            *self.0.lock().expect("secret lock") = None;
            Ok(())
        }
    }

    #[tokio::test]
    async fn projection_rebuilds_from_persisted_events() {
        let store =
            EventStore::open_in_memory(&SecretString::from("test-key".to_owned())).expect("store");
        let mut core = Core::new(store, FakeRuntime::new(vec![]));
        core.start().await.expect("start");
        let event = EventEnvelope::new(2, "ui", "default", "goal.created", &serde_json::json!({}))
            .expect("event");
        core.record(&event).expect("record");
        core.rebuild_projection().expect("rebuild");
        assert_eq!(core.projection().goals_total, 1);
        assert!(core.projection().runtime_healthy);
    }

    #[test]
    fn goal_supervisor_projection_tracks_suspension_and_bulk_control() {
        let mut projection = AppProjection::default();
        let events = [
            (1, "task.started", serde_json::json!({"task_id": "task-1"})),
            (
                2,
                "approval.requested",
                serde_json::json!({"task_id": "task-1"}),
            ),
            (
                3,
                "approval.resolved",
                serde_json::json!({"task_id": "task-1", "resumed": true}),
            ),
            (
                4,
                "goal.paused",
                serde_json::json!({
                    "running_tasks_stopped": 1,
                    "approvals_resolved": 0,
                }),
            ),
        ];
        for (sequence, event_type, payload) in events {
            let event =
                EventEnvelope::new(sequence, "goal-supervisor", "default", event_type, &payload)
                    .expect("event");
            projection.apply(&event).expect("projection");
        }
        assert_eq!(projection.tasks_running, 0);
        assert_eq!(projection.approvals_waiting, 0);

        let queued_cancel = EventEnvelope::new(
            5,
            "goal-supervisor",
            "default",
            "task.cancelled",
            &serde_json::json!({"was_running": false, "approval_resolved": false}),
        )
        .expect("queued cancel");
        projection.apply(&queued_cancel).expect("queued projection");
        assert_eq!(projection.tasks_running, 0);
    }

    #[test]
    fn profile_state_reopens_with_keychain_key_and_rebuilds() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("profile.db");
        let secrets = FakeSecretStore::default();
        {
            let mut state = ProfileState::open(&database, "default", &secrets).expect("open");
            state.submit_user_message("hello").expect("message");
            state
                .record_lifecycle_start(true)
                .expect("lifecycle recovery");
            state
                .record_runtime_health(&RuntimeHealth {
                    healthy: true,
                    version: personal_agent_runtime::OPENCODE_VERSION.to_owned(),
                    detail: "test".to_owned(),
                })
                .expect("health");
            assert_eq!(state.projection().last_sequence, 3);
        }
        let reopened = ProfileState::open(&database, "default", &secrets).expect("reopen");
        assert_eq!(reopened.projection().last_sequence, 3);
        assert_eq!(reopened.projection().active_profile, "default");
        assert!(reopened.projection().runtime_healthy);
        assert_eq!(reopened.projection().unclean_shutdowns, 1);
        assert!(reopened.projection().recovered_unclean_run);
    }

    #[test]
    fn profile_drop_persists_a_clean_shutdown_projection_checkpoint() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("clean-shutdown.db");
        let secrets = FakeSecretStore::default();
        {
            let mut state = ProfileState::open(&database, "default", &secrets).expect("open");
            state.submit_user_message("checkpoint me").expect("event");
        }
        let reference = SecretReference {
            service: "dev.personal-agent.storage".to_owned(),
            account: "database-default".to_owned(),
        };
        let key = secrets.get(&reference).expect("stored key");
        let store = EventStore::open(&database, &key).expect("reopen storage");
        let checkpoint = store
            .projection_checkpoint::<AppProjection>()
            .expect("load checkpoint")
            .expect("checkpoint exists");
        assert_eq!(checkpoint.last_sequence, 1);
        assert_eq!(checkpoint.projection_snapshot_blob.last_sequence, 1);
        assert_eq!(
            checkpoint.projection_snapshot_blob.active_profile,
            "default"
        );
    }

    #[test]
    fn profile_state_checkpoints_each_thousand_events() {
        let store =
            EventStore::open_in_memory(&SecretString::from("test-key".to_owned())).expect("store");
        let mut state = ProfileState {
            store,
            projection: AppProjection::default(),
            profile_id: "default".into(),
        };
        for sequence in 1..=1_000 {
            state
                .submit_user_message(&format!("message {sequence}"))
                .expect("event");
        }
        let checkpoint = state
            .store
            .projection_checkpoint::<AppProjection>()
            .expect("load checkpoint")
            .expect("checkpoint exists");
        assert_eq!(checkpoint.last_sequence, 1_000);
        assert_eq!(checkpoint.projection_snapshot_blob, state.projection);
    }

    #[test]
    fn profile_memory_snapshot_survives_reopen() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("memory-profile.db");
        let secrets = FakeSecretStore::default();
        let remembered = Memory::explicit_user(
            "project root is /srv/personal-agent",
            MemoryTier::Project,
            "test-event",
        );
        {
            let mut state = ProfileState::open(&database, "default", &secrets).expect("open");
            let mut memory = MemoryStore::default();
            memory.upsert(remembered.clone(), None).expect("insert");
            state.save_memory_snapshot(&memory).expect("save memory");
        }
        let reopened = ProfileState::open(&database, "default", &secrets).expect("reopen");
        let memory = reopened
            .memory_snapshot()
            .expect("load memory")
            .expect("snapshot");
        assert_eq!(memory.get(remembered.id), Some(&remembered));
    }

    #[test]
    fn profile_scheduler_snapshot_survives_reopen() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("scheduler-profile.db");
        let secrets = FakeSecretStore::default();
        let snapshot = personal_agent_automation::SchedulerSnapshot {
            last_evaluated_at: Some(chrono::Utc::now()),
            ..personal_agent_automation::SchedulerSnapshot::default()
        };
        {
            let mut state = ProfileState::open(&database, "default", &secrets).expect("open");
            state
                .save_scheduler_snapshot(&snapshot)
                .expect("save scheduler");
        }
        let reopened = ProfileState::open(&database, "default", &secrets).expect("reopen");
        assert_eq!(
            reopened
                .scheduler_snapshot()
                .expect("load scheduler")
                .expect("snapshot"),
            snapshot
        );
    }

    #[test]
    fn profile_persistent_memory_system_survives_reopen() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("persistent-memory-profile.db");
        let secrets = FakeSecretStore::default();
        let style = StylePreference {
            id: uuid::Uuid::now_v7(),
            namespace: MemoryNamespace::Profile("default".into()),
            description: "Use direct answers".into(),
            examples: vec!["Lead with the result.".into()],
            source_event_ids: vec!["test-event".into()],
            confidence: 1.0,
            reviewed: true,
        };
        {
            let mut state = ProfileState::open(&database, "default", &secrets).expect("open");
            let mut memory = PersistentMemory::default();
            memory.propose_style(style.clone()).expect("style");
            state
                .save_persistent_memory_snapshot(&memory)
                .expect("save memory system");
        }
        let reopened = ProfileState::open(&database, "default", &secrets).expect("reopen");
        let memory = reopened
            .persistent_memory_snapshot()
            .expect("load memory system")
            .expect("snapshot");
        assert_eq!(
            memory.style_for(&MemoryNamespace::Profile("default".into())),
            vec![&style]
        );
    }

    #[test]
    fn artifact_metadata_and_encrypted_blobs_survive_reopen() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("artifact-profile.db");
        let secrets = FakeSecretStore::default();
        let bytes = b"versioned encrypted artifact";
        let (artifact_id, hash) = {
            let mut state = ProfileState::open(&database, "default", &secrets).expect("open");
            let mut workspace = ArtifactWorkspace::default();
            let artifact = workspace
                .repository
                .create("Report", ArtifactKind::Text, "text/plain", bytes, vec![])
                .expect("artifact");
            workspace.whiteboard.add(artifact.id);
            let hash = state.store_artifact_blob(bytes).expect("blob");
            assert_eq!(hash, artifact.versions[0].content_sha256);
            state
                .record_artifact_workspace_event(
                    &workspace,
                    "artifact.created",
                    &serde_json::json!({"id": artifact.id}),
                )
                .expect("snapshot and event");
            assert_eq!(state.projection().last_sequence, 1);
            (artifact.id, hash)
        };
        let reopened = ProfileState::open(&database, "default", &secrets).expect("reopen");
        let workspace = reopened
            .artifact_workspace_snapshot()
            .expect("load")
            .expect("workspace");
        assert!(workspace.repository.get(artifact_id).is_some());
        assert_eq!(reopened.artifact_blob(&hash).expect("blob"), bytes);
    }

    #[test]
    fn profile_imports_reviewed_legacy_history_and_rebuilds_projection() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("profile.db");
        let secrets = FakeSecretStore::default();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/legacy/synthetic-v1");
        let plan = personal_agent_migration::discover(&fixture).expect("plan");
        let mut state = ProfileState::open(&database, "default", &secrets).expect("profile");

        let report = state
            .import_legacy(
                &plan,
                personal_agent_migration::MigrationConsent {
                    copy_personal_data: true,
                    adopt_opencode_auth: false,
                },
            )
            .expect("import");

        assert!(report.summary.imported > 5);
        assert_eq!(state.projection().last_sequence, 2);
        let second = state
            .import_legacy(
                &plan,
                personal_agent_migration::MigrationConsent {
                    copy_personal_data: true,
                    adopt_opencode_auth: false,
                },
            )
            .expect("idempotent import");
        assert_eq!(second.summary.imported, 0);
        assert_eq!(state.projection().last_sequence, 2);
    }
}
