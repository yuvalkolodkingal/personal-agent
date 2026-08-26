//! Durable automation definitions with missed-run and failure policies.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Supported trigger families.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    Once { at: DateTime<Utc> },
    Interval { seconds: u64 },
    Cron { expression: String },
    FileChange { path: String },
    ConnectorEvent { connector_id: String, event: String },
    Webhook { id: String },
    SystemHealth { metric: String, threshold: f64 },
    Heartbeat { seconds: u64 },
}

/// What to do after the application was unavailable for a scheduled run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissedRunPolicy {
    Skip,
    RunOnce,
    CatchUpBounded,
}

/// Stored automation; normal policy remains in force for every run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Automation {
    pub id: Uuid,
    pub name: String,
    pub goal_template: String,
    pub trigger: Trigger,
    pub enabled: bool,
    pub max_concurrency: u8,
    pub missed_run_policy: MissedRunPolicy,
    pub consecutive_failures: u8,
    pub pause_after_failures: u8,
    pub previous_state: Option<serde_json::Value>,
}

impl Automation {
    pub fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= self.pause_after_failures {
            self.enabled = false;
        }
    }
    pub fn record_success(&mut self, state: Option<serde_json::Value>) {
        self.consecutive_failures = 0;
        self.previous_state = state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_failures_pause_an_automation() {
        let mut automation = Automation {
            id: Uuid::now_v7(),
            name: "monitor".into(),
            goal_template: "check".into(),
            trigger: Trigger::Interval { seconds: 60 },
            enabled: true,
            max_concurrency: 1,
            missed_run_policy: MissedRunPolicy::RunOnce,
            consecutive_failures: 0,
            pause_after_failures: 3,
            previous_state: None,
        };
        automation.record_failure();
        automation.record_failure();
        assert!(automation.enabled);
        automation.record_failure();
        assert!(!automation.enabled);
    }
}
