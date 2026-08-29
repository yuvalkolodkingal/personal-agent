//! Durable goal/task projection and resident background executor.

#![allow(clippy::needless_pass_by_value)] // Tauri owns deserialized IPC values.

use super::DesktopState;
use chrono::{DateTime, Utc};
use personal_agent_agent::{DurableSupervisor, ExecutionZone, Goal, Task, TaskGraph, WorkStatus};
use personal_agent_contracts::proto::EventEnvelope;
use personal_agent_runtime::{AgentRuntime, PromptOptions, RuntimeAnswer, SessionOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Emitter, Manager};
use uuid::Uuid;

const MAXIMUM_PARALLELISM: usize = 2;
const MAXIMUM_DELEGATION_DEPTH: usize = 3;
const RESIDENT_TICK_SECONDS: u64 = 2;
const MAX_GOAL_TEXT_BYTES: usize = 65_536;
const SNAPSHOT_DEBOUNCE: Duration = Duration::from_millis(250);
const BACKGROUND_GOAL_SYSTEM_PROMPT: &str = "This is a durable background goal task, not an interactive user turn. Work only on the named goal and observable success criterion. Preserve user files and existing changes. All tools remain behind the native policy gateway. Pause for native approval before consequential or external effects. Return a concise result with verification evidence.";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PendingApproval {
    goal_id: Uuid,
    task_id: Uuid,
    session_id: String,
    request_id: String,
    reason: String,
    requested_at: DateTime<Utc>,
}

#[derive(Clone)]
struct ManagedGoal {
    goal: Goal,
    supervisor: DurableSupervisor,
    approvals: BTreeMap<Uuid, PendingApproval>,
}

type PendingApprovalsByGoal = BTreeMap<Uuid, BTreeMap<Uuid, PendingApproval>>;
type ReplayedGoalEvents = (
    BTreeMap<Uuid, Goal>,
    PendingApprovalsByGoal,
    Vec<GoalActivityView>,
);

#[derive(Default)]
struct GoalReplayState {
    goals: BTreeMap<Uuid, Goal>,
    approvals: PendingApprovalsByGoal,
    activities: Vec<GoalActivityView>,
}

pub(crate) struct GoalsHostState {
    goals: tokio::sync::Mutex<BTreeMap<Uuid, Arc<ManagedGoal>>>,
    persistence: Arc<GoalPersistence>,
    resident_active: AtomicBool,
    recovered_tasks: usize,
    activities: tokio::sync::Mutex<Vec<GoalActivityView>>,
}

struct PendingGoalCheckpoint {
    snapshot: personal_agent_agent::SupervisorSnapshot,
    events: Vec<EventEnvelope>,
}

#[derive(Default)]
struct GoalPersistence {
    generation: AtomicU64,
    pending: Mutex<BTreeMap<Uuid, PendingGoalCheckpoint>>,
    write_gate: Mutex<()>,
    #[cfg(test)]
    successful_writes: std::sync::atomic::AtomicUsize,
}

