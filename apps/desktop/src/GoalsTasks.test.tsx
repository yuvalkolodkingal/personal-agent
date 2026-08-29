import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { GoalsSnapshot } from "./GoalsTasks";

const invoke = vi.hoisted(() => vi.fn());
const listen = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

import { GoalsTasks } from "./GoalsTasks";

const projection = {
  last_sequence: 4,
  active_profile: "default",
  active_session: null,
  goals_total: 1,
  tasks_running: 1,
  approvals_waiting: 1,
  microphone_active: false,
  runtime_healthy: true,
  unclean_shutdowns: 0,
  recovered_unclean_run: false,
};

const snapshot: GoalsSnapshot = {
  resident_active: true,
  recovered_tasks: 1,
  maximum_parallelism: 2,
  activities: [
    {
      sequence: 4,
      event_type: "approval.requested",
      goal_id: "018f-goal",
      task_id: "018f-task",
      timestamp: "2026-08-28T12:00:00Z",
    },
  ],
  goals: [
    {
      goal: {
        id: "018f-goal",
        objective: "Ship durable goals",
        success_criteria: ["Focused tests pass"],
        created_at: "2026-08-28T12:00:00Z",
        priority: 4,
        status: "running",
        plan_revision: 1,
      },
      edges: [],
      approvals: [
        {
          goal_id: "018f-goal",
          task_id: "018f-task",
          reason: "write workspace",
          requested_at: "2026-08-28T12:01:00Z",
        },
      ],
      tasks: [
        {
          id: "018f-task",
          title: "Focused tests pass",
          status: "waiting",
          progress: 20,
          attempt: 1,
          max_attempts: 3,
          assigned_agent: "build",
          execution_zone: "workspace",
          checkpoint_id: "ses_background",
        },
      ],
    },
  ],
};

describe("GoalsTasks", () => {
  beforeEach(() => {
    invoke.mockReset();
    listen.mockReset();
    listen.mockResolvedValue(() => undefined);
    invoke.mockImplementation((command: string) => {
      if (command === "goals_snapshot") return Promise.resolve(snapshot);
      if (command === "goals_execute")
        return Promise.resolve({ snapshot, projection, message: "Updated." });
      return Promise.reject(new Error(`unexpected ${command}`));
    });
  });

  afterEach(cleanup);

  it("creates a durable goal with one task per observable criterion", async () => {
    const onProjection = vi.fn();
    render(<GoalsTasks onProjection={onProjection} />);
    await screen.findByText("Ship durable goals");

    fireEvent.change(screen.getByLabelText("Goal objective"), {
      target: { value: "Build the supervisor" },
    });
    fireEvent.change(screen.getByLabelText("Goal success criteria"), {
      target: { value: "State survives restart\nFocused tests pass" },
    });
    fireEvent.change(screen.getByLabelText("Goal priority"), {
      target: { value: "7" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create & run" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("goals_execute", {
        action: {
          type: "create",
          objective: "Build the supervisor",
          success_criteria: ["State survives restart", "Focused tests pass"],
          priority: 7,
        },
      }),
    );
    expect(onProjection).toHaveBeenCalledWith(projection);
  });

  it("projects recovery, task state, and approval-bound actions", async () => {
    render(<GoalsTasks />);
    expect(await screen.findByText(/Recovered 1 interrupted task/)).toBeInTheDocument();
    expect(screen.getByText("write workspace")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Allow once" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("goals_execute", {
        action: {
          type: "answer_approval",
          goal_id: "018f-goal",
          task_id: "018f-task",
          allow: true,
        },
      }),
    );

    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("goals_execute", {
        action: { type: "pause_goal", goal_id: "018f-goal" },
      }),
    );
  });
});
