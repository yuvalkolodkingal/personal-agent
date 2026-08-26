import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
const listen = vi.hoisted(() => vi.fn());
const listeners = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
import { App, fallbackConfig } from "./App";

const projection = { last_sequence: 0, active_profile: "default", active_session: null, goals_total: 0, tasks_running: 0, approvals_waiting: 0, microphone_active: false, runtime_healthy: true, unclean_shutdowns: 0, recovered_unclean_run: false };

const baseInvoke = (command: string) => {
  if (command === "bootstrap") return Promise.reject(new Error("fixture uses safe UI defaults"));
  if (command === "diagnostics") return Promise.resolve({ product: "Personal Agent", version: "0.1.0", platform: "test", arch: "test", opencode: { pinned: "1.18.23", topology: "authenticated-loopback-sidecar" }, capabilities: [] });
  if (command === "autostart_status") return Promise.resolve(false);
  if (command === "chat_send") return Promise.resolve({ session_id: "ses_test", projection });
  return Promise.resolve({});
};

describe("desktop workspace", () => {
  beforeEach(() => {
    listeners.clear(); invoke.mockReset(); invoke.mockImplementation(baseInvoke); listen.mockReset();
    listen.mockImplementation((name: string, callback: (event: { payload: unknown }) => void) => { listeners.set(name, callback); return Promise.resolve(() => listeners.delete(name)); });
  });
  afterEach(cleanup);

  it("shows explicit private microphone state and disables capture until native STT is ready", () => {
    render(<App />);
    expect(screen.getByRole("button", { name: "Start voice capture" })).toBeDisabled();
    expect(screen.getByText("MICROPHONE PRIVATE")).toBeInTheDocument();
    expect(screen.getByText("STT MISSING")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Install offline voice" })).toBeEnabled();
  });

  it("exposes every configuration category plus full managed OpenCode JSON", () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const settingsNavigation = within(document.querySelector(".settings-nav")!);
    for (const section of ["Persona", "Runtime", "Agent", "Voice", "Ui", "Workspace", "Privacy", "Browser", "Memory", "Automation", "Notifications", "Updates", "Opencode", "Providers & models", "System", "Advanced full config"]) {
      expect(settingsNavigation.getByRole("button", { name: section })).toBeInTheDocument();
    }
    fireEvent.click(settingsNavigation.getByRole("button", { name: "Opencode" }));
    expect(screen.getByText(/Full OpenCode-compatible JSON/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save all changes" })).toBeEnabled();
  });

  it("keeps all desktop destinations keyboard reachable", () => {
    render(<App />);
    for (const destination of ["Goals & tasks", "Browser", "Projects & terminal", "Artifacts", "History", "Memory", "Automations", "Integrations", "Skills & agents", "Usage & egress", "Diagnostics", "Settings"]) {
      fireEvent.click(screen.getByRole("button", { name: destination }));
      expect(screen.getByRole("heading", { level: 1, name: destination })).toBeInTheDocument();
    }
    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    expect(screen.getByRole("dialog", { name: "COMMAND PALETTE" })).toBeInTheDocument();
  });

  it("renders unknown additive runtime events by exact type and origin", async () => {
    render(<App />);
    await waitFor(() => expect(listeners.has("runtime-event")).toBe(true));
    const payload = new TextEncoder().encode(JSON.stringify({ value: 42 }));
    act(() => listeners.get("runtime-event")?.({ payload: { schema_version: 1, event_id: "evt_future", wall_clock_timestamp: "2026-08-26T12:00:00Z", monotonic_sequence: 9, origin: "future-fixture", profile_id: "default", type: "future.additive.event", payload_json: Array.from(payload) } }));
    fireEvent.click(screen.getByRole("button", { name: "History" }));
    expect(screen.getByText("future.additive.event")).toBeInTheDocument();
    expect(screen.getByText(/future-fixture/)).toBeInTheDocument();
  });

  it("submits a real chat request with runtime selection and attachment contract", async () => {
    render(<App />);
    fireEvent.change(screen.getByRole("textbox", { name: "Message JARVIS" }), { target: { value: "Inspect this project" } });
    fireEvent.click(screen.getByRole("button", { name: "↑" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("chat_send", expect.objectContaining({ text: "Inspect this project", attachments: [], speakResponse: false })));
    expect(screen.getByText("Inspect this project")).toBeInTheDocument();
  });

  it("always resolves a failed native turn instead of remaining in thinking", async () => {
    render(<App />);
    await waitFor(() => expect(listeners.has("runtime-turn-complete")).toBe(true));
    fireEvent.change(screen.getByRole("textbox", { name: "Message JARVIS" }), { target: { value: "Hello" } });
    fireEvent.click(screen.getByRole("button", { name: "↑" }));
    await waitFor(() => expect(screen.getByText("Connecting to model")).toBeInTheDocument());
    act(() => listeners.get("runtime-turn-complete")?.({ payload: { text: "", speak: false, status: "failed", error: "Provider sign-in is required." } }));
    expect((await screen.findAllByText("Provider sign-in is required.")).length).toBeGreaterThan(0);
    expect(screen.getByText("Stopped / failed")).toBeInTheDocument();
    expect(screen.queryByText("Connecting to model")).not.toBeInTheDocument();
  });

  it("starts the provider-advertised OpenCode OAuth flow inside settings", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "bootstrap") return Promise.resolve({
        config: fallbackConfig,
        projection,
        history: [],
        voice: { stt_ready: true, tts_ready: true, playback_ready: true, details: [] },
        catalog: {
          provider_auth: { available: true, data: { openai: [{ type: "oauth", label: "ChatGPT Plus / Pro" }] } },
          providers: { available: true, data: { all: [{ id: "openai", name: "OpenAI" }], connected: [] } },
          models: { available: true, data: [{ provider_id: "openai", model_id: "gpt-5", local: false, reasoning: true, tool_calls: true, input_modalities: ["text"], output_modalities: ["text"] }] },
        },
      });
      if (command === "provider_oauth_authorize") return Promise.resolve({ url: "https://auth.openai.com/authorize", method: "auto", instructions: "Complete sign-in in your browser." });
      return baseInvoke(command);
    });
    render(<App />);
    await waitFor(() => expect(screen.getByRole("button", { name: "Start voice capture" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Providers & models" }));
    expect(await screen.findByText("ChatGPT Plus / Pro")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Continue with browser" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("provider_oauth_authorize", {
      providerId: "openai", method: 0, inputs: {}, openBrowser: true,
    }));
    expect(await screen.findByRole("dialog", { name: "Complete provider sign in" })).toBeInTheDocument();
  });
});