impl GoalPersistence {
    fn queue(
        self: &Arc<Self>,
        profile: Arc<Mutex<personal_agent_core::ProfileState>>,
        snapshot: personal_agent_agent::SupervisorSnapshot,
        event: EventEnvelope,
        critical: bool,
    ) -> Result<(), String> {
        let generation = {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| "goal persistence queue lock is poisoned".to_owned())?;
            let entry =
                pending
                    .entry(snapshot.graph.goal_id)
                    .or_insert_with(|| PendingGoalCheckpoint {
                        snapshot: snapshot.clone(),
                        events: Vec::new(),
                    });
            entry.snapshot = snapshot;
            entry.events.push(event);
            self.generation.fetch_add(1, Ordering::SeqCst) + 1
        };
        if critical {
            return self.flush(&profile);
        }
        let persistence = Arc::clone(self);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                tokio::time::sleep(SNAPSHOT_DEBOUNCE).await;
                let _ = tokio::task::spawn_blocking(move || {
                    persistence.flush_generation(&profile, generation);
                })
                .await;
            });
        } else {
            std::thread::spawn(move || {
                std::thread::sleep(SNAPSHOT_DEBOUNCE);
                persistence.flush_generation(&profile, generation);
            });
        }
        Ok(())
    }

    fn take_generation(&self, generation: u64) -> Option<BTreeMap<Uuid, PendingGoalCheckpoint>> {
        let mut pending = self.pending.lock().ok()?;
        (self.generation.load(Ordering::SeqCst) == generation)
            .then(|| std::mem::take(&mut *pending))
    }

    fn updates(
        pending: &BTreeMap<Uuid, PendingGoalCheckpoint>,
    ) -> Vec<personal_agent_core::SupervisorCheckpointUpdate> {
        pending
            .values()
            .map(|pending| personal_agent_core::SupervisorCheckpointUpdate {
                snapshot: pending.snapshot.clone(),
                events: pending.events.clone(),
            })
            .collect()
    }

    fn restore_pending(&self, failed: BTreeMap<Uuid, PendingGoalCheckpoint>) {
        let Ok(mut pending) = self.pending.lock() else {
            tracing::error!("goal persistence queue lock is poisoned after a failed write");
            return;
        };
        for (goal_id, mut failed) in failed {
            if let Some(newer) = pending.remove(&goal_id) {
                failed.events.extend(newer.events);
                failed.events.sort_by_key(|event| event.monotonic_sequence);
                failed.events.dedup_by_key(|event| event.monotonic_sequence);
                failed.snapshot = newer.snapshot;
            }
            pending.insert(goal_id, failed);
        }
    }

    fn flush_generation(
        &self,
        profile: &Mutex<personal_agent_core::ProfileState>,
        generation: u64,
    ) {
        let Ok(_write_guard) = self.write_gate.lock() else {
            tracing::error!("goal persistence write gate is poisoned");
            return;
        };
        let Some(pending) = self.take_generation(generation) else {
            return;
        };
        let updates = Self::updates(&pending);
        let result = profile
            .lock()
            .map_err(|_| "profile state lock is poisoned".to_owned())
            .and_then(|mut profile| {
                profile
                    .save_supervisor_checkpoint_updates(&updates)
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => {
                #[cfg(test)]
                self.successful_writes.fetch_add(1, Ordering::SeqCst);
            }
            Err(error) => {
                tracing::error!(%error, "debounced goal snapshots could not be persisted");
                self.restore_pending(pending);
            }
        }
    }

    fn flush(&self, profile: &Mutex<personal_agent_core::ProfileState>) -> Result<(), String> {
        let _write_guard = self
            .write_gate
            .lock()
            .map_err(|_| "goal persistence write gate is poisoned".to_owned())?;
        let pending = {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| "goal persistence queue lock is poisoned".to_owned())?;
            self.generation.fetch_add(1, Ordering::SeqCst);
            std::mem::take(&mut *pending)
        };
        if pending.is_empty() {
            return Ok(());
        }
        let updates = Self::updates(&pending);
        let result = profile
            .lock()
            .map_err(|_| "profile state lock is poisoned".to_owned())?
            .save_supervisor_checkpoint_updates(&updates)
            .map_err(|error| error.to_string());
        if let Err(error) = result {
            self.restore_pending(pending);
            return Err(error);
        }
        #[cfg(test)]
        self.successful_writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct GoalActivityView {
    sequence: u64,
    event_type: String,
    goal_id: Option<Uuid>,
    task_id: Option<Uuid>,
    timestamp: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct GoalView {
    goal: Goal,
    tasks: Vec<Task>,
    edges: Vec<(Uuid, Uuid)>,
    approvals: Vec<PendingApproval>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct GoalsSnapshotView {
    goals: Vec<GoalView>,
    activities: Vec<GoalActivityView>,
    resident_active: bool,
    recovered_tasks: usize,
    maximum_parallelism: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct GoalActionResult {
    snapshot: GoalsSnapshotView,
    projection: personal_agent_core::AppProjection,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum GoalAction {
    Create {
        objective: String,
        success_criteria: Vec<String>,
        #[serde(default)]
        priority: i32,
    },
    PauseGoal {
        goal_id: Uuid,
    },
    ResumeGoal {
        goal_id: Uuid,
    },
    CancelGoal {
        goal_id: Uuid,
    },
    RetryGoal {
        goal_id: Uuid,
    },
    PauseTask {
        goal_id: Uuid,
        task_id: Uuid,
    },
    ResumeTask {
        goal_id: Uuid,
        task_id: Uuid,
    },
    CancelTask {
        goal_id: Uuid,
        task_id: Uuid,
    },
    RetryTask {
        goal_id: Uuid,
        task_id: Uuid,
    },
    AnswerApproval {
        goal_id: Uuid,
        task_id: Uuid,
        allow: bool,
    },
}

impl GoalsHostState {
    pub(crate) fn load(
        profile: &mut personal_agent_core::ProfileState,
        working_directory: &str,
    ) -> Result<Self, String> {
        let (mut definitions, mut approvals, activities) = replay_goal_events(profile)?;
        let mut goals = BTreeMap::new();
        let mut recovered_tasks = 0;
        for goal in definitions.values_mut() {
            let persisted = profile
                .supervisor_snapshot(goal.id)
                .map_err(|error| error.to_string())?;
            let supervisor = if let Some(snapshot) = persisted {
                let running = snapshot.running_order.len();
                let recovered = DurableSupervisor::recover(
                    snapshot,
                    MAXIMUM_PARALLELISM,
                    MAXIMUM_DELEGATION_DEPTH,
                )
                .map_err(|error| error.to_string())?;
                recovered_tasks += running;
                if running > 0 {
                    profile
                        .save_supervisor_snapshot(recovered.snapshot())
                        .map_err(|error| error.to_string())?;
                }
                recovered
            } else {
                goal.plan_revision = goal.plan_revision.max(1);
                let supervisor = DurableSupervisor::new(
                    task_graph(goal, working_directory),
                    MAXIMUM_PARALLELISM,
                    MAXIMUM_DELEGATION_DEPTH,
                )
                .map_err(|error| error.to_string())?;
                profile
                    .save_supervisor_snapshot(supervisor.snapshot())
                    .map_err(|error| error.to_string())?;
                supervisor
            };
            goals.insert(
                goal.id,
                Arc::new(ManagedGoal {
                    goal: goal.clone(),
                    supervisor,
                    approvals: approvals.remove(&goal.id).unwrap_or_default(),
                }),
            );
        }
        Ok(Self {
            goals: tokio::sync::Mutex::new(goals),
            persistence: Arc::new(GoalPersistence::default()),
            resident_active: AtomicBool::new(false),
            recovered_tasks,
            activities: tokio::sync::Mutex::new(activities),
        })
    }

    pub(crate) fn flush_persistence(
        &self,
        profile: &Mutex<personal_agent_core::ProfileState>,
    ) -> Result<(), String> {
        self.persistence.flush(profile)
    }
}

fn replay_goal_events(
    profile: &personal_agent_core::ProfileState,
) -> Result<ReplayedGoalEvents, String> {
    replay_goal_events_with_checkpoints(profile, true)
}

fn replay_goal_events_with_checkpoints(
    profile: &personal_agent_core::ProfileState,
    use_checkpoints: bool,
) -> Result<ReplayedGoalEvents, String> {
    let (mut state, mut after) = if use_checkpoints {
        checkpointed_goal_replay_base(profile)?.unwrap_or_default()
    } else {
        (GoalReplayState::default(), 0)
    };
    loop {
        let events = profile
            .events_after(after, 1_000)
            .map_err(|error| error.to_string())?;
        if events.is_empty() {
            break;
        }
        for event in events {
            after = event.monotonic_sequence;
            apply_goal_event(&mut state, &event)?;
        }
    }
    state.activities.sort_by_key(|activity| activity.sequence);
    state.activities.dedup_by_key(|activity| activity.sequence);
    if state.activities.len() > 100 {
        state.activities.drain(..state.activities.len() - 100);
    }
    Ok((state.goals, state.approvals, state.activities))
}

fn checkpointed_goal_replay_base(
    profile: &personal_agent_core::ProfileState,
) -> Result<Option<(GoalReplayState, u64)>, String> {
    let checkpoints = profile
        .supervisor_recovery_checkpoints()
        .map_err(|error| error.to_string())?;
    if checkpoints.is_empty()
        || checkpoints.iter().any(|checkpoint| {
            !checkpoint.replay_base_complete
                || checkpoint.last_sequence == 0
                || checkpoint.latest_goal_event.is_none()
        })
    {
        return Ok(None);
    }

    let mut state = GoalReplayState::default();
    // Recovery replays from the oldest per-goal checkpoint. A newer checkpoint
    // for one goal cannot prove that another goal has no durable, debounced
    // tail before that global sequence.
    let mut after = u64::MAX;
    for checkpoint in checkpoints {
        let event = checkpoint
            .latest_goal_event
            .as_ref()
            .expect("complete checkpoints have a latest goal event");
        if event.monotonic_sequence != checkpoint.last_sequence {
            return Ok(None);
        }
        let payload = event.payload().map_err(|error| error.to_string())?;
        let Some(goal) = goal_from_event(event, &payload) else {
            return Ok(None);
        };
        if goal.id != checkpoint.snapshot.graph.goal_id {
            return Ok(None);
        }
        state.goals.insert(goal.id, goal);
        for pending in &checkpoint.pending_approval_events {
            let payload = pending.payload().map_err(|error| error.to_string())?;
            let approval = serde_json::from_value::<PendingApproval>(payload)
                .map_err(|error| error.to_string())?;
            state
                .approvals
                .entry(approval.goal_id)
                .or_default()
                .insert(approval.task_id, approval);
        }
        state.activities.extend(
            checkpoint
                .recent_activities
                .into_iter()
                .map(activity_from_checkpoint),
        );
        after = after.min(checkpoint.last_sequence);
    }
    Ok(Some((state, after)))
}

fn apply_goal_event(state: &mut GoalReplayState, event: &EventEnvelope) -> Result<(), String> {
    if !is_goal_event(&event.r#type) {
        return Ok(());
    }
    let payload = event.payload().map_err(|error| error.to_string())?;
    if let Some(goal) = goal_from_event(event, &payload) {
        state.goals.insert(goal.id, goal);
    }
    apply_approval_event(&mut state.approvals, event, &payload);
    state.activities.push(activity_from_event(event, &payload));
    Ok(())
}

fn goal_from_event(event: &EventEnvelope, payload: &Value) -> Option<Goal> {
    if event.r#type == "goal.created" {
        serde_json::from_value(payload.clone()).ok().or_else(|| {
            payload
                .get("goal")
                .and_then(|goal| serde_json::from_value(goal.clone()).ok())
        })
    } else {
        payload
            .get("goal")
            .and_then(|goal| serde_json::from_value(goal.clone()).ok())
    }
}

fn apply_approval_event(
    approvals: &mut PendingApprovalsByGoal,
    event: &EventEnvelope,
    payload: &Value,
) {
    if event.r#type == "approval.requested"
        && let Ok(approval) = serde_json::from_value::<PendingApproval>(payload.clone())
    {
        approvals
            .entry(approval.goal_id)
            .or_default()
            .insert(approval.task_id, approval);
    }
    if event.r#type == "approval.resolved"
        && let (Some(goal_id), Some(task_id)) = (
            payload.get("goal_id").and_then(Value::as_str),
            payload.get("task_id").and_then(Value::as_str),
        )
        && let (Ok(goal_id), Ok(task_id)) = (goal_id.parse(), task_id.parse())
        && let Some(items) = approvals.get_mut(&goal_id)
    {
        items.remove(&task_id);
    }
    if payload
        .get("approvals_resolved")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
        && let Some(goal_id) = payload
            .get("goal_id")
            .and_then(Value::as_str)
            .and_then(|value| value.parse().ok())
    {
        approvals.remove(&goal_id);
    }
    if payload
        .get("approval_resolved")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && let (Some(goal_id), Some(task_id)) = (
            payload
                .get("goal_id")
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok()),
            payload
                .get("task_id")
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok()),
        )
        && let Some(items) = approvals.get_mut(&goal_id)
    {
        items.remove(&task_id);
    }
}

fn activity_from_checkpoint(
    activity: personal_agent_core::SupervisorActivityCheckpoint,
) -> GoalActivityView {
    GoalActivityView {
        sequence: activity.sequence,
        event_type: activity.event_type,
        goal_id: activity.goal_id.and_then(|value| value.parse().ok()),
        task_id: activity.task_id.and_then(|value| value.parse().ok()),
        timestamp: activity.timestamp,
    }
}

fn is_goal_event(event_type: &str) -> bool {
    event_type.starts_with("goal.")
        || event_type.starts_with("task.")
        || matches!(event_type, "approval.requested" | "approval.resolved")
}

fn activity_from_event(event: &EventEnvelope, payload: &Value) -> GoalActivityView {
    GoalActivityView {
        sequence: event.monotonic_sequence,
        event_type: event.r#type.clone(),
        goal_id: payload
            .get("goal_id")
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str)
            .and_then(|value| value.parse().ok()),
        task_id: payload
            .get("task_id")
            .and_then(Value::as_str)
            .and_then(|value| value.parse().ok()),
        timestamp: event.wall_clock_timestamp.clone(),
    }
}

fn task_graph(goal: &Goal, working_directory: &str) -> TaskGraph {
    let mut tasks = BTreeMap::new();
    let mut edges = Vec::new();
    let mut previous = None;
    for (index, criterion) in goal.success_criteria.iter().enumerate() {
        let id = Uuid::now_v7();
        tasks.insert(
            id,
            Task {
                id,
                goal_id: goal.id,
                parent_task_id: None,
                title: criterion.clone(),
                assigned_agent: "build".into(),
                workspace: Some(working_directory.into()),
                browser_profile: None,
                tool_scopes: [
                    "workspace.read".into(),
                    "workspace.write".into(),
                    "shell".into(),
                ]
                .into(),
                risk: "bounded".into(),
                execution_zone: ExecutionZone::Workspace,
                max_attempts: 3,
                attempt: 0,
                idempotency_key: None,
                checkpoint_id: None,
                status: WorkStatus::Queued,
                progress: u8::from(index == 0),
                output: None,
            },
        );
        if let Some(previous) = previous {
            edges.push((previous, id));
        }
        previous = Some(id);
    }
    TaskGraph {
        goal_id: goal.id,
        revision: goal.plan_revision.max(1),
        tasks,
        edges,
    }
}

#[tauri::command]
pub(crate) async fn goals_snapshot(
    host: tauri::State<'_, GoalsHostState>,
) -> Result<GoalsSnapshotView, String> {
    snapshot_view(&host).await
}

#[tauri::command]
#[allow(clippy::too_many_lines)] // Tagged state transitions stay auditable in one IPC boundary.
pub(crate) async fn goals_execute(
    action: Value,
    app: tauri::AppHandle,
    host: tauri::State<'_, GoalsHostState>,
    desktop: tauri::State<'_, DesktopState>,
) -> Result<GoalActionResult, String> {
    let action: GoalAction =
        serde_json::from_value(action).map_err(|error| format!("invalid goal action: {error}"))?;
    let (projection, message) = match action {
        GoalAction::Create {
            objective,
            success_criteria,
            priority,
        } => {
            let objective = bounded_text(&objective, "goal objective", MAX_GOAL_TEXT_BYTES)?;
            let criteria = success_criteria
                .iter()
                .map(|criterion| bounded_text(criterion, "success criterion", 4_096))
                .collect::<Result<Vec<_>, _>>()?;
            if criteria.is_empty() {
                return Err("at least one observable success criterion is required".into());
            }
            let config = desktop
                .config
                .read()
                .map_err(|_| "configuration lock is poisoned".to_owned())?
                .clone();
            let working_directory = config.runtime.working_directory.clone();
            let mut goal = Goal::new(objective, criteria, "desktop-ui");
            goal.priority = priority;
            goal.plan_revision = 1;
            if config.agent.default_token_budget > 0 {
                goal.budgets.tokens = Some(config.agent.default_token_budget);
            }
            if config.agent.default_cost_budget_microusd > 0 {
                goal.budgets.cost_usd = microusd_to_usd(config.agent.default_cost_budget_microusd);
            }
            if config.agent.default_wall_time_minutes > 0 {
                goal.budgets.wall_time_seconds =
                    Some(u64::from(config.agent.default_wall_time_minutes).saturating_mul(60));
            }
            if config.agent.default_tool_call_budget > 0 {
                goal.budgets.tool_calls = Some(config.agent.default_tool_call_budget);
            }
            let supervisor = DurableSupervisor::new(
                task_graph(&goal, &working_directory),
                MAXIMUM_PARALLELISM,
                MAXIMUM_DELEGATION_DEPTH,
            )
            .map_err(|error| error.to_string())?;
            let managed = ManagedGoal {
                goal: goal.clone(),
                supervisor,
                approvals: BTreeMap::new(),
            };
            let projection = persist_managed(
                &host,
                &desktop,
                &managed,
                "goal.created",
                json!(goal.clone()),
            )?;
            host.goals.lock().await.insert(goal.id, Arc::new(managed));
            record_activity(&host, &projection, "goal.created", Some(goal.id), None).await;
            (
                projection,
                "Goal created and queued in the resident supervisor.".into(),
            )
        }
        GoalAction::PauseGoal { goal_id } => {
            let (projection, sessions) =
                mutate_goal(&host, &desktop, goal_id, "goal.paused", |managed| {
                    let sessions = checkpoints(managed);
                    let running_tasks_stopped = managed.supervisor.snapshot().running_order.len();
                    let approvals_resolved = managed.approvals.len();
                    managed.supervisor.pause_all();
                    managed.goal.status = WorkStatus::Paused;
                    managed.approvals.clear();
                    Ok((
                        sessions,
                        json!({
                            "goal_id": goal_id,
                            "running_tasks_stopped": running_tasks_stopped,
                            "approvals_resolved": approvals_resolved,
                        }),
                    ))
                })
                .await?;
            abort_sessions(&desktop, sessions).await;
            (
                projection,
                "Goal paused. Active runtime turns were stopped safely.".into(),
            )
        }
        GoalAction::ResumeGoal { goal_id } => {
            let (projection, ()) =
                mutate_goal(&host, &desktop, goal_id, "goal.resumed", |managed| {
                    if !matches!(
                        managed.goal.status,
                        WorkStatus::Paused | WorkStatus::Waiting
                    ) {
                        return Err("only paused or waiting goals can resume".into());
                    }
                    managed.supervisor.resume_all();
                    managed.goal.status = WorkStatus::Running;
                    Ok(((), json!({"goal_id": goal_id})))
                })
                .await?;
            (projection, "Goal resumed in the background queue.".into())
        }
        GoalAction::CancelGoal { goal_id } => {
            let (projection, sessions) =
                mutate_goal(&host, &desktop, goal_id, "goal.cancelled", |managed| {
                    let sessions = checkpoints(managed);
                    let running_tasks_stopped = managed.supervisor.snapshot().running_order.len();
                    let approvals_resolved = managed.approvals.len();
                    managed.supervisor.cancel_all();
                    managed.goal.status = WorkStatus::Cancelled;
                    managed.approvals.clear();
                    Ok((
                        sessions,
                        json!({
                            "goal_id": goal_id,
                            "running_tasks_stopped": running_tasks_stopped,
                            "approvals_resolved": approvals_resolved,
                        }),
                    ))
                })
                .await?;
            abort_sessions(&desktop, sessions).await;
            (
                projection,
                "Goal cancelled; its durable history was retained.".into(),
            )
        }
        GoalAction::RetryGoal { goal_id } => {
            let (projection, sessions) = mutate_goal(
                &host,
                &desktop,
                goal_id,
                "goal.retry_requested",
                |managed| {
                    let sessions = checkpoints(managed);
                    let running_tasks_stopped = managed.supervisor.snapshot().running_order.len();
                    let approvals_resolved = managed.approvals.len();
                    let ids = managed
                        .supervisor
                        .snapshot()
                        .graph
                        .tasks
                        .values()
                        .filter(|task| {
                            matches!(task.status, WorkStatus::Failed | WorkStatus::Waiting)
                        })
                        .map(|task| task.id)
                        .collect::<Vec<_>>();
                    if ids.is_empty() {
                        return Err("the goal has no failed or waiting tasks to retry".into());
                    }
                    for id in ids {
                        managed
                            .supervisor
                            .retry(id)
                            .map_err(|error| error.to_string())?;
                    }
                    managed.approvals.clear();
                    managed.goal.status = WorkStatus::Running;
                    Ok((
                        sessions,
                        json!({
                            "goal_id": goal_id,
                            "running_tasks_stopped": running_tasks_stopped,
                            "approvals_resolved": approvals_resolved,
                        }),
                    ))
                },
            )
            .await?;
            abort_sessions(&desktop, sessions).await;
            (
                projection,
                "Retry queued with the existing attempt history.".into(),
            )
        }
        GoalAction::PauseTask { goal_id, task_id } => {
            let (projection, session) = task_transition(
                &host,
                &desktop,
                goal_id,
                task_id,
                "task.paused",
                |managed| managed.supervisor.pause(task_id),
            )
            .await?;
            abort_sessions(&desktop, session.into_iter().collect()).await;
            (projection, "Task paused.".into())
        }
        GoalAction::ResumeTask { goal_id, task_id } => {
            let (projection, _) = task_transition(
                &host,
                &desktop,
                goal_id,
                task_id,
                "task.resumed",
                |managed| managed.supervisor.resume(task_id),
            )
            .await?;
            (projection, "Task returned to the queue.".into())
        }
        GoalAction::CancelTask { goal_id, task_id } => {
            let (projection, session) = task_transition(
                &host,
                &desktop,
                goal_id,
                task_id,
                "task.cancelled",
                |managed| managed.supervisor.cancel(task_id),
            )
            .await?;
            abort_sessions(&desktop, session.into_iter().collect()).await;
            (projection, "Task cancelled.".into())
        }
        GoalAction::RetryTask { goal_id, task_id } => {
            let (projection, session) = task_transition(
                &host,
                &desktop,
                goal_id,
                task_id,
                "task.retry_requested",
                |managed| managed.supervisor.retry(task_id),
            )
            .await?;
            abort_sessions(&desktop, session.into_iter().collect()).await;
            (projection, "Task retry queued.".into())
        }
        GoalAction::AnswerApproval {
            goal_id,
            task_id,
            allow,
        } => {
            let approval = {
                let goals = host.goals.lock().await;
                goals
                    .get(&goal_id)
                    .and_then(|managed| managed.approvals.get(&task_id))
                    .cloned()
                    .ok_or_else(|| "the approval request is no longer available".to_owned())?
            };
            desktop
                .runtime
                .lock()
                .await
                .answer(
                    &approval.session_id,
                    RuntimeAnswer {
                        request_id: approval.request_id,
                        answer: json!({"kind":"permission", "reply": if allow { "once" } else { "reject" }}),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            let (projection, ()) = mutate_goal(
                &host,
                &desktop,
                goal_id,
                "approval.resolved",
                |managed| {
                    managed
                        .supervisor
                        .resume_after_approval(task_id)
                        .map_err(|error| error.to_string())?;
                    managed.approvals.remove(&task_id);
                    Ok(((), json!({"goal_id": goal_id, "task_id": task_id, "allow": allow, "resumed": true})))
                },
            )
            .await?;
            (
                projection,
                if allow {
                    "Allowed once; the same runtime turn is resuming."
                } else {
                    "Approval rejected; the runtime will finish without that effect."
                }
                .into(),
            )
        }
    };
    emit_snapshot(&app).await?;
    drain_ready(&app).await?;
    Ok(GoalActionResult {
        snapshot: snapshot_view(&host).await?,
        projection,
        message,
    })
}

async fn task_transition(
    host: &GoalsHostState,
    desktop: &DesktopState,
    goal_id: Uuid,
    task_id: Uuid,
    event_type: &str,
    transition: impl FnOnce(&mut ManagedGoal) -> Result<(), personal_agent_agent::PlanError>,
) -> Result<(personal_agent_core::AppProjection, Option<String>), String> {
    mutate_goal(host, desktop, goal_id, event_type, |managed| {
        let task = managed
            .supervisor
            .snapshot()
            .graph
            .tasks
            .get(&task_id)
            .ok_or_else(|| "task not found".to_owned())?;
        let checkpoint = task.checkpoint_id.clone();
        let was_running = task.status == WorkStatus::Running;
        let approval_resolved = managed.approvals.contains_key(&task_id);
        transition(managed).map_err(|error| error.to_string())?;
        managed.approvals.remove(&task_id);
        Ok((
            checkpoint,
            json!({
                "goal_id": goal_id,
                "task_id": task_id,
                "was_running": was_running,
                "approval_resolved": approval_resolved,
            }),
        ))
    })
    .await
}

async fn mutate_goal<T>(
    host: &GoalsHostState,
    desktop: &DesktopState,
    goal_id: Uuid,
    event_type: &str,
    operation: impl FnOnce(&mut ManagedGoal) -> Result<(T, Value), String>,
) -> Result<(personal_agent_core::AppProjection, T), String> {
    let mut goals = host.goals.lock().await;
    let mut candidate = goals
        .get(&goal_id)
        .cloned()
        .ok_or_else(|| "goal not found".to_owned())?;
    let (result, details) = operation(Arc::make_mut(&mut candidate))?;
    let payload = transition_payload(&candidate, details);
    let projection = persist_managed(host, desktop, &candidate, event_type, payload.clone())?;
    goals.insert(goal_id, candidate);
    drop(goals);
    record_activity(
        host,
        &projection,
        event_type,
        Some(goal_id),
        payload_task_id(&payload),
    )
    .await;
    Ok((projection, result))
}

fn persist_managed(
    host: &GoalsHostState,
    desktop: &DesktopState,
    managed: &ManagedGoal,
    event_type: &str,
    payload: Value,
) -> Result<personal_agent_core::AppProjection, String> {
    let snapshot = managed.supervisor.snapshot().clone();
    let (projection, event) = desktop
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?
        .append_supervisor_event(&snapshot, event_type, &payload)
        .map_err(|error| error.to_string())?;
    if let Err(error) = host.persistence.queue(
        Arc::clone(&desktop.profile),
        snapshot,
        event,
        is_critical_goal_transition(event_type),
    ) {
        // The event is already durable and is the recovery authority. Keep the
        // resident state aligned with it; restart replay remains correct even if
        // advancing the optimization checkpoint must wait for shutdown/retry.
        tracing::error!(%error, %event_type, "goal checkpoint could not be queued or flushed");
    }
    Ok(projection)
}

fn is_critical_goal_transition(event_type: &str) -> bool {
    matches!(
        event_type,
        "approval.requested"
            | "approval.resolved"
            | "task.completed"
            | "task.failed"
            | "task.cancelled"
            | "goal.completed"
            | "goal.cancelled"
    )
}

fn transition_payload(managed: &ManagedGoal, details: Value) -> Value {
    let mut payload = details.as_object().cloned().unwrap_or_default();
    payload.insert("goal".into(), json!(managed.goal));
    payload.insert("goal_id".into(), json!(managed.goal.id));
    Value::Object(payload)
}

fn payload_task_id(payload: &Value) -> Option<Uuid> {
    payload
        .get("task_id")
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
}

async fn record_activity(
    host: &GoalsHostState,
    projection: &personal_agent_core::AppProjection,
    event_type: &str,
    goal_id: Option<Uuid>,
    task_id: Option<Uuid>,
) {
    let mut activities = host.activities.lock().await;
    activities.push(GoalActivityView {
        sequence: projection.last_sequence,
        event_type: event_type.into(),
        goal_id,
        task_id,
        timestamp: Utc::now().to_rfc3339(),
    });
    if activities.len() > 100 {
        activities.remove(0);
    }
}

fn checkpoints(managed: &ManagedGoal) -> Vec<String> {
    managed
        .supervisor
        .snapshot()
        .graph
        .tasks
        .values()
        .filter_map(|task| task.checkpoint_id.clone())
        .collect()
}

fn goal_budget_limits(goal: &Goal) -> personal_agent_core::BudgetLimits {
    let cost_microusd = goal.budgets.cost_usd.and_then(usd_to_microusd);
    personal_agent_core::BudgetLimits {
        tokens: goal.budgets.tokens,
        cost_microusd,
        tool_calls: goal.budgets.tool_calls,
    }
}

fn microusd_to_usd(value: u64) -> Option<f64> {
    let dollars = value / 1_000_000;
    let micros = value % 1_000_000;
    format!("{dollars}.{micros:06}").parse().ok()
}

fn usd_to_microusd(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let fixed = format!("{value:.6}");
    let (dollars, micros) = fixed.split_once('.')?;
    dollars
        .parse::<u64>()
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(micros.parse::<u64>().ok()?)
}

async fn abort_sessions(desktop: &DesktopState, sessions: Vec<String>) {
    for session in sessions {
        let _ = desktop.runtime.lock().await.abort_session(&session).await;
    }
}

pub(crate) fn ensure_resident_executor(app: tauri::AppHandle) {
    let host = app.state::<GoalsHostState>();
    if host.resident_active.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(RESIDENT_TICK_SECONDS));
        loop {
            timer.tick().await;
            if let Err(error) = drain_ready(&app).await {
                tracing::warn!(%error, "resident goal supervisor tick failed");
            }
        }
    });
}

