//! Durable automation definitions with missed-run and failure policies.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

/// Supported trigger families.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    Once { at: DateTime<Utc> },
    Interval { seconds: u64 },
    Cron { expression: String },
    FileChange { path: String },
    DirectoryChange { path: String },
    CalendarEvent { connector_id: String },
    EmailEvent { connector_id: String },
    ConnectorEvent { connector_id: String, event: String },
    Webhook { id: String },
    SystemHealth { metric: String, threshold: f64 },
    Heartbeat { seconds: u64 },
    NetworkChange,
    DeviceChange { device_kind: String },
    SemanticMonitor { source_id: String, query: String },
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
    pub next_due_at: Option<DateTime<Utc>>,
    pub maximum_catch_up_runs: u8,
    pub quiet_hours_utc: Option<(u8, u8)>,
    pub notification_route: String,
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

/// One durable run. The schedule key deduplicates restarts and clock re-evaluation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationRun {
    pub id: Uuid,
    pub automation_id: Uuid,
    pub schedule_key: String,
    pub scheduled_for: DateTime<Utc>,
    pub status: AutomationRunStatus,
    pub attempt: u16,
    pub approval_reason: Option<String>,
}

/// Automation run lifecycle; approval is a suspension, not a failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunStatus {
    Queued,
    Running,
    WaitingApproval,
    PausedForUser,
    Completed,
    Failed,
    Skipped,
}

/// External trigger event normalized before it enters the scheduler.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TriggerEvent {
    FileChange { path: String },
    DirectoryChange { path: String },
    Calendar { connector_id: String },
    Email { connector_id: String },
    Connector { connector_id: String, event: String },
    Webhook { id: String },
    NetworkChange,
    DeviceChange { device_kind: String },
    SemanticChange { source_id: String, query: String },
}

/// Persistable scheduler state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SchedulerSnapshot {
    pub automations: BTreeMap<Uuid, Automation>,
    pub runs: BTreeMap<String, AutomationRun>,
    pub last_evaluated_at: Option<DateTime<Utc>>,
}

/// Scheduler validation or transition error.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SchedulerError {
    #[error("automation does not exist: {0}")]
    MissingAutomation(Uuid),
    #[error("automation run does not exist: {0}")]
    MissingRun(String),
    #[error("invalid trigger: {0}")]
    InvalidTrigger(String),
    #[error("invalid automation limits")]
    InvalidLimits,
    #[error("automation run is not in the required state")]
    InvalidTransition,
}

/// Durable scheduler with deterministic missed-run and failure behavior.
#[derive(Clone, Debug, Default)]
pub struct Scheduler {
    snapshot: SchedulerSnapshot,
}

impl Scheduler {
    #[must_use]
    pub fn from_snapshot(snapshot: SchedulerSnapshot) -> Self {
        Self { snapshot }
    }

    #[must_use]
    pub fn snapshot(&self) -> &SchedulerSnapshot {
        &self.snapshot
    }

    /// Register or update one automation after validating bounds and trigger.
    ///
    /// # Errors
    ///
    /// Rejects zero concurrency/failure thresholds/catch-up bounds or unsupported cron syntax.
    pub fn upsert(&mut self, automation: Automation) -> Result<(), SchedulerError> {
        if automation.max_concurrency == 0
            || automation.pause_after_failures == 0
            || automation.maximum_catch_up_runs == 0
        {
            return Err(SchedulerError::InvalidLimits);
        }
        validate_trigger(&automation.trigger)?;
        self.snapshot.automations.insert(automation.id, automation);
        Ok(())
    }

    /// Evaluate due time-based work without sleeping. Existing schedule keys are
    /// reused, so restart/evaluation cannot enqueue duplicates.
    ///
    /// # Errors
    ///
    /// Returns trigger-validation failures for corrupt persisted definitions.
    pub fn evaluate(&mut self, now: DateTime<Utc>) -> Result<Vec<String>, SchedulerError> {
        let mut queued = Vec::new();
        let ids = self
            .snapshot
            .automations
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for id in ids {
            let automation = self
                .snapshot
                .automations
                .get(&id)
                .cloned()
                .ok_or(SchedulerError::MissingAutomation(id))?;
            if !automation.enabled {
                continue;
            }
            let Some(next_due) = automation.next_due_at else {
                continue;
            };
            if next_due > now {
                continue;
            }
            let interval = trigger_interval_seconds(&automation.trigger)?;
            let due_times = due_times(&automation, next_due, now, interval);
            for (scheduled_for, status) in due_times {
                let key = schedule_key(id, scheduled_for);
                if self.snapshot.runs.contains_key(&key) {
                    continue;
                }
                let run = AutomationRun {
                    id: Uuid::now_v7(),
                    automation_id: id,
                    schedule_key: key.clone(),
                    scheduled_for,
                    status,
                    attempt: 0,
                    approval_reason: None,
                };
                if status == AutomationRunStatus::Queued {
                    queued.push(key.clone());
                }
                self.snapshot.runs.insert(key, run);
            }
            if let Some(stored) = self.snapshot.automations.get_mut(&id) {
                stored.next_due_at = next_due_after(now, interval, &stored.trigger);
                if matches!(stored.trigger, Trigger::Once { .. }) {
                    stored.enabled = false;
                }
            }
        }
        self.snapshot.last_evaluated_at = Some(now);
        Ok(queued)
    }

