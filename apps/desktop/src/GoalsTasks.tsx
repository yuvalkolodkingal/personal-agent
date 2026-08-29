import { useEffect, useMemo, useState, type FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Projection } from "./types";
import "./goals-tasks.css";

type WorkStatus =
  | "queued"
  | "planning"
  | "running"
  | "waiting"
  | "paused"
  | "completed"
  | "failed"
  | "cancelled";

type Goal = {
  id: string;
  objective: string;
  success_criteria: string[];
  created_at: string;
  priority: number;
  status: WorkStatus;
  plan_revision: number;
  final_result?: unknown;
};

type Task = {
  id: string;
  title: string;
  status: WorkStatus;
  progress: number;
  attempt: number;
  max_attempts: number;
  assigned_agent: string;
  execution_zone: string;
  checkpoint_id?: string | null;
  output?: unknown;
};

type Approval = {
  goal_id: string;
  task_id: string;
  reason: string;
  requested_at: string;
};

type GoalView = {
  goal: Goal;
  tasks: Task[];
  edges: Array<[string, string]>;
  approvals: Approval[];
};

type Activity = {
  sequence: number;
  event_type: string;
  goal_id?: string | null;
  task_id?: string | null;
  timestamp: string;
};

export type GoalsSnapshot = {
  goals: GoalView[];
  activities: Activity[];
  resident_active: boolean;
  recovered_tasks: number;
  maximum_parallelism: number;
};

type GoalActionResult = {
  snapshot: GoalsSnapshot;
  projection: Projection;
  message: string;
};

const emptySnapshot: GoalsSnapshot = {
  goals: [],
  activities: [],
  resident_active: false,
  recovered_tasks: 0,
  maximum_parallelism: 0,
};

