//! Durable goals, typed tasks, and acyclic execution plans.

use chrono::{DateTime, Utc};
use petgraph::{algo::toposort, graphmap::DiGraphMap};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use uuid::Uuid;

/// Lifecycle shared by goals and tasks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    /// Accepted but not yet planned.
    #[default]
    Queued,
    /// A typed plan is being produced.
    Planning,
    /// Work is executing.
    Running,
    /// Work is suspended on approval, clarification, or a dependency.
    Waiting,
    /// Explicitly paused.
    Paused,
    /// Verified against success criteria.
    Completed,
    /// Terminal failure.
    Failed,
    /// Explicitly cancelled.
    Cancelled,
}

/// Limits enforced independently of provider behavior.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Budgets {
    /// Maximum provider cost in USD.
    pub cost_usd: Option<f64>,
    /// Maximum tokens across provider calls.
    pub tokens: Option<u64>,
    /// Maximum wall time in seconds.
    pub wall_time_seconds: Option<u64>,
    /// Maximum tool invocations.
    pub tool_calls: Option<u32>,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            cost_usd: Some(10.0),
            tokens: Some(1_000_000),
            wall_time_seconds: Some(14_400),
            tool_calls: Some(500),
        }
    }
}

/// User-owned objective that survives process restarts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    /// Stable goal identifier.
    pub id: Uuid,
    /// User objective, stored verbatim.
    pub objective: String,
    /// Observable completion checks.
    pub success_criteria: Vec<String>,
    /// Input channel such as UI, voice, CLI, or automation.
    pub origin: String,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Higher values run first.
    pub priority: i32,
    /// Optional deadline.
    pub deadline: Option<DateTime<Utc>>,
    /// Named autonomy policy snapshot.
    pub autonomy_policy: String,
    /// Resource ceilings.
    pub budgets: Budgets,
    /// Current plan revision, starting at zero before planning.
    pub plan_revision: u32,
    /// Current lifecycle state.
    pub status: WorkStatus,
    /// Verified result when terminal.
    pub final_result: Option<Value>,
    /// Produced content-addressed artifact IDs.
    pub artifacts: Vec<String>,
}

impl Goal {
    /// Construct a queued goal with bounded defaults.
    pub fn new(
        objective: impl Into<String>,
        success_criteria: Vec<String>,
        origin: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            objective: objective.into(),
            success_criteria,
            origin: origin.into(),
            created_at: Utc::now(),
            priority: 0,
            deadline: None,
            autonomy_policy: "bounded".into(),
            budgets: Budgets::default(),
            plan_revision: 0,
            status: WorkStatus::Queued,
            final_result: None,
            artifacts: Vec::new(),
        }
    }

    /// Completion is legal only when every declared criterion was verified.
    ///
    /// # Errors
    ///
    /// Returns the criteria that have not been verified.
    pub fn complete(
        &mut self,
        verified_criteria: &BTreeSet<String>,
        result: Value,
    ) -> Result<(), PlanError> {
        let missing: Vec<_> = self
            .success_criteria
            .iter()
            .filter(|criterion| !verified_criteria.contains(*criterion))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(PlanError::UnverifiedCriteria(missing));
        }
        self.status = WorkStatus::Completed;
        self.final_result = Some(result);
        Ok(())
    }
}

/// Isolation boundary for task execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionZone {
    Isolated,
    Workspace,
    Desktop,
}

/// One typed node in a plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub goal_id: Uuid,
    pub parent_task_id: Option<Uuid>,
    pub title: String,
    pub assigned_agent: String,
    pub workspace: Option<String>,
    pub browser_profile: Option<String>,
    pub tool_scopes: BTreeSet<String>,
    pub risk: String,
    pub execution_zone: ExecutionZone,
    pub max_attempts: u16,
    pub attempt: u16,
    pub idempotency_key: Option<String>,
    pub checkpoint_id: Option<String>,
    pub status: WorkStatus,
    pub progress: u8,
    pub output: Option<Value>,
}

