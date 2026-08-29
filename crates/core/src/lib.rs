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
    FeatureHashEmbedder, Memory, MemoryNamespace, MemoryStorageRow, MemoryStore, MemoryTier,
    MemoryTrust, PersistentMemory, PersistentMemoryMetadata, ProjectNode, ProjectRelation,
    RecallResult, StylePreference, TextEmbedder,
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
use personal_agent_storage::{
    EgressWrite, EventStore, StorageError, UsageFactWrite, UsagePageQuery,
};
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
    usage: UsageLedger,
    legacy_usage: UsageLedger,
}

/// Bounded provider/egress details plus SQL-rebuilt aggregates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsagePage {
    pub ledger: UsageLedger,
    pub usage_total: u64,
    pub egress_total: u64,
    pub limit: usize,
    pub offset: usize,
}

/// Detail filters for the append-only usage/egress page.
#[derive(Clone, Copy, Debug)]
pub struct UsagePageRequest<'a> {
    pub limit: usize,
    pub offset: usize,
    pub from: Option<&'a str>,
    pub to: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub session: Option<&'a str>,
    pub source: Option<&'a str>,
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
        let legacy_usage = store
            .runtime_snapshot("usage-ledger-v1", profile_id)?
            .unwrap_or_default();
        let usage = rebuild_usage_from(&store, profile_id, &legacy_usage)?;
        Ok(Self {
            store,
            projection,
            profile_id: profile_id.to_owned(),
            usage,
            legacy_usage,
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
        let mutation = self.usage.ingest_runtime_event_delta(&event)?;
        self.usage.clear_detail_records();
        let fact_json = mutation
            .fact
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let usage_facts = mutation
            .fact
            .as_ref()
            .zip(fact_json.as_deref())
            .map_or_else(Vec::new, |(fact, body_json)| {
                vec![UsageFactWrite {
                    id: &fact.event_id,
                    day: &fact.day_utc,
                    session_id: Some(&fact.session_id),
                    body_json,
                }]
            });
        let egress_json = mutation
            .egress
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let egress_id = mutation.egress.as_ref().map(|record| record.id.to_string());
        let egress_day = mutation
            .egress
            .as_ref()
            .map(|record| record.at.format("%Y-%m-%d").to_string());
        let egress = mutation
            .egress
            .as_ref()
            .zip(egress_id.as_deref())
            .zip(egress_day.as_deref())
            .zip(egress_json.as_deref())
            .map_or_else(Vec::new, |(((record, id), day), body_json)| {
                vec![EgressWrite {
                    id,
                    day,
                    session_id: record.session_id.as_deref(),
                    destination: &record.destination,
                    size_bytes: record.size_bytes,
                    body_json,
                }]
            });
        if let Err(error) = self.store.append_usage_event(&event, &usage_facts, &egress) {
            self.usage = rebuild_usage_from(&self.store, &self.profile_id, &self.legacy_usage)?;
            return Err(error.into());
        }
        self.apply_persisted_event(&event)
    }

    /// Load encrypted provider accounting and content-free egress state.
    ///
    /// # Errors
    /// Returns an encrypted storage or serialization failure.
    pub fn usage_snapshot(&self) -> Result<UsageLedger, CoreError> {
        Ok(self.usage.aggregate_snapshot())
    }

    /// Return one bounded detail page and aggregates for the requested UTC-day
    /// range. `from` and `to` are inclusive `YYYY-MM-DD` values.
    ///
    /// # Errors
    /// Returns encrypted storage or malformed-record errors.
    pub fn usage_page(&self, request: UsagePageRequest<'_>) -> Result<UsagePage, CoreError> {
        let aggregates = self
            .store
            .usage_aggregates(&self.profile_id, request.from, request.to)?;
        let fetch_limit = request.offset.saturating_add(request.limit);
        let stored = self.store.usage_page(
            &self.profile_id,
            UsagePageQuery {
                limit: fetch_limit,
                offset: 0,
                from: request.from,
                to: request.to,
                provider: request.provider,
                model: request.model,
                session: request.session,
                source: request.source,
            },
        )?;
        let mut ledger = UsageLedger::from_stored(aggregates, &[])?;
        ledger.merge_aggregates_from(
            &self
                .legacy_usage
                .aggregates_in_range(request.from, request.to),
        );
        let mut records = stored
            .usage_facts_json
            .iter()
            .map(|body| UsageLedger::fact_from_json(body))
            .collect::<Result<Vec<_>, _>>()?;
        let legacy_records = self
            .legacy_usage
            .records
            .iter()
            .filter(|record| legacy_usage_matches(record, request))
            .cloned()
            .collect::<Vec<_>>();
        let legacy_usage_total = u64::try_from(legacy_records.len()).unwrap_or(u64::MAX);
        records.extend(legacy_records);
        records.sort_by(|left, right| right.at.cmp(&left.at).then_with(|| right.id.cmp(&left.id)));
        ledger.records = records
            .into_iter()
            .skip(request.offset)
            .take(request.limit)
            .collect();

        let mut egress = stored
            .egress_json
            .iter()
            .map(|body| serde_json::from_str(body))
            .collect::<Result<Vec<_>, _>>()?;
        let legacy_egress = self
            .legacy_usage
            .egress
            .iter()
            .filter(|record| legacy_egress_matches(record, request))
            .cloned()
            .collect::<Vec<_>>();
        let legacy_egress_total = u64::try_from(legacy_egress.len()).unwrap_or(u64::MAX);
        egress.extend(legacy_egress);
        egress.sort_by(|left, right| right.at.cmp(&left.at).then_with(|| right.id.cmp(&left.id)));
        ledger.egress = egress
            .into_iter()
            .skip(request.offset)
            .take(request.limit)
            .collect();
        Ok(UsagePage {
            ledger,
            usage_total: stored.usage_total.saturating_add(legacy_usage_total),
            egress_total: stored.egress_total.saturating_add(legacy_egress_total),
            limit: request.limit,
            offset: request.offset,
        })
    }

    /// Atomically append one content-free egress event and update aggregates.
    ///
    /// # Errors
    /// Returns validation, event, serialization, or encrypted-storage failures.
    #[allow(clippy::needless_pass_by_value)] // Existing effect gateways transfer ownership here.
    pub fn record_egress(&mut self, record: EgressRecord) -> Result<AppProjection, CoreError> {
        let event = EventEnvelope::new(
            self.projection.last_sequence + 1,
            "egress-gateway",
            &self.profile_id,
            "egress.recorded",
            &serde_json::to_value(&record)?,
        )?;
        let exists = self.usage.has_seen_egress(record.id)
            || self
                .store
                .egress_exists(&self.profile_id, &record.id.to_string())?;
        if !exists {
            self.usage.record_egress(record.clone())?;
            self.usage.clear_detail_records();
        }
        let body_json = serde_json::to_string(&record)?;
        let id = record.id.to_string();
        let day = record.at.format("%Y-%m-%d").to_string();
        let rows = if exists {
            Vec::new()
        } else {
            vec![EgressWrite {
                id: &id,
                day: &day,
                session_id: record.session_id.as_deref(),
                destination: &record.destination,
                size_bytes: record.size_bytes,
                body_json: &body_json,
            }]
        };
        if let Err(error) = self.store.append_usage_event(&event, &[], &rows) {
            self.usage = rebuild_usage_from(&self.store, &self.profile_id, &self.legacy_usage)?;
            return Err(error.into());
        }
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

fn rebuild_usage_from(
    store: &EventStore,
    profile_id: &str,
    legacy: &UsageLedger,
) -> Result<UsageLedger, CoreError> {
    let aggregates = store.usage_aggregates(profile_id, None, None)?;
    let contexts = store.usage_turn_contexts(profile_id)?;
    let newer = UsageLedger::from_stored(aggregates, &contexts)?;
    let mut combined = legacy.clone();
    combined.clear_detail_records();
    combined.merge_append_only(newer)?;
    Ok(combined)
}

fn legacy_usage_matches(record: &ProviderUsageRecord, query: UsagePageRequest<'_>) -> bool {
    day_matches(&record.day_utc, query.from, query.to)
        && optional_contains(record.provider_id.as_deref(), query.provider)
        && optional_contains(record.model_id.as_deref(), query.model)
        && optional_contains(Some(&record.session_id), query.session)
}

fn legacy_egress_matches(record: &EgressRecord, query: UsagePageRequest<'_>) -> bool {
    day_matches(
        &record.at.format("%Y-%m-%d").to_string(),
        query.from,
        query.to,
    ) && optional_contains(record.session_id.as_deref(), query.session)
        && query.source.is_none_or(|source| {
            let stored = match record.source {
                EgressSource::Web => "web",
                EgressSource::Mcp => "mcp",
                EgressSource::Connector => "connector",
            };
            stored.eq_ignore_ascii_case(source)
        })
}

fn day_matches(day: &str, from: Option<&str>, to: Option<&str>) -> bool {
    from.is_none_or(|from| day >= from) && to.is_none_or(|to| day <= to)
}

fn optional_contains(value: Option<&str>, needle: Option<&str>) -> bool {
    needle.is_none_or(|needle| {
        value.is_some_and(|value| {
            value
                .to_ascii_lowercase()
                .contains(&needle.to_ascii_lowercase())
        })
    })
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
    fn append_only_usage_rebuilds_context_aggregates_and_pages_after_reopen() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("usage-profile.db");
        let secrets = FakeSecretStore::default();
        {
            let mut state = ProfileState::open(&database, "default", &secrets).expect("open");
            let mut admitted = EventEnvelope::new(
                1,
                "runtime",
                "default",
                "response.admitted",
                &serde_json::json!({
                    "message_id": "turn-1",
                    "provider_id": "openai",
                    "model_id": "gpt-test"
                }),
            )
            .expect("admitted");
            admitted.session_id = Some("session-1".into());
            state
                .record_runtime_event(admitted)
                .expect("persist context");
        }
        {
            let mut state = ProfileState::open(&database, "default", &secrets).expect("reopen");
            let mut step = EventEnvelope::new(
                1,
                "runtime",
                "default",
                "response.step_completed",
                &serde_json::json!({"tokens":{"input":7,"output":3,"total":10},"cost":0.000_042}),
            )
            .expect("step");
            step.session_id = Some("session-1".into());
            state.record_runtime_event(step).expect("persist step");
            let snapshot = state.usage_snapshot().expect("aggregate snapshot");
            assert_eq!(snapshot.turns["turn-1"].tokens.total, 10);
            assert!(snapshot.turns["turn-1"].providers.contains("openai"));
        }
        let reopened = ProfileState::open(&database, "default", &secrets).expect("reopen again");
        let snapshot = reopened.usage_snapshot().expect("recovered aggregate");
        assert_eq!(snapshot.sessions["session-1"].provider_steps, 1);
        assert_eq!(snapshot.sessions["session-1"].reported_cost_microusd, 42);
        let page = reopened
            .usage_page(UsagePageRequest {
                limit: 10,
                offset: 0,
                from: None,
                to: None,
                provider: None,
                model: None,
                session: None,
                source: None,
            })
            .expect("usage page");
        assert_eq!(page.usage_total, 1);
        assert_eq!(page.ledger.records[0].turn_id, "turn-1");
        assert_eq!(page.ledger.records[0].model_id.as_deref(), Some("gpt-test"));
    }

    #[test]
    #[allow(clippy::too_many_lines)] // End-to-end compatibility proof keeps the frozen base visible.
    fn frozen_legacy_usage_snapshot_merges_with_new_rows_without_being_rewritten() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("legacy-usage-profile.db");
        let secrets = FakeSecretStore::default();
        let reference = SecretReference {
            service: "dev.personal-agent.storage".to_owned(),
            account: "database-default".to_owned(),
        };
        let key = SecretString::from("legacy-usage-test-key".to_owned());
        secrets.put(&reference, &key).expect("store profile key");

        let mut legacy = UsageLedger::default();
        let mut admitted = EventEnvelope::new(
            1,
            "legacy-runtime",
            "default",
            "response.admitted",
            &serde_json::json!({
                "message_id": "legacy-turn",
                "provider_id": "legacy-provider",
                "model_id": "legacy-model"
            }),
        )
        .expect("legacy admitted");
        admitted.session_id = Some("legacy-session".into());
        admitted.wall_clock_timestamp = "2026-08-29T10:00:00Z".into();
        legacy
            .ingest_runtime_event(&admitted)
            .expect("legacy context");
        let mut legacy_step = EventEnvelope::new(
            2,
            "legacy-runtime",
            "default",
            "response.step_completed",
            &serde_json::json!({"tokens":{"input":3,"output":2,"total":5},"cost":0.000_005}),
        )
        .expect("legacy step");
        legacy_step.session_id = Some("legacy-session".into());
        legacy_step.wall_clock_timestamp = "2026-08-29T10:01:00Z".into();
        legacy
            .ingest_runtime_event(&legacy_step)
            .expect("legacy usage");
        let legacy_egress = EgressRecord {
            id: uuid::Uuid::now_v7(),
            at: "2026-08-29T10:02:00Z".parse().expect("legacy time"),
            source: EgressSource::Mcp,
            destination: "legacy-mcp".into(),
            operation: "tools.call".into(),
            data_class: "tool arguments".into(),
            size_bytes: Some(42),
            purpose: "legacy user request".into(),
            session_id: Some("legacy-session".into()),
            scope_key: Some("session:legacy-session".into()),
        };
        legacy
            .record_egress(legacy_egress.clone())
            .expect("legacy egress");

        let (legacy_body, legacy_revision) = {
            let mut store = EventStore::open(&database, &key).expect("seed store");
            store
                .save_runtime_snapshot("usage-ledger-v1", "default", &legacy)
                .expect("seed legacy ledger");
            let body = store
                .runtime_snapshot::<serde_json::Value>("usage-ledger-v1", "default")
                .expect("legacy body")
                .expect("legacy body exists");
            let revision = store
                .runtime_snapshot_updated_at("usage-ledger-v1", "default")
                .expect("legacy revision")
                .expect("legacy revision exists");
            (body, revision)
        };
        std::thread::sleep(std::time::Duration::from_millis(2));

        let mut state = ProfileState::open(&database, "default", &secrets).expect("open cutover");
        let mut new_step = EventEnvelope::new(
            1,
            "runtime",
            "default",
            "response.step_completed",
            &serde_json::json!({"tokens":{"input":4,"output":3,"total":7},"cost":0.000_007}),
        )
        .expect("new step");
        new_step.session_id = Some("legacy-session".into());
        new_step.wall_clock_timestamp = "2026-08-30T11:00:00Z".into();
        state
            .record_runtime_event(new_step)
            .expect("append post-cutover usage");
        state
            .record_egress(legacy_egress.clone())
            .expect("legacy egress retry stays idempotent");

        let aggregate = state.usage_snapshot().expect("combined aggregate");
        assert_eq!(aggregate.turns["legacy-turn"].provider_steps, 2);
        assert_eq!(aggregate.turns["legacy-turn"].tokens.total, 12);
        assert_eq!(aggregate.turns["legacy-turn"].reported_cost_microusd, 12);
        assert_eq!(aggregate.sessions["legacy-session"].egress_events, 1);

        let page = state
            .usage_page(UsagePageRequest {
                limit: 10,
                offset: 0,
                from: None,
                to: None,
                provider: Some("legacy-provider"),
                model: None,
                session: Some("legacy-session"),
                source: None,
            })
            .expect("combined page");
        assert_eq!(page.usage_total, 2);
        assert_eq!(page.ledger.records.len(), 2);
        assert_eq!(page.ledger.records[0].turn_id, "legacy-turn");
        assert_eq!(page.ledger.records[1].turn_id, "legacy-turn");
        assert_eq!(page.egress_total, 1);
        assert_eq!(page.ledger.egress, vec![legacy_egress]);

        let body_after = state
            .store
            .runtime_snapshot::<serde_json::Value>("usage-ledger-v1", "default")
            .expect("legacy body after")
            .expect("legacy body remains");
        let revision_after = state
            .store
            .runtime_snapshot_updated_at("usage-ledger-v1", "default")
            .expect("legacy revision after")
            .expect("legacy revision remains");
        assert_eq!(body_after, legacy_body);
        assert_eq!(revision_after, legacy_revision);
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
            usage: UsageLedger::default(),
            legacy_usage: UsageLedger::default(),
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