export function GoalsTasks({
  onProjection,
}: {
  onProjection?: (projection: Projection) => void;
}) {
  const [snapshot, setSnapshot] = useState(emptySnapshot);
  const [objective, setObjective] = useState("");
  const [criteria, setCriteria] = useState("");
  const [priority, setPriority] = useState(0);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");

  useEffect(() => {
    let alive = true;
    void invoke<GoalsSnapshot>("goals_snapshot")
      .then((value) => alive && setSnapshot(value))
      .catch((caught) => alive && setError(String(caught)));
    const unlisten = listen<GoalsSnapshot>(
      "goals-supervisor://changed",
      ({ payload }) => alive && setSnapshot(payload),
    );
    return () => {
      alive = false;
      void unlisten.then((dispose) => dispose());
    };
  }, []);

  const counts = useMemo(
    () => ({
      active: snapshot.goals.filter(({ goal }) =>
        ["queued", "planning", "running", "waiting"].includes(goal.status),
      ).length,
      running: snapshot.goals.flatMap(({ tasks }) => tasks).filter(
        (task) => task.status === "running",
      ).length,
      approvals: snapshot.goals.flatMap(({ approvals }) => approvals).length,
    }),
    [snapshot],
  );

  const execute = async (action: Record<string, unknown>, key: string): Promise<boolean> => {
    setBusy(key);
    setError("");
    setMessage("");
    try {
      const result = await invoke<GoalActionResult>("goals_execute", { action });
      setSnapshot(result.snapshot);
      setMessage(result.message);
      onProjection?.(result.projection);
      return true;
    } catch (caught) {
      setError(String(caught));
      return false;
    } finally {
      setBusy("");
    }
  };

  const create = async (event: FormEvent) => {
    event.preventDefault();
    const successCriteria = criteria
      .split("\n")
      .map((criterion) => criterion.trim())
      .filter(Boolean);
    const created = await execute(
      {
        type: "create",
        objective,
        success_criteria: successCriteria,
        priority,
      },
      "create",
    );
    if (created) {
      setObjective("");
      setCriteria("");
      setPriority(0);
    }
  };

  return (
    <section className="goals-workspace">
      <header className="goals-hero">
        <div>
          <span className="eyebrow">BACKGROUND SUPERVISOR</span>
          <h2>Goals &amp; tasks</h2>
          <p>
            Durable task graphs survive restart, pause for approvals, and retry only
            through the native supervisor.
          </p>
        </div>
        <dl>
          <div><dt>{counts.active}</dt><dd>active goals</dd></div>
          <div><dt>{counts.running}</dt><dd>running tasks</dd></div>
          <div><dt>{counts.approvals}</dt><dd>approvals</dd></div>
        </dl>
      </header>

      {snapshot.recovered_tasks > 0 && (
        <div className="goal-recovery" role="status">
          Recovered {snapshot.recovered_tasks} interrupted task
          {snapshot.recovered_tasks === 1 ? "" : "s"} without replaying an unsafe effect.
        </div>
      )}
      {error && <p className="error-banner" role="alert">{error}</p>}
      {message && <p className="goal-message" role="status">{message}</p>}

      <div className="goals-layout">
        <main>
          <div className="goals-section-title">
            <div><span className="eyebrow">DURABLE QUEUE</span><b>{snapshot.goals.length} goals</b></div>
            <small>
              {snapshot.resident_active ? "Supervisor online" : "Waiting for runtime"}
              {snapshot.maximum_parallelism > 0
                ? ` · up to ${snapshot.maximum_parallelism} tasks per goal`
                : ""}
            </small>
          </div>

          {snapshot.goals.length === 0 ? (
            <div className="goals-empty">
              <strong>No durable goals yet</strong>
              <p>Create an objective with checks that can be observed and verified.</p>
            </div>
          ) : (
            <div className="goal-list">
              {snapshot.goals.map((view) => (
                <GoalCard
                  key={view.goal.id}
                  view={view}
                  expanded={expanded === view.goal.id}
                  busy={busy}
                  onToggle={() =>
                    setExpanded((current) =>
                      current === view.goal.id ? null : view.goal.id,
                    )
                  }
                  onAction={(action, key) => void execute(action, key)}
                />
              ))}
            </div>
          )}
        </main>

        <aside>
          <form className="goal-create" onSubmit={(event) => void create(event)}>
            <span className="eyebrow">NEW GOAL</span>
            <label>
              Objective
              <textarea
                aria-label="Goal objective"
                value={objective}
                onChange={(event) => setObjective(event.target.value)}
                placeholder="Ship the offline voice workflow"
                required
              />
            </label>
            <label>
              Observable success criteria
              <textarea
                aria-label="Goal success criteria"
                value={criteria}
                onChange={(event) => setCriteria(event.target.value)}
                placeholder={"Implementation is complete\nFocused tests pass"}
                required
              />
              <small>One criterion per line. Each becomes a durable task.</small>
            </label>
            <label>
              Priority
              <input
                aria-label="Goal priority"
                type="number"
                min={-100}
                max={100}
                value={priority}
                onChange={(event) => setPriority(Number(event.target.value))}
              />
            </label>
            <button
              className="primary"
              disabled={busy === "create" || !objective.trim() || !criteria.trim()}
            >
              {busy === "create" ? "Creating…" : "Create & run"}
            </button>
          </form>

          <section className="goal-activity">
            <header><span className="eyebrow">EVENTS</span><b>{snapshot.activities.length}</b></header>
            {snapshot.activities.length === 0 ? (
              <small>No supervisor events yet.</small>
            ) : (
              <ol>
                {[...snapshot.activities].reverse().slice(0, 20).map((activity) => (
                  <li key={`${activity.sequence}-${activity.event_type}`}>
                    <i>{activity.sequence}</i>
                    <div>
                      <strong>{activity.event_type.replaceAll("_", " ")}</strong>
                      <small>{new Date(activity.timestamp).toLocaleString()}</small>
                    </div>
                  </li>
                ))}
              </ol>
            )}
          </section>
        </aside>
      </div>
    </section>
  );
}

