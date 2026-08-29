import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AutomationCenter, type AutomationSnapshot } from "./AutomationCenter";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => undefined) }));

const snapshot: AutomationSnapshot = {
  automations: [
    {
      id: "018f0000-0000-7000-8000-000000000001",
      name: "Morning briefing",
      goal_template: "Summarize my open work.",
      trigger: { kind: "interval", seconds: 86400 },
      enabled: true,
      max_concurrency: 1,
      missed_run_policy: "run_once",
      consecutive_failures: 0,
      pause_after_failures: 3,
      next_due_at: "2026-08-29T06:00:00Z",
      maximum_catch_up_runs: 3,
      quiet_hours_utc: [22, 7],
      notification_route: "desktop",
    },
  ],
  runs: [
    {
      id: "018f0000-0000-7000-8000-000000000002",
      automation_id: "018f0000-0000-7000-8000-000000000001",
      schedule_key: "approval-key",
      scheduled_for: "2026-08-28T06:00:00Z",
      status: "waiting_approval",
      attempt: 1,
      approval_reason: "write files",
    },
  ],
  resident_active: true,
  global_enabled: true,
  recovered_runs: 0,
  supported_schedules: ["daily at HH:MM (UTC)", "every N seconds/minutes/hours"],
  unsupported_triggers: ["file/directory watchers"],
  notification: {
    enabled: true,
    native_delivery: true,
    desktop_actions: false,
    action_guidance: "Approve or reject inside Personal Agent.",
    quiet_hours_utc: [22, 7],
  },
};

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation(async (command: string) => {
    if (command === "automation_snapshot") return structuredClone(snapshot);
    if (command === "automation_execute") return { snapshot: structuredClone(snapshot) };
    throw new Error(`unexpected command: ${command}`);
  });
});

afterEach(cleanup);

describe("AutomationCenter", () => {
  it("shows durable runtime, native notification, approval and unsupported states", async () => {
    render(<AutomationCenter />);
    expect((await screen.findAllByText("Morning briefing")).length).toBeGreaterThan(0);
    expect(screen.getByText("Resident executor")).toBeInTheDocument();
    expect(screen.getByText("Native notifications enabled")).toBeInTheDocument();
    expect(screen.getByText("write files")).toBeInTheDocument();
    expect(screen.getByText("file/directory watchers")).toBeInTheDocument();
    expect(screen.getByText("Approve or reject inside Personal Agent.")).toBeInTheDocument();
  });

  it("creates a persisted schedule through typed native IPC", async () => {
    render(<AutomationCenter />);
    await screen.findAllByText("Morning briefing");
    const form = screen.getByRole("button", { name: "Create automation" }).closest("form")!;
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Weekly review" } });
    fireEvent.change(within(form).getByLabelText("Prompt"), { target: { value: "Review the project." } });
    fireEvent.change(within(form).getByLabelText("Schedule"), { target: { value: "every 2 hours" } });
    fireEvent.click(within(form).getByRole("button", { name: "Create automation" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("automation_execute", {
        action: expect.objectContaining({
          type: "create",
          name: "Weekly review",
          prompt: "Review the project.",
          schedule: "every 2 hours",
          missed_run_policy: "run_once",
        }),
      }),
    );
  });

  it("answers the exact suspended run instead of starting a second attempt", async () => {
    render(<AutomationCenter />);
    await screen.findByText("write files");
    fireEvent.click(screen.getByRole("button", { name: "Allow once" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("automation_execute", {
        action: { type: "answer_approval", schedule_key: "approval-key", allow: true },
      }),
    );
  });

  it("requires a second explicit click before deleting history", async () => {
    render(<AutomationCenter />);
    await screen.findAllByText("Morning briefing");
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(invoke).not.toHaveBeenCalledWith("automation_execute", expect.anything());
    fireEvent.click(screen.getByRole("button", { name: "Delete permanently" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("automation_execute", {
        action: {
          type: "delete",
          automation_id: "018f0000-0000-7000-8000-000000000001",
          confirmed: true,
        },
      }),
    );
  });
});
