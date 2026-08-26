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
}