    /// Match an externally produced event against enabled definitions.
    #[must_use]
    pub fn trigger_event(&mut self, event: &TriggerEvent, now: DateTime<Utc>) -> Vec<String> {
        let mut queued = Vec::new();
        for automation in self.snapshot.automations.values() {
            if !automation.enabled || !event_matches(&automation.trigger, event) {
                continue;
            }
            let key = format!("{}:event:{}", automation.id, now.timestamp_millis());
            if self.snapshot.runs.contains_key(&key) {
                continue;
            }
            self.snapshot.runs.insert(
                key.clone(),
                AutomationRun {
                    id: Uuid::now_v7(),
                    automation_id: automation.id,
                    schedule_key: key.clone(),
                    scheduled_for: now,
                    status: AutomationRunStatus::Queued,
                    attempt: 0,
                    approval_reason: None,
                },
            );
            queued.push(key);
        }
        queued
    }

    /// Start a queued run if its automation concurrency budget permits it.
    ///
    /// # Errors
    ///
    /// Rejects missing definitions/runs, invalid transitions, or exhausted concurrency.
    pub fn start(&mut self, key: &str) -> Result<(), SchedulerError> {
        let automation_id = self
            .snapshot
            .runs
            .get(key)
            .ok_or_else(|| SchedulerError::MissingRun(key.into()))?
            .automation_id;
        let automation = self
            .snapshot
            .automations
            .get(&automation_id)
            .ok_or(SchedulerError::MissingAutomation(automation_id))?;
        let running = self
            .snapshot
            .runs
            .values()
            .filter(|run| {
                run.automation_id == automation_id && run.status == AutomationRunStatus::Running
            })
            .count();
        if running >= usize::from(automation.max_concurrency) {
            return Err(SchedulerError::InvalidLimits);
        }
        let run = self
            .snapshot
            .runs
            .get_mut(key)
            .ok_or_else(|| SchedulerError::MissingRun(key.into()))?;
        if run.status != AutomationRunStatus::Queued {
            return Err(SchedulerError::InvalidTransition);
        }
        run.status = AutomationRunStatus::Running;
        run.attempt = run.attempt.saturating_add(1);
        Ok(())
    }

    /// Suspend a running task on approval without increasing failure counts.
    ///
    /// # Errors
    ///
    /// Rejects missing or non-running runs.
    pub fn wait_for_approval(&mut self, key: &str, reason: &str) -> Result<(), SchedulerError> {
        let run = self
            .snapshot
            .runs
            .get_mut(key)
            .ok_or_else(|| SchedulerError::MissingRun(key.into()))?;
        if run.status != AutomationRunStatus::Running {
            return Err(SchedulerError::InvalidTransition);
        }
        run.status = AutomationRunStatus::WaitingApproval;
        run.approval_reason = Some(reason.into());
        Ok(())
    }

    /// Resume a user-approved suspended run.
    ///
    /// # Errors
    ///
    /// Rejects missing or non-waiting runs.
    pub fn approve(&mut self, key: &str) -> Result<(), SchedulerError> {
        let run = self
            .snapshot
            .runs
            .get_mut(key)
            .ok_or_else(|| SchedulerError::MissingRun(key.into()))?;
        if run.status != AutomationRunStatus::WaitingApproval {
            return Err(SchedulerError::InvalidTransition);
        }
        run.status = AutomationRunStatus::Queued;
        run.approval_reason = None;
        Ok(())
    }