/// Validated, revisioned DAG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskGraph {
    pub goal_id: Uuid,
    pub revision: u32,
    pub tasks: BTreeMap<Uuid, Task>,
    /// Dependency first, dependent second.
    pub edges: Vec<(Uuid, Uuid)>,
}

/// Invalid plan or completion transition.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PlanError {
    #[error("task {0} is referenced but not present")]
    MissingTask(Uuid),
    #[error("task graph contains a cycle")]
    Cycle,
    #[error("task {task} belongs to goal {actual}, expected {expected}")]
    WrongGoal {
        task: Uuid,
        actual: Uuid,
        expected: Uuid,
    },
    #[error("success criteria were not verified: {0:?}")]
    UnverifiedCriteria(Vec<String>),
    #[error("task {0} is not ready to start")]
    TaskNotReady(Uuid),
    #[error("task {0} exceeded its retry policy")]
    AttemptsExhausted(Uuid),
    #[error("task {0} has a consequential effect without an idempotency key")]
    MissingIdempotencyKey(Uuid),
    #[error("task {task} delegation widens parent authority: {detail}")]
    AuthorityWidening { task: Uuid, detail: String },
    #[error("snapshot is invalid: {0}")]
    InvalidSnapshot(String),
}

impl TaskGraph {
    /// Verify ownership, referential integrity, and acyclicity.
    ///
    /// # Errors
    ///
    /// Returns a typed ownership, reference, or cycle error.
    pub fn validate(&self) -> Result<(), PlanError> {
        for task in self.tasks.values() {
            if task.goal_id != self.goal_id {
                return Err(PlanError::WrongGoal {
                    task: task.id,
                    actual: task.goal_id,
                    expected: self.goal_id,
                });
            }
        }
        let mut graph: DiGraphMap<Uuid, ()> = DiGraphMap::new();
        for id in self.tasks.keys() {
            graph.add_node(*id);
        }
        for (dependency, dependent) in &self.edges {
            if !self.tasks.contains_key(dependency) {
                return Err(PlanError::MissingTask(*dependency));
            }
            if !self.tasks.contains_key(dependent) {
                return Err(PlanError::MissingTask(*dependent));
            }
            graph.add_edge(*dependency, *dependent, ());
        }
        toposort(&graph, None).map_err(|_| PlanError::Cycle)?;
        Ok(())
    }
}

/// Durable record for a consequential effect. A completed receipt prevents a
/// provider/process retry from performing the effect twice.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EffectReceipt {
    pub idempotency_key: String,
    pub task_id: Uuid,
    pub effect_fingerprint: String,
    pub verified: bool,
    pub completed_at: DateTime<Utc>,
}

/// Persistable supervisor state. Storage owns the atomic write/event boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SupervisorSnapshot {
    pub schema_version: u32,
    pub graph: TaskGraph,
    pub receipts: BTreeMap<String, EffectReceipt>,
    pub running_order: Vec<Uuid>,
    pub elapsed_virtual_seconds: u64,
}

/// Scheduler decisions are explicit and can be persisted before execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ScheduleDecision {
    Start { task_id: Uuid, attempt: u16 },
    WaitForDependencies { task_id: Uuid },
    WaitForApproval { task_id: Uuid, reason: String },
    AlreadyCompleted { task_id: Uuid },
}

/// Crash-safe task supervisor with bounded parallelism and delegation depth.
#[derive(Clone, Debug)]
pub struct DurableSupervisor {
    snapshot: SupervisorSnapshot,
    maximum_parallelism: usize,
    maximum_delegation_depth: usize,
}

