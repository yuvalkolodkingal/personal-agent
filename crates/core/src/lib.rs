//! Native composition root. UI and CLI share these handlers.

mod config;

pub use config::{
    AgentConfig, CONFIG_SCHEMA, ConfigError, ConfigLoad, KeychainAlias, PersonaConfig,
    PersonalAgentConfig, PrivacyConfig, RiskAcknowledgement, RiskLevel, RuntimeConfig,
    parse_config,
};

use personal_agent_contracts::proto::EventEnvelope;
use personal_agent_runtime::{AgentRuntime, RuntimeError, RuntimeHealth};
use personal_agent_storage::{EventStore, StorageError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

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
}

impl AppProjection {
    /// Apply one event. Unknown additive event types are intentionally ignored.
    ///
    /// # Errors
    ///
    /// Returns an error for sequence regression or malformed event payload.
    pub fn apply(&mut self, event: &EventEnvelope) -> Result<(), CoreError> {
        if event.monotonic_sequence <= self.last_sequence {
            return Err(CoreError::SequenceRegression {
                previous: self.last_sequence,
                next: event.monotonic_sequence,
            });
        }
        self.last_sequence = event.monotonic_sequence;
        self.active_profile.clone_from(&event.profile_id);
        match event.r#type.as_str() {
            "goal.created" => self.goals_total += 1,
            "task.started" => self.tasks_running += 1,
            "task.completed" | "task.failed" | "task.cancelled" => {
                self.tasks_running = self.tasks_running.saturating_sub(1);
            }
            "approval.requested" => self.approvals_waiting += 1,
            "approval.resolved" => {
                self.approvals_waiting = self.approvals_waiting.saturating_sub(1);
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
            _ => {}
        }
        if let Some(session) = &event.session_id {
            self.active_session = Some(session.clone());
        }
        Ok(())
    }
}

/// Composition-root failure.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Event(#[from] personal_agent_contracts::EventError),
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
        Ok(())
    }

    /// Rebuild application state solely from committed events.
    ///
    /// # Errors
    ///
    /// Returns a storage or projection error.
    pub fn rebuild_projection(&mut self) -> Result<(), CoreError> {
        let mut projection = AppProjection::default();
        let mut after = 0;
        loop {
            let batch = self.store.after(after, 512)?;
            if batch.is_empty() {
                break;
            }
            for event in batch {
                after = event.monotonic_sequence;
                projection.apply(&event)?;
            }
        }
        self.projection = projection;
        Ok(())
    }

    #[must_use]
    pub fn projection(&self) -> &AppProjection {
        &self.projection
    }
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
    use personal_agent_runtime::FakeRuntime;
    use secrecy::SecretString;

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
}
