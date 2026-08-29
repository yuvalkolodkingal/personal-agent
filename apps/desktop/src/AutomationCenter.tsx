import { useEffect, useMemo, useState, type FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./automation-center.css";

type Trigger = {
  kind: string;
  at?: string;
  seconds?: number;
  expression?: string;
  [key: string]: unknown;
};

export type DurableAutomation = {
  id: string;
  name: string;
  goal_template: string;
  trigger: Trigger;
  enabled: boolean;
  max_concurrency: number;
  missed_run_policy: "skip" | "run_once" | "catch_up_bounded";
  consecutive_failures: number;
  pause_after_failures: number;
  next_due_at?: string | null;
  maximum_catch_up_runs: number;
  quiet_hours_utc?: [number, number] | null;
  notification_route: string;
};

export type AutomationRun = {
  id: string;
  automation_id: string;
  schedule_key: string;
  scheduled_for: string;
  status:
    | "queued"
    | "running"
    | "waiting_approval"
    | "paused_for_user"
    | "completed"
    | "failed"
    | "skipped";
  attempt: number;
  approval_reason?: string | null;
  started_at?: string | null;
  finished_at?: string | null;
  result_summary?: string | null;
};

export type AutomationSnapshot = {
  automations: DurableAutomation[];
  runs: AutomationRun[];
  resident_active: boolean;
  global_enabled: boolean;
  recovered_runs: number;
  supported_schedules: string[];
  unsupported_triggers: string[];
  notification: {
    enabled: boolean;
    native_delivery: boolean;
    desktop_actions: boolean;
    action_guidance: string;
    quiet_hours_utc?: [number, number] | null;
    last_error?: string | null;
  };
};

type Action =
  | { type: "refresh" }
  | {
      type: "create";
      name: string;
      prompt: string;
      schedule: string;
      missed_run_policy: "skip" | "run_once" | "catch_up_bounded";
      max_concurrency: number;
      pause_after_failures: number;
      maximum_catch_up_runs: number;
      notification_route: "desktop" | "none";
    }
  | { type: "set_enabled"; automation_id: string; enabled: boolean }
  | { type: "run_now"; automation_id: string }
  | { type: "delete"; automation_id: string; confirmed: boolean }
  | { type: "answer_approval"; schedule_key: string; allow: boolean };

type ActionResult = { snapshot?: AutomationSnapshot; message?: string };

const empty: AutomationSnapshot = {
  automations: [],
  runs: [],
  resident_active: false,
  global_enabled: true,
  recovered_runs: 0,
  supported_schedules: [],
  unsupported_triggers: [],
  notification: {
    enabled: false,
    native_delivery: false,
    desktop_actions: false,
    action_guidance: "Notification capability has not loaded.",
  },
};

function normalized(value: Partial<AutomationSnapshot> | null | undefined): AutomationSnapshot {
  return {
    ...empty,
    ...value,
    automations: Array.isArray(value?.automations) ? value.automations : [],
    runs: Array.isArray(value?.runs) ? value.runs : [],
    supported_schedules: Array.isArray(value?.supported_schedules) ? value.supported_schedules : [],
    unsupported_triggers: Array.isArray(value?.unsupported_triggers) ? value.unsupported_triggers : [],
    notification: { ...empty.notification, ...(value?.notification ?? {}) },
  };
}

function triggerLabel(trigger: Trigger): string {
  if (trigger.kind === "once" && typeof trigger.at === "string") return `Once · ${new Date(trigger.at).toLocaleString()}`;
  if (trigger.kind === "interval" && typeof trigger.seconds === "number") {
    if (trigger.seconds % 3600 === 0) return `Every ${trigger.seconds / 3600} hour(s)`;
    if (trigger.seconds % 60 === 0) return `Every ${trigger.seconds / 60} minute(s)`;
    return `Every ${trigger.seconds} second(s)`;
  }
  if (trigger.kind === "cron" && typeof trigger.expression === "string") return trigger.expression;
  return `${trigger.kind.replaceAll("_", " ")} · not connected to the resident watcher`;
}

function timeLabel(value?: string | null): string {
  if (!value) return "Not scheduled";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString();
}

export function AutomationCenter() {
  const [snapshot, setSnapshot] = useState(empty);
  const [form, setForm] = useState({
    name: "",
    prompt: "",
    schedule: "daily at 09:00",
    missed: "run_once" as "skip" | "run_once" | "catch_up_bounded",
  });
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void invoke<AutomationSnapshot>("automation_snapshot")
      .then((next) => !disposed && setSnapshot(normalized(next)))
      .catch((caught) => !disposed && setError(String(caught)));
    void listen<AutomationSnapshot>("automation://changed", (event) => {
      if (!disposed) setSnapshot(normalized(event.payload));
    }).then((dispose) => {
      if (disposed) dispose();
      else unlisten = dispose;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  async function execute(action: Action, key: string = action.type): Promise<void> {
    setBusy(key);
    setError("");
    setNotice("");
    try {
      const result = await invoke<ActionResult>("automation_execute", { action });
      if (result.snapshot) setSnapshot(normalized(result.snapshot));
      if (result.message) setNotice(result.message);
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy("");
    }
  }

  async function create(event: FormEvent) {
    event.preventDefault();
    await execute({
      type: "create",
      name: form.name,
      prompt: form.prompt,
      schedule: form.schedule,
      missed_run_policy: form.missed,
      max_concurrency: 1,
      pause_after_failures: 3,
      maximum_catch_up_runs: 3,
      notification_route: "desktop",
    });
    setForm((current) => ({ ...current, name: "", prompt: "" }));
  }

  const approvals = snapshot.runs.filter((run) => run.status === "waiting_approval");
  const runsByAutomation = useMemo(() => {
    const groups = new Map<string, AutomationRun[]>();
    for (const run of snapshot.runs) {
      const group = groups.get(run.automation_id) ?? [];
      group.push(run);
      groups.set(run.automation_id, group);
    }
    return groups;
  }, [snapshot.runs]);

  return (
    <div className="automation-center">
      <header className="automation-hero">
        <div>
          <span className="eyebrow">DURABLE AUTOMATIONS</span>
          <h2>Schedules that survive restart</h2>
          <p>
            Every run uses an isolated agent session. Consequential tools pause for native approval instead of inheriting background permission.
          </p>
        </div>
        <div className="automation-health" aria-label="Automation runtime status">
          <b className={snapshot.resident_active && snapshot.global_enabled ? "ready" : "off"}>
            {snapshot.resident_active ? "Resident executor" : "Executor unavailable"}
          </b>
          <small>{snapshot.global_enabled ? "Global scheduling enabled" : "Disabled in Settings"}</small>
          <small>
            Native notifications {snapshot.notification.enabled && snapshot.notification.native_delivery ? "enabled" : "disabled"}
          </small>
        </div>
      </header>

      {!snapshot.global_enabled && (
        <div className="automation-warning">Automations are globally disabled. Definitions and history remain stored, but no due work will start.</div>
      )}
      {snapshot.recovered_runs > 0 && (
        <div className="automation-warning">
          {snapshot.recovered_runs} interrupted run(s) were paused after restart to prevent an unsafe replay.
        </div>
      )}
      {snapshot.notification.last_error && (
        <div className="automation-warning">Native notification delivery failed: {snapshot.notification.last_error}</div>
      )}
      {error && <div className="automation-error">{error}</div>}
      {notice && <div className="automation-notice">{notice}</div>}

      {approvals.length > 0 && (
        <section className="automation-approvals">
          <header><span className="eyebrow">APPROVALS</span><b>{approvals.length} waiting</b></header>
          {approvals.map((run) => {
            const automation = snapshot.automations.find((item) => item.id === run.automation_id);
            return (
              <article key={run.schedule_key}>
                <div><strong>{automation?.name ?? "Automation"}</strong><small>{run.approval_reason ?? "Background action needs review"}</small></div>
                <button disabled={Boolean(busy)} onClick={() => void execute({ type: "answer_approval", schedule_key: run.schedule_key, allow: false })}>Reject</button>
                <button className="primary" disabled={Boolean(busy)} onClick={() => void execute({ type: "answer_approval", schedule_key: run.schedule_key, allow: true })}>Allow once</button>
              </article>
            );
          })}
          <small>{snapshot.notification.action_guidance}</small>
        </section>
      )}

      <div className="automation-layout">
        <section className="automation-list">
          <div className="automation-section-title"><div><span className="eyebrow">SCHEDULES</span><h3>{snapshot.automations.length} automations</h3></div><button onClick={() => void execute({ type: "refresh" })}>Refresh</button></div>
          {snapshot.automations.length === 0 ? (
            <div className="automation-empty"><strong>No durable automations yet</strong><p>Create one with a supported schedule. It will be encrypted in the profile database and restored on startup.</p></div>
          ) : snapshot.automations.map((automation) => {
            const runs = (runsByAutomation.get(automation.id) ?? []).slice(0, 4);
            return (
              <article className="automation-card" key={automation.id}>
                <header>
                  <div><strong>{automation.name}</strong><small>{triggerLabel(automation.trigger)}</small></div>
                  <label className="automation-switch"><input type="checkbox" checked={automation.enabled} disabled={Boolean(busy)} onChange={(event) => void execute({ type: "set_enabled", automation_id: automation.id, enabled: event.target.checked }, `toggle-${automation.id}`)} /><span>{automation.enabled ? "Enabled" : "Disabled"}</span></label>
                </header>
                <p>{automation.goal_template}</p>
                <dl>
                  <div><dt>Next due</dt><dd>{timeLabel(automation.next_due_at)}</dd></div>
                  <div><dt>Missed runs</dt><dd>{automation.missed_run_policy.replaceAll("_", " ")}</dd></div>
                  <div><dt>Failure pause</dt><dd>{automation.consecutive_failures}/{automation.pause_after_failures}</dd></div>
                  <div><dt>Notifications</dt><dd>{automation.notification_route}</dd></div>
                </dl>
                {runs.length > 0 && <div className="automation-runs">{runs.map((run) => <div key={run.schedule_key}><span className={`run-status ${run.status}`}>{run.status.replaceAll("_", " ")}</span><small>{timeLabel(run.scheduled_for)} · attempt {run.attempt}</small>{run.result_summary && <p>{run.result_summary}</p>}</div>)}</div>}
                <footer>
                  <button disabled={Boolean(busy) || !snapshot.global_enabled} onClick={() => void execute({ type: "run_now", automation_id: automation.id }, `run-${automation.id}`)}>Run now</button>
                  {confirmDelete === automation.id ? <><span>Delete schedule and run history?</span><button onClick={() => setConfirmDelete(null)}>Cancel</button><button className="danger" onClick={() => { setConfirmDelete(null); void execute({ type: "delete", automation_id: automation.id, confirmed: true }, `delete-${automation.id}`); }}>Delete permanently</button></> : <button className="danger" onClick={() => setConfirmDelete(automation.id)}>Delete</button>}
                </footer>
              </article>
            );
          })}
        </section>

        <aside className="automation-create">
          <span className="eyebrow">NEW AUTOMATION</span>
          <h3>Schedule a prompt</h3>
          <form onSubmit={(event) => void create(event)}>
            <label>Name<input required value={form.name} onChange={(event) => setForm((current) => ({ ...current, name: event.target.value }))} placeholder="Morning briefing" /></label>
            <label>Prompt<textarea required value={form.prompt} onChange={(event) => setForm((current) => ({ ...current, prompt: event.target.value }))} placeholder="Summarize my open work and blockers." /></label>
            <label>Schedule<input required list="automation-schedules" value={form.schedule} onChange={(event) => setForm((current) => ({ ...current, schedule: event.target.value }))} /><datalist id="automation-schedules">{snapshot.supported_schedules.map((schedule) => <option key={schedule} value={schedule} />)}</datalist></label>
            <label>Missed run policy<select value={form.missed} onChange={(event) => setForm((current) => ({ ...current, missed: event.target.value as typeof current.missed }))}><option value="run_once">Run once</option><option value="skip">Skip</option><option value="catch_up_bounded">Catch up, bounded</option></select></label>
            <button className="primary" disabled={Boolean(busy)}>Create automation</button>
          </form>
          <div className="automation-support">
            <strong>Connected schedules</strong>
            <ul>{snapshot.supported_schedules.map((item) => <li key={item}>{item}</li>)}</ul>
            <strong>Stored but not watched yet</strong>
            <ul>{snapshot.unsupported_triggers.map((item) => <li key={item}>{item}</li>)}</ul>
          </div>
        </aside>
      </div>
    </div>
  );
}