    /// Complete or fail one run and update change-detection/failure state.
    ///
    /// # Errors
    ///
    /// Rejects missing or non-running runs.
    pub fn finish(
        &mut self,
        key: &str,
        success: bool,
        previous_state: Option<serde_json::Value>,
    ) -> Result<(), SchedulerError> {
        let run = self
            .snapshot
            .runs
            .get_mut(key)
            .ok_or_else(|| SchedulerError::MissingRun(key.into()))?;
        if run.status != AutomationRunStatus::Running {
            return Err(SchedulerError::InvalidTransition);
        }
        run.status = if success {
            AutomationRunStatus::Completed
        } else {
            AutomationRunStatus::Failed
        };
        let automation = self
            .snapshot
            .automations
            .get_mut(&run.automation_id)
            .ok_or(SchedulerError::MissingAutomation(run.automation_id))?;
        if success {
            automation.record_success(previous_state);
        } else {
            automation.record_failure();
        }
        Ok(())
    }

    /// Foreground conversation preempts active background work.
    pub fn preempt_for_user(&mut self) {
        for run in self.snapshot.runs.values_mut() {
            if run.status == AutomationRunStatus::Running {
                run.status = AutomationRunStatus::PausedForUser;
            }
        }
    }
}

fn validate_trigger(trigger: &Trigger) -> Result<(), SchedulerError> {
    match trigger {
        Trigger::Interval { seconds } | Trigger::Heartbeat { seconds } if *seconds == 0 => Err(
            SchedulerError::InvalidTrigger("interval must be greater than zero".into()),
        ),
        Trigger::Cron { expression } => parse_cron_interval(expression).map(|_| ()),
        _ => Ok(()),
    }
}

fn trigger_interval_seconds(trigger: &Trigger) -> Result<Option<u64>, SchedulerError> {
    match trigger {
        Trigger::Interval { seconds } | Trigger::Heartbeat { seconds } => Ok(Some(*seconds)),
        Trigger::Cron { expression } => parse_cron_interval(expression).map(Some),
        _ => Ok(None),
    }
}

fn parse_cron_interval(expression: &str) -> Result<u64, SchedulerError> {
    let fields = expression.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 || fields[1..] != ["*", "*", "*", "*"] {
        return Err(SchedulerError::InvalidTrigger(
            "cron currently requires a five-field */N * * * * expression".into(),
        ));
    }
    let minutes = fields[0]
        .strip_prefix("*/")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| SchedulerError::InvalidTrigger("invalid cron minute interval".into()))?;
    minutes
        .checked_mul(60)
        .ok_or_else(|| SchedulerError::InvalidTrigger("cron interval overflow".into()))
}

fn due_times(
    automation: &Automation,
    first: DateTime<Utc>,
    now: DateTime<Utc>,
    interval: Option<u64>,
) -> Vec<(DateTime<Utc>, AutomationRunStatus)> {
    let mut due = vec![first];
    if let Some(seconds) = interval {
        let step = chrono::Duration::seconds(i64::try_from(seconds).unwrap_or(i64::MAX));
        let mut cursor = first;
        while let Some(next) = cursor.checked_add_signed(step) {
            if next > now || due.len() >= usize::from(automation.maximum_catch_up_runs) {
                break;
            }
            due.push(next);
            cursor = next;
        }
    }
    match automation.missed_run_policy {
        MissedRunPolicy::Skip => due
            .into_iter()
            .map(|time| (time, AutomationRunStatus::Skipped))
            .collect(),
        MissedRunPolicy::RunOnce => due
            .last()
            .copied()
            .map(|time| vec![(time, AutomationRunStatus::Queued)])
            .unwrap_or_default(),
        MissedRunPolicy::CatchUpBounded => due
            .into_iter()
            .map(|time| (time, AutomationRunStatus::Queued))
            .collect(),
    }
}

fn next_due_after(
    now: DateTime<Utc>,
    interval: Option<u64>,
    trigger: &Trigger,
) -> Option<DateTime<Utc>> {
    if matches!(trigger, Trigger::Once { .. }) {
        return None;
    }
    let seconds = interval?;
    now.checked_add_signed(chrono::Duration::seconds(
        i64::try_from(seconds).unwrap_or(i64::MAX),
    ))
}

fn schedule_key(id: Uuid, at: DateTime<Utc>) -> String {
    format!("{id}:{}", at.timestamp_millis())
}