impl DurableSupervisor {
    /// Start supervising a validated graph.
    ///
    /// # Errors
    ///
    /// Returns plan validation or invalid scheduler-bound errors.
    pub fn new(
        graph: TaskGraph,
        maximum_parallelism: usize,
        maximum_delegation_depth: usize,
    ) -> Result<Self, PlanError> {
        graph.validate()?;
        if !(1..=8).contains(&maximum_parallelism) {
            return Err(PlanError::InvalidSnapshot(
                "parallelism must be between one and eight".into(),
            ));
        }
        if maximum_delegation_depth == 0 {
            return Err(PlanError::InvalidSnapshot(
                "delegation depth must be at least one".into(),
            ));
        }
        Ok(Self {
            snapshot: SupervisorSnapshot {
                schema_version: 1,
                graph,
                receipts: BTreeMap::new(),
                running_order: Vec::new(),
                elapsed_virtual_seconds: 0,
            },
            maximum_parallelism,
            maximum_delegation_depth,
        })
    }

    /// Recover a persisted snapshot. In-flight tasks are re-queued only when
    /// retry-safe; a verified receipt marks their effect complete instead.
    ///
    /// # Errors
    ///
    /// Rejects unknown schema versions, invalid graphs, or unsafe in-flight work.
    pub fn recover(
        mut snapshot: SupervisorSnapshot,
        maximum_parallelism: usize,
        maximum_delegation_depth: usize,
    ) -> Result<Self, PlanError> {
        if snapshot.schema_version != 1 {
            return Err(PlanError::InvalidSnapshot(format!(
                "unsupported schema version {}",
                snapshot.schema_version
            )));
        }
        snapshot.graph.validate()?;
        for task_id in std::mem::take(&mut snapshot.running_order) {
            let task = snapshot
                .graph
                .tasks
                .get_mut(&task_id)
                .ok_or(PlanError::MissingTask(task_id))?;
            if task.status != WorkStatus::Running {
                return Err(PlanError::InvalidSnapshot(format!(
                    "running order references non-running task {task_id}"
                )));
            }
            let receipt = task
                .idempotency_key
                .as_ref()
                .and_then(|key| snapshot.receipts.get(key));
            if receipt.is_some_and(|receipt| receipt.verified) {
                task.status = WorkStatus::Completed;
                task.progress = 100;
            } else if retry_safe(task) && task.attempt < task.max_attempts {
                task.status = WorkStatus::Queued;
            } else {
                task.status = WorkStatus::Waiting;
            }
        }
        let supervisor = Self {
            snapshot,
            maximum_parallelism,
            maximum_delegation_depth,
        };
        supervisor.validate_scheduler_bounds()?;
        Ok(supervisor)
    }

