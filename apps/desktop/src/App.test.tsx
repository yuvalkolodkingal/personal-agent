import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { App } from "./App";

const baseInvoke = (command: string) => {
  if (command === "diagnostics") return Promise.resolve({ product: "Personal Agent", version: "0.1.0", platform: "test", arch: "test", opencode: { pinned: "1.18.23", topology: "test" }, capabilities: [] });
  if (command === "projection") return Promise.resolve({ last_sequence: 0, active_profile: "default", active_session: null, goals_total: 0, tasks_running: 0, approvals_waiting: 0, microphone_active: false, runtime_healthy: false, unclean_shutdowns: 0, recovered_unclean_run: false });
  if (command === "autostart_status") return new Promise(() => {});
  return Promise.resolve(null);
};

describe("workspace", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(baseInvoke);
  });
  afterEach(cleanup);

  it("exposes voice privacy and agent controls without relying on color", () => {
    render(<App />);
    expect(screen.getByRole("button", { name: "Voice capture unavailable" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Pause goal" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Toggle start at login" })).toBeDisabled();
    expect(screen.getAllByText("NOT CONNECTED").length).toBeGreaterThan(0);
  });

  it("requires a reviewed dry run and explicit consent before legacy import", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "migration_dry_run") return Promise.resolve({
        review_token: "review-token", plan: {
          source_fingerprint: "0123456789abcdef", requires_confirmation: true,
          remote_devices_require_repairing: true, plaintext_secrets_will_be_skipped: true,
          inputs: [{ kind: "memory", path: "/legacy/memory", bytes: 42, entries: 1, contains_possible_secrets: false, action: "import" }],
        },
      });
      return baseInvoke(command);
    });
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const importButton = screen.queryByRole("button", { name: "IMPORT REVIEWED DATA" });
    expect(importButton).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Legacy configuration root"), { target: { value: "/legacy/config" } });
    fireEvent.change(screen.getByLabelText("Legacy data root"), { target: { value: "/legacy/data" } });
    fireEvent.click(screen.getByRole("button", { name: "RUN METADATA-ONLY DRY RUN" }));
    const reviewedImport = await screen.findByRole("button", { name: "IMPORT REVIEWED DATA" });
    expect(reviewedImport).toBeDisabled();
    expect(screen.getByText("1 source groups found")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox"));
    await waitFor(() => expect(reviewedImport).toBeEnabled());
  });

  it("keeps every workspace destination keyboard reachable with an honest empty state", () => {
    render(<App />);
    for (const destination of ["Goals & tasks", "Browser", "Projects & terminal", "Artifacts", "Memory", "Automations", "Integrations", "Skills & agents", "Usage & egress", "Diagnostics"]) {
      fireEvent.click(screen.getByRole("button", { name: destination }));
      expect(screen.getByRole("heading", { level: 2, name: destination })).toBeInTheDocument();
    }
    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    expect(screen.getByRole("dialog", { name: "COMMAND PALETTE" })).toBeInTheDocument();
    fireEvent.click(screen.getAllByRole("button", { name: "Memory" }).at(-1)!);
    expect(screen.getByRole("heading", { level: 2, name: "Memory" })).toBeInTheDocument();
  });

  it("renders unknown additive event types by exact name and origin", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "projection") return Promise.resolve({
        last_sequence: 2, active_profile: "default", active_session: null,
        goals_total: 0, tasks_running: 0, approvals_waiting: 0,
        microphone_active: false, runtime_healthy: true,
        unclean_shutdowns: 0, recovered_unclean_run: false,
        recent_events: [
          { sequence: 1, event_type: "tool.started", origin: "gateway" },
          { sequence: 2, event_type: "future.additive.event", origin: "fixture" },
        ],
      });
      return baseInvoke(command);
    });
    render(<App />);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("projection"));
    fireEvent.click(screen.getByRole("button", { name: "History" }));
    expect(await screen.findByText("tool.started")).toBeInTheDocument();
    expect(screen.getByText("future.additive.event")).toBeInTheDocument();
    expect(screen.getByText("fixture")).toBeInTheDocument();
  });

  it("provides explicit HUD and non-color theme controls", () => {
    render(<App />);
    const hud = screen.getByRole("button", { name: "Toggle compact HUD" });
    expect(hud).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(hud);
    expect(hud).toHaveAttribute("aria-pressed", "true");
    const theme = screen.getByRole("button", { name: "Change theme; current theme cyan" });
    fireEvent.click(theme);
    expect(screen.getByRole("button", { name: "Change theme; current theme amber" })).toBeInTheDocument();
  });
});