fn event_matches(trigger: &Trigger, event: &TriggerEvent) -> bool {
    match (trigger, event) {
        (Trigger::FileChange { path: expected }, TriggerEvent::FileChange { path })
        | (Trigger::DirectoryChange { path: expected }, TriggerEvent::DirectoryChange { path }) => {
            expected == path
        }
        (
            Trigger::CalendarEvent {
                connector_id: expected,
            },
            TriggerEvent::Calendar { connector_id },
        )
        | (
            Trigger::EmailEvent {
                connector_id: expected,
            },
            TriggerEvent::Email { connector_id },
        ) => expected == connector_id,
        (
            Trigger::ConnectorEvent {
                connector_id: expected_id,
                event: expected_event,
            },
            TriggerEvent::Connector {
                connector_id,
                event,
            },
        ) => expected_id == connector_id && expected_event == event,
        (Trigger::Webhook { id: expected }, TriggerEvent::Webhook { id }) => expected == id,
        (Trigger::NetworkChange, TriggerEvent::NetworkChange) => true,
        (
            Trigger::DeviceChange {
                device_kind: expected,
            },
            TriggerEvent::DeviceChange { device_kind },
        ) => expected == device_kind,
        (
            Trigger::SemanticMonitor {
                source_id: expected_source,
                query: expected_query,
            },
            TriggerEvent::SemanticChange { source_id, query },
        ) => expected_source == source_id && expected_query == query,
        _ => false,
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
            next_due_at: Some(Utc::now()),
            maximum_catch_up_runs: 3,
            quiet_hours_utc: None,
            notification_route: "desktop".into(),
        };
        automation.record_failure();
        automation.record_failure();
        assert!(automation.enabled);
        automation.record_failure();
        assert!(!automation.enabled);
    }

    fn fixture_automation(now: DateTime<Utc>) -> Automation {
        Automation {
            id: Uuid::now_v7(),
            name: "briefing".into(),
            goal_template: "summarize changes".into(),
            trigger: Trigger::Interval { seconds: 60 },
            enabled: true,
            max_concurrency: 1,
            missed_run_policy: MissedRunPolicy::CatchUpBounded,
            consecutive_failures: 0,
            pause_after_failures: 2,
            previous_state: None,
            next_due_at: Some(now),
            maximum_catch_up_runs: 3,
            quiet_hours_utc: Some((22, 7)),
            notification_route: "desktop".into(),
        }
    }

    #[test]
    fn missed_runs_are_bounded_and_restart_deduplicated() {
        let now = Utc::now();
        let mut scheduler = Scheduler::default();
        scheduler.upsert(fixture_automation(now)).expect("register");
        let later = now + chrono::Duration::minutes(10);
        let first = scheduler.evaluate(later).expect("evaluate");
        assert_eq!(first.len(), 3);
        let snapshot = scheduler.snapshot().clone();
        let mut recovered = Scheduler::from_snapshot(snapshot);
        assert!(recovered.evaluate(later).expect("repeat").is_empty());
        assert_eq!(recovered.snapshot().runs.len(), 3);
    }

    #[test]
    fn approval_suspends_and_failures_pause_definition() {
        let now = Utc::now();
        let mut automation = fixture_automation(now);
        automation.missed_run_policy = MissedRunPolicy::RunOnce;
        let id = automation.id;
        let mut scheduler = Scheduler::default();
        scheduler.upsert(automation).expect("register");
        let key = scheduler.evaluate(now).expect("evaluate").remove(0);
        scheduler.start(&key).expect("start");
        scheduler
            .wait_for_approval(&key, "send communication")
            .expect("wait");
        assert_eq!(
            scheduler.snapshot().runs[&key].status,
            AutomationRunStatus::WaitingApproval
        );
        assert_eq!(
            scheduler.snapshot().automations[&id].consecutive_failures,
            0
        );
        scheduler.approve(&key).expect("approve");
        scheduler.start(&key).expect("restart");
        scheduler.finish(&key, false, None).expect("failure");

        let next = now + chrono::Duration::minutes(2);
        let second = scheduler.evaluate(next).expect("next").remove(0);
        scheduler.start(&second).expect("start second");
        scheduler.finish(&second, false, None).expect("failure two");
        assert!(!scheduler.snapshot().automations[&id].enabled);
    }

    #[test]
    fn foreground_conversation_preempts_background_run() {
        let now = Utc::now();
        let mut scheduler = Scheduler::default();
        scheduler.upsert(fixture_automation(now)).expect("register");
        let key = scheduler.evaluate(now).expect("evaluate").remove(0);
        scheduler.start(&key).expect("start");
        scheduler.preempt_for_user();
        assert_eq!(
            scheduler.snapshot().runs[&key].status,
            AutomationRunStatus::PausedForUser
        );
    }
}