    fn validate_scheduler_bounds(&self) -> Result<(), PlanError> {
        if !(1..=8).contains(&self.maximum_parallelism) {
            return Err(PlanError::InvalidSnapshot(
                "parallelism must be between one and eight".into(),
            ));
        }
        if self.maximum_delegation_depth == 0 {
            return Err(PlanError::InvalidSnapshot(
                "delegation depth must be at least one".into(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> &SupervisorSnapshot {
        &self.snapshot
    }

    /// Return ready tasks in stable ID order, limited by available worker slots.
    #[must_use]
    pub fn ready_tasks(&self) -> Vec<Uuid> {
        let running = self
            .snapshot
            .graph
            .tasks
            .values()
            .filter(|task| task.status == WorkStatus::Running)
            .count();
        let available = self.maximum_parallelism.saturating_sub(running);
        self.snapshot
            .graph
            .tasks
            .iter()
            .filter(|(_, task)| task.status == WorkStatus::Queued)
            .filter(|(id, _)| self.dependencies_completed(**id))
            .map(|(id, _)| *id)
            .take(available)
            .collect()
    }

    fn dependencies_completed(&self, task_id: Uuid) -> bool {
        self.snapshot
            .graph
            .edges
            .iter()
            .filter(|(_, dependent)| *dependent == task_id)
            .all(|(dependency, _)| {
                self.snapshot.graph.tasks[dependency].status == WorkStatus::Completed
            })
    }

    /// Persistable transition into running state.
    ///
    /// # Errors
    ///
    /// Rejects blocked, unsafe, duplicate, or exhausted tasks.
    pub fn start(&mut self, task_id: Uuid) -> Result<ScheduleDecision, PlanError> {
        let task = self
            .snapshot
            .graph
            .tasks
            .get(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        if task.status == WorkStatus::Completed {
            return Ok(ScheduleDecision::AlreadyCompleted { task_id });
        }
        if !self.dependencies_completed(task_id) {
            return Ok(ScheduleDecision::WaitForDependencies { task_id });
        }
        if task.status != WorkStatus::Queued {
            return Err(PlanError::TaskNotReady(task_id));
        }
        if task.attempt >= task.max_attempts {
            return Err(PlanError::AttemptsExhausted(task_id));
        }
        if is_consequential(task) && task.idempotency_key.is_none() {
            return Err(PlanError::MissingIdempotencyKey(task_id));
        }
        if self.snapshot.running_order.len() >= self.maximum_parallelism {
            return Err(PlanError::TaskNotReady(task_id));
        }
        let task = self
            .snapshot
            .graph
            .tasks
            .get_mut(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        task.attempt = task.attempt.saturating_add(1);
        task.status = WorkStatus::Running;
        self.snapshot.running_order.push(task_id);
        Ok(ScheduleDecision::Start {
            task_id,
            attempt: task.attempt,
        })
    }

    /// Complete a task only after its result has been verified. Consequential
    /// effects atomically carry a deduplication receipt in the next snapshot.
    ///
    /// # Errors
    ///
    /// Rejects non-running tasks, unverified results, or inconsistent receipts.
    pub fn complete(
        &mut self,
        task_id: Uuid,
        output: Value,
        verified: bool,
        effect_fingerprint: Option<String>,
    ) -> Result<(), PlanError> {
        let task = self
            .snapshot
            .graph
            .tasks
            .get_mut(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        if task.status != WorkStatus::Running || !verified {
            return Err(PlanError::TaskNotReady(task_id));
        }
        if is_consequential(task) {
            let key = task
                .idempotency_key
                .clone()
                .ok_or(PlanError::MissingIdempotencyKey(task_id))?;
            let fingerprint = effect_fingerprint.ok_or_else(|| {
                PlanError::InvalidSnapshot("consequential completion needs a fingerprint".into())
            })?;
            if let Some(existing) = self.snapshot.receipts.get(&key) {
                if existing.effect_fingerprint != fingerprint || existing.task_id != task_id {
                    return Err(PlanError::InvalidSnapshot(
                        "idempotency key was reused for a different effect".into(),
                    ));
                }
            } else {
                self.snapshot.receipts.insert(
                    key.clone(),
                    EffectReceipt {
                        idempotency_key: key,
                        task_id,
                        effect_fingerprint: fingerprint,
                        verified: true,
                        completed_at: Utc::now(),
                    },
                );
            }
        }
        task.output = Some(output);
        task.progress = 100;
        task.status = WorkStatus::Completed;
        self.snapshot.running_order.retain(|id| *id != task_id);
        Ok(())
    }

    /// Handle provider/process failure. Only retry-safe work returns to the queue.
    ///
    /// # Errors
    ///
    /// Rejects missing or non-running tasks.
    pub fn provider_failed(&mut self, task_id: Uuid) -> Result<(), PlanError> {
        let task = self
            .snapshot
            .graph
            .tasks
            .get_mut(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        if task.status != WorkStatus::Running {
            return Err(PlanError::TaskNotReady(task_id));
        }
        task.status = if retry_safe(task) && task.attempt < task.max_attempts {
            WorkStatus::Queued
        } else {
            WorkStatus::Waiting
        };
        self.snapshot.running_order.retain(|id| *id != task_id);
        Ok(())
    }

    /// Persist a runtime checkpoint/session identifier for restart-safe supervision.
    ///
    /// # Errors
    /// Rejects missing tasks and tasks that are not active or approval-suspended.
    pub fn bind_checkpoint(
        &mut self,
        task_id: Uuid,
        checkpoint_id: impl Into<String>,
    ) -> Result<(), PlanError> {
        let task = self
            .snapshot
            .graph
            .tasks
            .get_mut(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        if !matches!(task.status, WorkStatus::Running | WorkStatus::Waiting) {
            return Err(PlanError::TaskNotReady(task_id));
        }
        task.checkpoint_id = Some(checkpoint_id.into());
        Ok(())
    }

    /// Suspend one running task while a native approval is outstanding.
    ///
    /// # Errors
    /// Rejects missing or non-running tasks.
    pub fn wait_for_approval(
        &mut self,
        task_id: Uuid,
        reason: impl Into<String>,
    ) -> Result<ScheduleDecision, PlanError> {
        let task = self
            .snapshot
            .graph
            .tasks
            .get_mut(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        if task.status != WorkStatus::Running {
            return Err(PlanError::TaskNotReady(task_id));
        }
        task.status = WorkStatus::Waiting;
        self.snapshot.running_order.retain(|id| *id != task_id);
        Ok(ScheduleDecision::WaitForApproval {
            task_id,
            reason: reason.into(),
        })
    }

    /// Return an approval-suspended task to the same live runtime turn.
    ///
    /// # Errors
    /// Rejects missing tasks, non-waiting tasks, or a full worker lane.
    pub fn resume_after_approval(&mut self, task_id: Uuid) -> Result<(), PlanError> {
        let task = self
            .snapshot
            .graph
            .tasks
            .get_mut(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        if task.status != WorkStatus::Waiting
            || self.snapshot.running_order.len() >= self.maximum_parallelism
        {
            return Err(PlanError::TaskNotReady(task_id));
        }
        task.status = WorkStatus::Running;
        self.snapshot.running_order.push(task_id);
        Ok(())
    }

    /// Pause one non-terminal task without losing attempts or checkpoints.
    ///
    /// # Errors
    /// Rejects missing or terminal tasks.
    pub fn pause(&mut self, task_id: Uuid) -> Result<(), PlanError> {
        let task = self
            .snapshot
            .graph
            .tasks
            .get_mut(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        if matches!(
            task.status,
            WorkStatus::Completed | WorkStatus::Failed | WorkStatus::Cancelled
        ) {
            return Err(PlanError::TaskNotReady(task_id));
        }
        task.status = WorkStatus::Paused;
        self.snapshot.running_order.retain(|id| *id != task_id);
        Ok(())
    }

    /// Resume one explicitly paused task into the durable queue.
    ///
    /// # Errors
    /// Rejects missing or non-paused tasks.
    pub fn resume(&mut self, task_id: Uuid) -> Result<(), PlanError> {
        let task = self
            .snapshot
            .graph
            .tasks
            .get_mut(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        if task.status != WorkStatus::Paused {
            return Err(PlanError::TaskNotReady(task_id));
        }
        task.status = WorkStatus::Queued;
        Ok(())
    }

    /// Cancel one non-terminal task and remove it from the running lane.
    ///
    /// # Errors
    /// Rejects missing or already-terminal tasks.
    pub fn cancel(&mut self, task_id: Uuid) -> Result<(), PlanError> {
        let task = self
            .snapshot
            .graph
            .tasks
            .get_mut(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        if matches!(task.status, WorkStatus::Completed | WorkStatus::Cancelled) {
            return Err(PlanError::TaskNotReady(task_id));
        }
        task.status = WorkStatus::Cancelled;
        self.snapshot.running_order.retain(|id| *id != task_id);
        Ok(())
    }

    /// Retry failed or safely suspended work without resetting its attempt counter.
    ///
    /// # Errors
    /// Rejects missing tasks, unsupported states, or exhausted attempts.
    pub fn retry(&mut self, task_id: Uuid) -> Result<(), PlanError> {
        let task = self
            .snapshot
            .graph
            .tasks
            .get_mut(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        if !matches!(task.status, WorkStatus::Failed | WorkStatus::Waiting)
            || task.attempt >= task.max_attempts
        {
            return Err(if task.attempt >= task.max_attempts {
                PlanError::AttemptsExhausted(task_id)
            } else {
                PlanError::TaskNotReady(task_id)
            });
        }
        task.status = WorkStatus::Queued;
        task.checkpoint_id = None;
        Ok(())
    }

    /// Mark an active task as terminally failed so retry remains an explicit UI action.
    ///
    /// # Errors
    /// Rejects missing or non-running tasks.
    pub fn fail(&mut self, task_id: Uuid, output: Value) -> Result<(), PlanError> {
        let task = self
            .snapshot
            .graph
            .tasks
            .get_mut(&task_id)
            .ok_or(PlanError::MissingTask(task_id))?;
        if !matches!(task.status, WorkStatus::Running | WorkStatus::Waiting) {
            return Err(PlanError::TaskNotReady(task_id));
        }
        task.status = WorkStatus::Failed;
        task.output = Some(output);
        self.snapshot.running_order.retain(|id| *id != task_id);
        Ok(())
    }

    /// Pause every non-terminal node in a goal graph.
    pub fn pause_all(&mut self) {
        for task in self.snapshot.graph.tasks.values_mut() {
            if !matches!(
                task.status,
                WorkStatus::Completed | WorkStatus::Failed | WorkStatus::Cancelled
            ) {
                task.status = WorkStatus::Paused;
            }
        }
        self.snapshot.running_order.clear();
    }

    /// Resume every task explicitly paused with its goal.
    pub fn resume_all(&mut self) {
        for task in self.snapshot.graph.tasks.values_mut() {
            if task.status == WorkStatus::Paused {
                task.status = WorkStatus::Queued;
            }
        }
    }

    /// Cancel every unfinished node in a goal graph.
    pub fn cancel_all(&mut self) {
        for task in self.snapshot.graph.tasks.values_mut() {
            if !matches!(task.status, WorkStatus::Completed | WorkStatus::Cancelled) {
                task.status = WorkStatus::Cancelled;
            }
        }
        self.snapshot.running_order.clear();
    }

    /// Pause active background tasks for the priority user lane.
    pub fn preempt_for_user(&mut self) {
        for task_id in std::mem::take(&mut self.snapshot.running_order) {
            if let Some(task) = self.snapshot.graph.tasks.get_mut(&task_id) {
                task.status = WorkStatus::Paused;
            }
        }
    }

    /// Advance only the testable/scheduler clock; no wall-clock sleep is required.
    pub fn advance_virtual_time(&mut self, seconds: u64) {
        self.snapshot.elapsed_virtual_seconds = self
            .snapshot
            .elapsed_virtual_seconds
            .saturating_add(seconds);
    }

    /// Validate that delegated work cannot widen authority or execution zone.
    ///
    /// # Errors
    ///
    /// Returns `AuthorityWidening` when tools, zone, or nesting exceed the parent.
    pub fn validate_delegation(&self, child: &Task) -> Result<(), PlanError> {
        let Some(parent_id) = child.parent_task_id else {
            return Ok(());
        };
        let parent = self
            .snapshot
            .graph
            .tasks
            .get(&parent_id)
            .ok_or(PlanError::MissingTask(parent_id))?;
        if !child.tool_scopes.is_subset(&parent.tool_scopes) {
            return Err(PlanError::AuthorityWidening {
                task: child.id,
                detail: "tool scopes exceed parent".into(),
            });
        }
        if zone_rank(child.execution_zone) > zone_rank(parent.execution_zone) {
            return Err(PlanError::AuthorityWidening {
                task: child.id,
                detail: "execution zone exceeds parent".into(),
            });
        }
        let mut depth = 1;
        let mut cursor = parent.parent_task_id;
        while let Some(id) = cursor {
            depth += 1;
            cursor = self
                .snapshot
                .graph
                .tasks
                .get(&id)
                .ok_or(PlanError::MissingTask(id))?
                .parent_task_id;
        }
        if depth > self.maximum_delegation_depth {
            return Err(PlanError::AuthorityWidening {
                task: child.id,
                detail: "delegation depth exceeded".into(),
            });
        }
        Ok(())
    }
}

fn is_consequential(task: &Task) -> bool {
    matches!(task.risk.as_str(), "consequential" | "irreversible")
}

fn retry_safe(task: &Task) -> bool {
    !is_consequential(task) || task.idempotency_key.is_some()
}

fn zone_rank(zone: ExecutionZone) -> u8 {
    match zone {
        ExecutionZone::Isolated => 0,
        ExecutionZone::Workspace => 1,
        ExecutionZone::Desktop => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(goal_id: Uuid, title: &str) -> Task {
        Task {
            id: Uuid::now_v7(),
            goal_id,
            parent_task_id: None,
            title: title.into(),
            assigned_agent: "executor".into(),
            workspace: None,
            browser_profile: None,
            tool_scopes: BTreeSet::new(),
            risk: "read".into(),
            execution_zone: ExecutionZone::Isolated,
            max_attempts: 1,
            attempt: 0,
            idempotency_key: None,
            checkpoint_id: None,
            status: WorkStatus::Queued,
            progress: 0,
            output: None,
        }
    }

    #[test]
    fn cycles_are_rejected() {
        let goal_id = Uuid::now_v7();
        let a = task(goal_id, "a");
        let b = task(goal_id, "b");
        let graph = TaskGraph {
            goal_id,
            revision: 1,
            tasks: [(a.id, a.clone()), (b.id, b.clone())].into(),
            edges: vec![(a.id, b.id), (b.id, a.id)],
        };
        assert_eq!(graph.validate(), Err(PlanError::Cycle));
    }

    #[test]
    fn goal_requires_verified_success_criteria() {
        let mut goal = Goal::new("ship", vec!["tests pass".into()], "ui");
        assert!(goal.complete(&BTreeSet::new(), json!({"ok":true})).is_err());
        assert_eq!(goal.status, WorkStatus::Queued);
    }

    #[test]
    fn multi_hour_goal_recovers_without_duplicate_consequential_effect() {
        let goal_id = Uuid::now_v7();
        let mut observe = task(goal_id, "research");
        observe.max_attempts = 3;
        let mut publish = task(goal_id, "publish");
        publish.risk = "consequential".into();
        publish.max_attempts = 3;
        publish.idempotency_key = Some("publish:release-1".into());
        let graph = TaskGraph {
            goal_id,
            revision: 1,
            tasks: [(observe.id, observe.clone()), (publish.id, publish.clone())].into(),
            edges: vec![(observe.id, publish.id)],
        };
        let mut supervisor = DurableSupervisor::new(graph, 3, 3).expect("supervisor");
        supervisor.start(observe.id).expect("start observe");
        supervisor
            .provider_failed(observe.id)
            .expect("provider failure");
        supervisor.start(observe.id).expect("retry observe");
        supervisor
            .complete(observe.id, json!({"sources":3}), true, None)
            .expect("complete observe");
        supervisor.advance_virtual_time(4 * 60 * 60);
        supervisor.start(publish.id).expect("start publish");
        supervisor
            .complete(
                publish.id,
                json!({"published":true}),
                true,
                Some("sha256:fixture".into()),
            )
            .expect("complete publish");

        let encoded = serde_json::to_vec(supervisor.snapshot()).expect("persist snapshot");
        let snapshot: SupervisorSnapshot = serde_json::from_slice(&encoded).expect("reload");
        let recovered = DurableSupervisor::recover(snapshot, 3, 3).expect("recover");
        assert_eq!(recovered.snapshot().elapsed_virtual_seconds, 14_400);
        assert_eq!(recovered.snapshot().receipts.len(), 1);
        assert_eq!(
            recovered.snapshot().graph.tasks[&publish.id].status,
            WorkStatus::Completed
        );
        assert!(recovered.ready_tasks().is_empty());
    }

    #[test]
    fn crash_during_unsafe_effect_waits_instead_of_repeating() {
        let goal_id = Uuid::now_v7();
        let mut unsafe_task = task(goal_id, "external effect");
        unsafe_task.risk = "consequential".into();
        unsafe_task.idempotency_key = Some("effect:1".into());
        unsafe_task.max_attempts = 1;
        let graph = TaskGraph {
            goal_id,
            revision: 1,
            tasks: [(unsafe_task.id, unsafe_task.clone())].into(),
            edges: vec![],
        };
        let mut supervisor = DurableSupervisor::new(graph, 1, 3).expect("supervisor");
        supervisor.start(unsafe_task.id).expect("start");
        let recovered = DurableSupervisor::recover(supervisor.snapshot().clone(), 1, 3)
            .expect("recover safely");
        assert_eq!(
            recovered.snapshot().graph.tasks[&unsafe_task.id].status,
            WorkStatus::Waiting
        );
    }

    #[test]
    fn native_controls_preserve_attempts_and_require_explicit_retry() {
        let goal_id = Uuid::now_v7();
        let mut task = task(goal_id, "background work");
        task.max_attempts = 3;
        let task_id = task.id;
        let graph = TaskGraph {
            goal_id,
            revision: 1,
            tasks: [(task.id, task)].into(),
            edges: vec![],
        };
        let mut supervisor = DurableSupervisor::new(graph, 1, 3).expect("supervisor");
        supervisor.start(task_id).expect("start");
        supervisor
            .bind_checkpoint(task_id, "ses_background")
            .expect("bind checkpoint");
        supervisor.pause(task_id).expect("pause");
        assert_eq!(
            supervisor.snapshot().graph.tasks[&task_id].status,
            WorkStatus::Paused
        );
        supervisor.resume(task_id).expect("resume");
        supervisor.start(task_id).expect("restart");
        supervisor
            .fail(task_id, json!({"error":"provider disconnected"}))
            .expect("fail");
        assert_eq!(
            supervisor.snapshot().graph.tasks[&task_id].status,
            WorkStatus::Failed
        );
        supervisor.retry(task_id).expect("retry");
        let retried = &supervisor.snapshot().graph.tasks[&task_id];
        assert_eq!(retried.status, WorkStatus::Queued);
        assert_eq!(retried.attempt, 2);
        assert!(retried.checkpoint_id.is_none());
    }

    #[test]
    fn approval_waiting_is_a_durable_suspension_not_a_failure() {
        let goal_id = Uuid::now_v7();
        let task = task(goal_id, "approval gated");
        let task_id = task.id;
        let graph = TaskGraph {
            goal_id,
            revision: 1,
            tasks: [(task.id, task)].into(),
            edges: vec![],
        };
        let mut supervisor = DurableSupervisor::new(graph, 1, 3).expect("supervisor");
        supervisor.start(task_id).expect("start");
        assert!(matches!(
            supervisor
                .wait_for_approval(task_id, "write workspace")
                .expect("wait"),
            ScheduleDecision::WaitForApproval { .. }
        ));
        assert_eq!(
            supervisor.snapshot().graph.tasks[&task_id].status,
            WorkStatus::Waiting
        );
        assert!(supervisor.snapshot().running_order.is_empty());
        supervisor
            .resume_after_approval(task_id)
            .expect("resume approval");
        assert_eq!(
            supervisor.snapshot().graph.tasks[&task_id].status,
            WorkStatus::Running
        );
    }

    #[test]
    fn delegated_task_cannot_widen_parent_tools_or_desktop_zone() {
        let goal_id = Uuid::now_v7();
        let mut parent = task(goal_id, "parent");
        parent.tool_scopes = ["files.read".into()].into();
        parent.execution_zone = ExecutionZone::Workspace;
        let mut child = task(goal_id, "child");
        child.parent_task_id = Some(parent.id);
        child.tool_scopes = ["files.read".into(), "desktop.control".into()].into();
        child.execution_zone = ExecutionZone::Desktop;
        let graph = TaskGraph {
            goal_id,
            revision: 1,
            tasks: [(parent.id, parent), (child.id, child.clone())].into(),
            edges: vec![],
        };
        let supervisor = DurableSupervisor::new(graph, 3, 3).expect("supervisor");
        assert!(matches!(
            supervisor.validate_delegation(&child),
            Err(PlanError::AuthorityWidening { .. })
        ));
    }
}