function GoalCard({
  view,
  expanded,
  busy,
  onToggle,
  onAction,
}: {
  view: GoalView;
  expanded: boolean;
  busy: string;
  onToggle: () => void;
  onAction: (action: Record<string, unknown>, key: string) => void;
}) {
  const { goal, tasks, approvals } = view;
  const completed = tasks.filter((task) => task.status === "completed").length;
  const progress = tasks.length ? Math.round((completed / tasks.length) * 100) : 0;
  const goalAction = (type: string) =>
    onAction({ type, goal_id: goal.id }, `${type}:${goal.id}`);

  return (
    <article className={`goal-card status-${goal.status}`}>
      <header>
        <button className="goal-expand" type="button" onClick={onToggle} aria-expanded={expanded}>
          <span className={`goal-status ${goal.status}`}>{goal.status}</span>
          <strong>{goal.objective}</strong>
          <small>{completed}/{tasks.length} tasks · priority {goal.priority}</small>
        </button>
        <div className="goal-controls">
          {["queued", "planning", "running"].includes(goal.status) && (
            <button disabled={Boolean(busy)} onClick={() => goalAction("pause_goal")}>Pause</button>
          )}
          {goal.status === "paused" && (
            <button disabled={Boolean(busy)} onClick={() => goalAction("resume_goal")}>Resume</button>
          )}
          {["failed", "waiting"].includes(goal.status) && (
            <button disabled={Boolean(busy)} onClick={() => goalAction("retry_goal")}>Retry</button>
          )}
          {!['completed', 'cancelled'].includes(goal.status) && (
            <button className="danger" disabled={Boolean(busy)} onClick={() => goalAction("cancel_goal")}>Cancel</button>
          )}
        </div>
      </header>
      <div className="goal-progress"><i style={{ width: `${progress}%` }} /></div>

      {approvals.map((approval) => (
        <div className="goal-approval" key={approval.task_id}>
          <div><b>Approval required</b><span>{approval.reason}</span></div>
          <button
            disabled={Boolean(busy)}
            onClick={() => onAction(
              { type: "answer_approval", goal_id: goal.id, task_id: approval.task_id, allow: false },
              `reject:${approval.task_id}`,
            )}
          >Reject</button>
          <button
            className="primary"
            disabled={Boolean(busy)}
            onClick={() => onAction(
              { type: "answer_approval", goal_id: goal.id, task_id: approval.task_id, allow: true },
              `allow:${approval.task_id}`,
            )}
          >Allow once</button>
        </div>
      ))}

      {expanded && (
        <div className="task-list">
          {tasks.map((task, index) => (
            <TaskRow
              key={task.id}
              goalId={goal.id}
              task={task}
              index={index}
              approval={approvals.some((approval) => approval.task_id === task.id)}
              busy={busy}
              onAction={onAction}
            />
          ))}
        </div>
      )}
    </article>
  );
}

function TaskRow({
  goalId,
  task,
  index,
  approval,
  busy,
  onAction,
}: {
  goalId: string;
  task: Task;
  index: number;
  approval: boolean;
  busy: string;
  onAction: (action: Record<string, unknown>, key: string) => void;
}) {
  const act = (type: string) =>
    onAction(
      { type, goal_id: goalId, task_id: task.id },
      `${type}:${task.id}`,
    );
  return (
    <article className="task-row">
      <i>{String(index + 1).padStart(2, "0")}</i>
      <div>
        <header><strong>{task.title}</strong><span className={`task-status ${task.status}`}>{task.status}</span></header>
        <small>
          {task.assigned_agent} · {task.execution_zone} · attempt {task.attempt}/{task.max_attempts}
          {approval ? " · approval pending" : ""}
        </small>
        {task.output != null && <pre>{JSON.stringify(task.output, null, 2)}</pre>}
      </div>
      <footer>
        {["queued", "planning", "running"].includes(task.status) && (
          <button disabled={Boolean(busy)} onClick={() => act("pause_task")}>Pause</button>
        )}
        {task.status === "paused" && (
          <button disabled={Boolean(busy)} onClick={() => act("resume_task")}>Resume</button>
        )}
        {["failed", "waiting"].includes(task.status) && !approval && task.attempt < task.max_attempts && (
          <button disabled={Boolean(busy)} onClick={() => act("retry_task")}>Retry</button>
        )}
        {!['completed', 'cancelled'].includes(task.status) && (
          <button className="danger" disabled={Boolean(busy)} onClick={() => act("cancel_task")}>Cancel</button>
        )}
      </footer>
    </article>
  );
}