async fn drain_ready(app: &tauri::AppHandle) -> Result<(), String> {
    let host = app.state::<GoalsHostState>();
    let desktop = app.state::<DesktopState>();
    let ready = {
        let goals = host.goals.lock().await;
        goals
            .iter()
            .filter(|(_, managed)| {
                matches!(
                    managed.goal.status,
                    WorkStatus::Queued | WorkStatus::Running
                )
            })
            .flat_map(|(goal_id, managed)| {
                managed
                    .supervisor
                    .ready_tasks()
                    .into_iter()
                    .map(|task_id| (*goal_id, task_id))
            })
            .collect::<Vec<_>>()
    };
    for (goal_id, task_id) in ready {
        let transition = mutate_goal(&host, &desktop, goal_id, "task.started", |managed| {
            managed
                .supervisor
                .start(task_id)
                .map_err(|error| error.to_string())?;
            managed.goal.status = WorkStatus::Running;
            Ok(((), json!({"goal_id": goal_id, "task_id": task_id})))
        })
        .await;
        if transition.is_err() {
            continue;
        }
        emit_snapshot(app).await?;
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = run_task(&app, goal_id, task_id).await {
                tracing::warn!(%goal_id, %task_id, %error, "background goal task failed");
                let _ = fail_task(&app, goal_id, task_id, &error).await;
            }
        });
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Runtime event handling keeps approval and terminal transitions ordered.
async fn run_task(app: &tauri::AppHandle, goal_id: Uuid, task_id: Uuid) -> Result<(), String> {
    let host = app.state::<GoalsHostState>();
    let desktop = app.state::<DesktopState>();
    let (goal, task) = {
        let goals = host.goals.lock().await;
        let managed = goals
            .get(&goal_id)
            .ok_or_else(|| "goal disappeared".to_owned())?;
        let task = managed
            .supervisor
            .snapshot()
            .graph
            .tasks
            .get(&task_id)
            .cloned()
            .ok_or_else(|| "task disappeared".to_owned())?;
        (managed.goal.clone(), task)
    };
    let config = desktop
        .config
        .read()
        .map_err(|_| "configuration lock is poisoned".to_owned())?
        .clone();
    let directory = PathBuf::from(
        task.workspace
            .as_deref()
            .unwrap_or(&config.runtime.working_directory),
    );
    if !directory.is_dir() {
        return Err("goal working directory is unavailable".into());
    }
    let model = (!config.runtime.default_model.trim().is_empty()).then(|| {
        if config.runtime.default_model.contains('/') {
            config.runtime.default_model.clone()
        } else {
            format!(
                "{}/{}",
                config.runtime.default_provider, config.runtime.default_model
            )
        }
    });
    let agent = (!config.runtime.default_agent.trim().is_empty())
        .then(|| config.runtime.default_agent.clone());
    let effort = (!config.runtime.default_effort.trim().is_empty())
        .then(|| config.runtime.default_effort.clone());
    let prompt = format!(
        "Goal:\n{}\n\nCurrent success criterion:\n{}\n\nAll success criteria:\n{}",
        goal.objective,
        task.title,
        goal.success_criteria
            .iter()
            .map(|criterion| format!("- {criterion}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let (session_id, submission, api_client) = {
        let mut runtime = desktop.runtime.lock().await;
        let session_id = runtime
            .begin_session(SessionOptions {
                model: model.clone(),
                effort: effort.clone(),
                agent: agent.clone(),
                working_directory: directory.clone(),
                environment: BTreeMap::new(),
            })
            .await
            .map_err(|error| error.to_string())?;
        let submission = runtime
            .submit_with_attachments(
                &session_id,
                &prompt,
                Vec::new(),
                PromptOptions {
                    model: model.as_deref(),
                    agent: agent.as_deref(),
                    effort: effort.as_deref(),
                    system: Some(BACKGROUND_GOAL_SYSTEM_PROMPT),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let api_client = runtime.api_client().map_err(|error| error.to_string())?;
        (session_id, submission, api_client)
    };
    mutate_goal(&host, &desktop, goal_id, "task.runtime_bound", |managed| {
        managed
            .supervisor
            .bind_checkpoint(task_id, session_id.clone())
            .map_err(|error| error.to_string())?;
        Ok((
            (),
            json!({"goal_id": goal_id, "task_id": task_id, "session_id": session_id}),
        ))
    })
    .await?;
    emit_snapshot(app).await?;

    let prompt_message_id = submission.message_id;
    let mut events = submission.events;
    let deadline_seconds = goal.budgets.wall_time_seconds.unwrap_or(14_400).min(14_400);
    let deadline = tokio::time::sleep(Duration::from_secs(deadline_seconds));
    tokio::pin!(deadline);
    let mut response = String::new();
    let mut success = false;
    let mut failure = None;
    loop {
        let mut event = tokio::select! {
            event = events.recv() => {
                let Some(event) = event else {
                    failure = Some("The goal runtime event stream ended before completion.".to_owned());
                    break;
                };
                event
            },
            () = &mut deadline => {
                let _ = desktop.runtime.lock().await.abort_session(&session_id).await;
                failure = Some("The goal task exceeded its wall-time budget.".into());
                break;
            }
        };
        event.goal_id = Some(goal_id.to_string());
        event.task_id = Some(task_id.to_string());
        if !matches!(
            event.r#type.as_str(),
            "approval.requested" | "approval.resolved"
        ) {
            let budget = {
                let mut profile = desktop
                    .profile
                    .lock()
                    .map_err(|_| "profile state lock is poisoned".to_owned())?;
                profile
                    .record_runtime_event(event.clone())
                    .map_err(|error| error.to_string())?;
                profile
                    .usage_snapshot()
                    .map_err(|error| error.to_string())?
                    .check_budget(&format!("goal:{goal_id}"), goal_budget_limits(&goal))
            };
            if budget.exceeded() {
                let _ = desktop
                    .runtime
                    .lock()
                    .await
                    .abort_session(&session_id)
                    .await;
                failure = Some(format!("Goal stopped because its {budget}."));
                break;
            }
        }
        let _ = app.emit("goal-runtime-event", &event);
        if event.r#type == "response.delta"
            && let Ok(payload) = event.payload()
            && let Some(delta) = payload
                .get("delta")
                .or_else(|| payload.get("text"))
                .and_then(Value::as_str)
        {
            response.push_str(delta);
        }
        if event.r#type == "approval.requested" {
            let payload = event.payload().map_err(|error| error.to_string())?;
            let request_id = payload
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| "runtime approval omitted its request ID".to_owned())?;
            let reason = payload
                .get("permission")
                .or_else(|| payload.get("action"))
                .and_then(Value::as_str)
                .unwrap_or("Consequential background action")
                .to_owned();
            let approval = PendingApproval {
                goal_id,
                task_id,
                session_id: session_id.clone(),
                request_id: request_id.into(),
                reason: reason.clone(),
                requested_at: Utc::now(),
            };
            mutate_goal(&host, &desktop, goal_id, "approval.requested", |managed| {
                managed
                    .supervisor
                    .wait_for_approval(task_id, reason.clone())
                    .map_err(|error| error.to_string())?;
                managed.approvals.insert(task_id, approval.clone());
                Ok(((), json!(approval)))
            })
            .await?;
            emit_snapshot(app).await?;
            continue;
        }
        if event.r#type == "clarification.requested" {
            failure = Some(
                "Background clarification needs an interactive session; the task was stopped."
                    .into(),
            );
            let _ = desktop
                .runtime
                .lock()
                .await
                .abort_session(&session_id)
                .await;
            break;
        }
        if event.r#type == "response.failed" {
            failure = event.payload().ok().and_then(|payload| {
                payload
                    .pointer("/error/data/message")
                    .or_else(|| payload.pointer("/error/message"))
                    .or_else(|| payload.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            break;
        }
        if matches!(
            event.r#type.as_str(),
            "runtime.stream_error" | "runtime.stream_closed"
        ) {
            failure = Some("The goal runtime stream disconnected.".into());
            break;
        }
        if event.r#type == "response.completed" {
            success = true;
            break;
        }
    }
    if success
        && let Ok(messages) = api_client
            .request_json(
                reqwest::Method::GET,
                &format!("/session/{session_id}/message"),
                &[("directory", directory.display().to_string())],
                None,
            )
            .await
        && let Some(text) = assistant_text(&messages, &prompt_message_id)
    {
        response = text;
    }
    if success {
        complete_task(app, goal_id, task_id, response).await
    } else {
        Err(failure.unwrap_or_else(|| "Goal task failed without a diagnostic.".into()))
    }
}

async fn complete_task(
    app: &tauri::AppHandle,
    goal_id: Uuid,
    task_id: Uuid,
    response: String,
) -> Result<(), String> {
    let host = app.state::<GoalsHostState>();
    let desktop = app.state::<DesktopState>();
    let (projection, goal_completed) =
        mutate_goal(&host, &desktop, goal_id, "task.completed", |managed| {
            managed
                .supervisor
                .complete(task_id, json!({"summary": response}), true, None)
                .map_err(|error| error.to_string())?;
            let completed = managed
                .supervisor
                .snapshot()
                .graph
                .tasks
                .values()
                .all(|task| task.status == WorkStatus::Completed);
            Ok((completed, json!({"goal_id": goal_id, "task_id": task_id})))
        })
        .await?;
    let _ = projection;
    if goal_completed {
        mutate_goal(&host, &desktop, goal_id, "goal.completed", |managed| {
            let verified = managed
                .goal
                .success_criteria
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            let results = managed
                .supervisor
                .snapshot()
                .graph
                .tasks
                .values()
                .filter_map(|task| {
                    task.output
                        .clone()
                        .map(|output| (task.title.clone(), output))
                })
                .collect::<BTreeMap<_, _>>();
            managed
                .goal
                .complete(&verified, json!({"tasks": results}))
                .map_err(|error| error.to_string())?;
            Ok(((), json!({"goal_id": goal_id})))
        })
        .await?;
    }
    emit_snapshot(app).await?;
    // The resident tick schedules the next dependency. Avoid recursive worker futures so every
    // spawned task remains `Send` across platforms.
    Ok(())
}

async fn fail_task(
    app: &tauri::AppHandle,
    goal_id: Uuid,
    task_id: Uuid,
    error: &str,
) -> Result<(), String> {
    let host = app.state::<GoalsHostState>();
    let desktop = app.state::<DesktopState>();
    let status = {
        let goals = host.goals.lock().await;
        goals
            .get(&goal_id)
            .and_then(|managed| managed.supervisor.snapshot().graph.tasks.get(&task_id))
            .map(|task| task.status)
    };
    if !matches!(status, Some(WorkStatus::Running | WorkStatus::Waiting)) {
        return Ok(());
    }
    mutate_goal(&host, &desktop, goal_id, "task.failed", |managed| {
        let was_running = managed
            .supervisor
            .snapshot()
            .graph
            .tasks
            .get(&task_id)
            .is_some_and(|task| task.status == WorkStatus::Running);
        let approval_resolved = managed.approvals.contains_key(&task_id);
        managed
            .supervisor
            .fail(task_id, json!({"error": error}))
            .map_err(|failure| failure.to_string())?;
        managed.goal.status = WorkStatus::Failed;
        managed.approvals.remove(&task_id);
        Ok((
            (),
            json!({
                "goal_id": goal_id,
                "task_id": task_id,
                "error": error,
                "was_running": was_running,
                "approval_resolved": approval_resolved,
            }),
        ))
    })
    .await?;
    emit_snapshot(app).await
}

fn assistant_text(messages: &Value, prompt_message_id: &str) -> Option<String> {
    messages.as_array()?.iter().rev().find_map(|message| {
        if message.pointer("/info/role").and_then(Value::as_str) != Some("assistant")
            || message.pointer("/info/parentID").and_then(Value::as_str) != Some(prompt_message_id)
        {
            return None;
        }
        let text = message
            .get("parts")?
            .as_array()?
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<String>();
        (!text.trim().is_empty()).then_some(text)
    })
}

async fn snapshot_view(host: &GoalsHostState) -> Result<GoalsSnapshotView, String> {
    let goals = host.goals.lock().await;
    let mut views = goals
        .values()
        .map(|managed| {
            let snapshot = managed.supervisor.snapshot();
            let mut tasks = snapshot.graph.tasks.values().cloned().collect::<Vec<_>>();
            tasks.sort_by_key(|task| task.id);
            GoalView {
                goal: managed.goal.clone(),
                tasks,
                edges: snapshot.graph.edges.clone(),
                approvals: managed.approvals.values().cloned().collect(),
            }
        })
        .collect::<Vec<_>>();
    views.sort_by_key(|view| {
        (
            std::cmp::Reverse(view.goal.priority),
            std::cmp::Reverse(view.goal.created_at),
        )
    });
    drop(goals);
    Ok(GoalsSnapshotView {
        goals: views,
        activities: host.activities.lock().await.clone(),
        resident_active: host.resident_active.load(Ordering::SeqCst),
        recovered_tasks: host.recovered_tasks,
        maximum_parallelism: MAXIMUM_PARALLELISM,
    })
}

async fn emit_snapshot(app: &tauri::AppHandle) -> Result<(), String> {
    let snapshot = snapshot_view(&app.state::<GoalsHostState>()).await?;
    app.emit("goals-supervisor://changed", snapshot)
        .map_err(|error| error.to_string())
}

fn bounded_text(value: &str, field: &str, maximum: usize) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{field} is required"));
    }
    if value.len() > maximum {
        return Err(format!("{field} exceeds {maximum} bytes"));
    }
    Ok(value.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_agent_platform::{SecretReference, SecretStore, SecretStoreError};
    use secrecy::SecretString;

    struct TestSecrets;

    impl SecretStore for TestSecrets {
        fn put(
            &self,
            _reference: &SecretReference,
            _value: &SecretString,
        ) -> Result<(), SecretStoreError> {
            Ok(())
        }

        fn get(&self, _reference: &SecretReference) -> Result<SecretString, SecretStoreError> {
            Ok(SecretString::from("goal-host-test-key".to_owned()))
        }

        fn delete(&self, _reference: &SecretReference) -> Result<(), SecretStoreError> {
            Ok(())
        }
    }

    fn test_profile(
        directory: &tempfile::TempDir,
        name: &str,
    ) -> Arc<Mutex<personal_agent_core::ProfileState>> {
        Arc::new(Mutex::new(
            personal_agent_core::ProfileState::open(
                &directory.path().join(name),
                "default",
                &TestSecrets,
            )
            .expect("profile"),
        ))
    }

    #[test]
    fn task_graph_is_sequential_and_bounded() {
        let mut goal = Goal::new(
            "Ship a feature",
            vec!["implementation exists".into(), "tests pass".into()],
            "test",
        );
        goal.plan_revision = 1;
        let graph = task_graph(&goal, "/workspace");
        graph.validate().expect("valid graph");
        assert_eq!(graph.tasks.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert!(graph.tasks.values().all(|task| task.max_attempts == 3));
    }

    #[test]
    fn event_filter_excludes_unrelated_runtime_and_memory_events() {
        assert!(is_goal_event("goal.created"));
        assert!(is_goal_event("task.completed"));
        assert!(is_goal_event("approval.requested"));
        assert!(!is_goal_event("runtime.health"));
        assert!(!is_goal_event("memory.created"));
    }

    #[test]
    fn goal_text_validation_rejects_empty_and_oversized_values() {
        assert!(bounded_text("  ", "goal", 10).is_err());
        assert!(bounded_text("eleven bytes", "goal", 5).is_err());
        assert_eq!(bounded_text(" goal ", "goal", 10).unwrap(), "goal");
    }

    #[test]
    fn goal_recovery_is_identical_with_and_without_supervisor_checkpoints() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("goal-replay-checkpoint.db");
        let mut profile =
            personal_agent_core::ProfileState::open(&database, "default", &TestSecrets)
                .expect("profile");

        let mut first = Goal::new("Recover efficiently", vec!["state matches".into()], "test");
        first.plan_revision = 1;
        let mut first_supervisor = DurableSupervisor::new(
            task_graph(&first, temp.path().to_str().unwrap()),
            MAXIMUM_PARALLELISM,
            MAXIMUM_DELEGATION_DEPTH,
        )
        .expect("first supervisor");
        profile
            .record_supervisor_event(first_supervisor.snapshot(), "goal.created", &json!(first))
            .expect("create first goal");
        let task_id = first_supervisor.ready_tasks()[0];
        first_supervisor.start(task_id).expect("start task");
        first.status = WorkStatus::Running;
        profile
            .record_supervisor_event(
                first_supervisor.snapshot(),
                "task.started",
                &json!({"goal": first, "goal_id": first.id, "task_id": task_id}),
            )
            .expect("start event");
        first_supervisor
            .wait_for_approval(task_id, "confirm test effect")
            .expect("wait for approval");
        let approval = PendingApproval {
            goal_id: first.id,
            task_id,
            session_id: "test-session".into(),
            request_id: "test-request".into(),
            reason: "confirm test effect".into(),
            requested_at: Utc::now(),
        };
        let mut approval_payload = serde_json::to_value(&approval).expect("approval payload");
        approval_payload
            .as_object_mut()
            .expect("approval object")
            .insert("goal".into(), json!(first));
        profile
            .record_supervisor_event(
                first_supervisor.snapshot(),
                "approval.requested",
                &approval_payload,
            )
            .expect("approval event");

        let mut second = Goal::new("Preserve ordering", vec!["timeline matches".into()], "test");
        second.plan_revision = 1;
        let second_supervisor = DurableSupervisor::new(
            task_graph(&second, temp.path().to_str().unwrap()),
            MAXIMUM_PARALLELISM,
            MAXIMUM_DELEGATION_DEPTH,
        )
        .expect("second supervisor");
        profile
            .record_supervisor_event(second_supervisor.snapshot(), "goal.created", &json!(second))
            .expect("create second goal");

        let full = replay_goal_events_with_checkpoints(&profile, false).expect("full replay");
        let checkpointed =
            replay_goal_events_with_checkpoints(&profile, true).expect("checkpoint replay");
        assert_eq!(checkpointed, full);
        assert_eq!(checkpointed.0.len(), 2);
        assert_eq!(checkpointed.1[&first.id][&task_id], approval);
        assert_eq!(checkpointed.2.len(), 4);
    }

    #[test]
    fn newer_other_goal_checkpoint_cannot_hide_an_unflushed_goal_tail() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("goal-interleaved-checkpoints.db");
        let mut profile =
            personal_agent_core::ProfileState::open(&database, "default", &TestSecrets)
                .expect("profile");

        let mut first = Goal::new(
            "Recover the unflushed tail",
            vec!["running state survives".into()],
            "test",
        );
        first.plan_revision = 1;
        let mut first_supervisor = DurableSupervisor::new(
            task_graph(&first, temp.path().to_str().unwrap()),
            MAXIMUM_PARALLELISM,
            MAXIMUM_DELEGATION_DEPTH,
        )
        .expect("first supervisor");
        let first_created = profile
            .append_supervisor_event(first_supervisor.snapshot(), "goal.created", &json!(first))
            .expect("append first goal")
            .1;
        profile
            .save_supervisor_checkpoint_updates(&[
                personal_agent_core::SupervisorCheckpointUpdate {
                    snapshot: first_supervisor.snapshot().clone(),
                    events: vec![first_created],
                },
            ])
            .expect("checkpoint first goal");

        let first_task = first_supervisor.ready_tasks()[0];
        first_supervisor
            .start(first_task)
            .expect("start first task");
        first.status = WorkStatus::Running;
        let first_tail = profile
            .append_supervisor_event(
                first_supervisor.snapshot(),
                "task.started",
                &json!({
                    "goal": first,
                    "goal_id": first.id,
                    "task_id": first_task,
                }),
            )
            .expect("append uncheckpointed first-goal tail")
            .1;

        let mut second = Goal::new(
            "Flush a newer checkpoint",
            vec!["checkpoint is newer".into()],
            "test",
        );
        second.plan_revision = 1;
        let second_supervisor = DurableSupervisor::new(
            task_graph(&second, temp.path().to_str().unwrap()),
            MAXIMUM_PARALLELISM,
            MAXIMUM_DELEGATION_DEPTH,
        )
        .expect("second supervisor");
        let second_created = profile
            .append_supervisor_event(second_supervisor.snapshot(), "goal.created", &json!(second))
            .expect("append second goal")
            .1;
        assert!(second_created.monotonic_sequence > first_tail.monotonic_sequence);
        profile
            .save_supervisor_checkpoint_updates(&[
                personal_agent_core::SupervisorCheckpointUpdate {
                    snapshot: second_supervisor.snapshot().clone(),
                    events: vec![second_created],
                },
            ])
            .expect("checkpoint newer second goal");

        let full = replay_goal_events_with_checkpoints(&profile, false).expect("full replay");
        let checkpointed =
            replay_goal_events_with_checkpoints(&profile, true).expect("checkpoint replay");
        assert_eq!(checkpointed, full);
        assert_eq!(checkpointed.0[&first.id].status, WorkStatus::Running);
        assert_eq!(checkpointed.2.len(), 3);
        assert_eq!(
            checkpointed
                .2
                .iter()
                .map(|activity| activity.sequence)
                .collect::<BTreeSet<_>>()
                .len(),
            3,
            "events already represented by newer checkpoints remain deduplicated"
        );
    }

    #[test]
    fn mutation_burst_writes_one_complete_goal_checkpoint_and_critical_flushes() {
        let temp = tempfile::tempdir().expect("temp");
        let profile = test_profile(&temp, "goal-debounce.db");
        let mut goal = Goal::new("Coalesce safely", vec!["all events survive".into()], "test");
        goal.plan_revision = 1;
        let supervisor = DurableSupervisor::new(
            task_graph(&goal, temp.path().to_str().unwrap()),
            MAXIMUM_PARALLELISM,
            MAXIMUM_DELEGATION_DEPTH,
        )
        .expect("supervisor");
        let task_id = supervisor.ready_tasks()[0];
        let persistence = Arc::new(GoalPersistence::default());
        for index in 0..10 {
            let event_type = if index == 0 {
                "goal.created"
            } else if index == 9 {
                "approval.requested"
            } else {
                "goal.updated"
            };
            let payload = json!({
                "goal": goal,
                "goal_id": goal.id,
                "task_id": task_id,
                "index": index,
            });
            let event = profile
                .lock()
                .expect("profile")
                .append_supervisor_event(supervisor.snapshot(), event_type, &payload)
                .expect("append event")
                .1;
            persistence
                .queue(
                    Arc::clone(&profile),
                    supervisor.snapshot().clone(),
                    event,
                    index == 9,
                )
                .expect("queue checkpoint");
        }
        assert_eq!(
            persistence.successful_writes.load(Ordering::SeqCst),
            1,
            "the approval transition flushes the entire burst once"
        );
        let checkpoints = profile
            .lock()
            .expect("profile")
            .supervisor_recovery_checkpoints()
            .expect("checkpoints");
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].last_sequence, 10);
        assert_eq!(checkpoints[0].recent_activities.len(), 10);
        assert_eq!(checkpoints[0].pending_approval_events.len(), 1);
        assert!(checkpoints[0].replay_base_complete);
    }

    #[test]
    fn pending_goal_checkpoint_flushes_synchronously_for_shutdown() {
        let temp = tempfile::tempdir().expect("temp");
        let profile = test_profile(&temp, "goal-shutdown.db");
        let mut goal = Goal::new("Flush safely", vec!["checkpoint exists".into()], "test");
        goal.plan_revision = 1;
        let supervisor = DurableSupervisor::new(
            task_graph(&goal, temp.path().to_str().unwrap()),
            MAXIMUM_PARALLELISM,
            MAXIMUM_DELEGATION_DEPTH,
        )
        .expect("supervisor");
        let event = profile
            .lock()
            .expect("profile")
            .append_supervisor_event(supervisor.snapshot(), "goal.created", &json!(goal))
            .expect("append event")
            .1;
        let persistence = Arc::new(GoalPersistence::default());
        persistence
            .queue(
                Arc::clone(&profile),
                supervisor.snapshot().clone(),
                event,
                false,
            )
            .expect("queue checkpoint");
        persistence.flush(&profile).expect("shutdown flush");
        assert_eq!(persistence.successful_writes.load(Ordering::SeqCst), 1);
        assert_eq!(
            profile
                .lock()
                .expect("profile")
                .supervisor_recovery_checkpoints()
                .expect("checkpoints")
                .len(),
            1
        );
    }

    #[test]
    fn critical_goal_flush_waits_for_an_inflight_checkpoint_writer() {
        let temp = tempfile::tempdir().expect("temp");
        let profile = test_profile(&temp, "goal-write-gate.db");
        let mut goal = Goal::new(
            "Serialize checkpoint writes",
            vec!["tail retained".into()],
            "test",
        );
        goal.plan_revision = 1;
        let supervisor = DurableSupervisor::new(
            task_graph(&goal, temp.path().to_str().unwrap()),
            MAXIMUM_PARALLELISM,
            MAXIMUM_DELEGATION_DEPTH,
        )
        .expect("supervisor");
        let event = profile
            .lock()
            .expect("profile")
            .append_supervisor_event(supervisor.snapshot(), "goal.created", &json!(goal))
            .expect("append event")
            .1;
        let persistence = Arc::new(GoalPersistence::default());
        let write_guard = persistence.write_gate.lock().expect("write gate");
        let writer = {
            let persistence = Arc::clone(&persistence);
            let profile = Arc::clone(&profile);
            let snapshot = supervisor.snapshot().clone();
            std::thread::spawn(move || persistence.queue(profile, snapshot, event, true))
        };

        for _ in 0..1_000 {
            if persistence
                .pending
                .lock()
                .expect("pending checkpoints")
                .contains_key(&goal.id)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            persistence
                .pending
                .lock()
                .expect("pending checkpoints")
                .contains_key(&goal.id),
            "critical batch reached the write gate"
        );
        assert!(
            !writer.is_finished(),
            "critical flush overtook the writer gate"
        );
        assert_eq!(persistence.successful_writes.load(Ordering::SeqCst), 0);

        drop(write_guard);
        writer
            .join()
            .expect("critical writer thread")
            .expect("critical checkpoint flush");
        assert_eq!(persistence.successful_writes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn approvals_and_terminal_transitions_are_critical() {
        for event_type in [
            "approval.requested",
            "approval.resolved",
            "task.completed",
            "task.failed",
            "task.cancelled",
            "goal.completed",
            "goal.cancelled",
        ] {
            assert!(is_critical_goal_transition(event_type), "{event_type}");
        }
        assert!(!is_critical_goal_transition("task.started"));
    }

    #[tokio::test]
    async fn restart_recovery_requeues_retry_safe_running_tasks() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("goal-profile.db");
        let goal_id;
        let task_id;
        {
            let mut profile =
                personal_agent_core::ProfileState::open(&database, "default", &TestSecrets)
                    .expect("profile");
            let mut goal = Goal::new("Recover safely", vec!["tests pass".into()], "test");
            goal.plan_revision = 1;
            goal.status = WorkStatus::Running;
            goal_id = goal.id;
            let mut supervisor = DurableSupervisor::new(
                task_graph(&goal, temp.path().to_str().unwrap()),
                MAXIMUM_PARALLELISM,
                MAXIMUM_DELEGATION_DEPTH,
            )
            .expect("supervisor");
            task_id = supervisor.ready_tasks()[0];
            supervisor.start(task_id).expect("start");
            profile
                .record_supervisor_event(supervisor.snapshot(), "goal.created", &json!(goal))
                .expect("persist transition");
        }
        let mut reopened =
            personal_agent_core::ProfileState::open(&database, "default", &TestSecrets)
                .expect("reopen");
        let host = GoalsHostState::load(&mut reopened, temp.path().to_str().unwrap())
            .expect("recover host");
        let goals = host.goals.lock().await;
        let recovered = goals.get(&goal_id).expect("goal recovered");
        assert_eq!(
            recovered.supervisor.snapshot().graph.tasks[&task_id].status,
            WorkStatus::Queued
        );
        assert_eq!(host.recovered_tasks, 1);
        assert!(recovered.supervisor.snapshot().running_order.is_empty());
    }
}
