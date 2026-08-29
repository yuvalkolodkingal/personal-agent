import "@testing-library/jest-dom/vitest";
import { StrictMode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
const listen = vi.hoisted(() => vi.fn());
const listeners = vi.hoisted(
  () => new Map<string, (event: { payload: unknown }) => void>(),
);
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
import { App, fallbackConfig } from "./App";

const projection = {
  last_sequence: 0,
  active_profile: "default",
  active_session: null,
  goals_total: 0,
  tasks_running: 0,
  approvals_waiting: 0,
  microphone_active: false,
  runtime_healthy: true,
  unclean_shutdowns: 0,
  recovered_unclean_run: false,
};

const baseInvoke = (command: string) => {
  if (command === "bootstrap")
    return Promise.reject(new Error("fixture uses safe UI defaults"));
  if (command === "diagnostics")
    return Promise.resolve({
      product: "Personal Agent",
      version: "0.1.0",
      platform: "test",
      arch: "test",
      opencode: {
        pinned: "1.18.23",
        topology: "authenticated-loopback-sidecar",
      },
      capabilities: [],
    });
  if (command === "autostart_status") return Promise.resolve(false);
  if (command === "chat_send")
    return Promise.resolve({
      session_id: "ses_test",
      message_id: "msg_test",
      projection,
    });
  return Promise.resolve({});
};

describe("desktop workspace", () => {
  beforeEach(() => {
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: 1440,
    });
    listeners.clear();
    invoke.mockReset();
    invoke.mockImplementation(baseInvoke);
    listen.mockReset();
    listen.mockImplementation(
      (name: string, callback: (event: { payload: unknown }) => void) => {
        listeners.set(name, callback);
        return Promise.resolve(() => listeners.delete(name));
      },
    );
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("subscribes before signaling first paint and applies deferred capabilities", async () => {
    render(<App />);
    await waitFor(() =>
      expect(listeners.has("capabilities-ready")).toBe(true),
    );
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("startup_window_painted"),
    );
    const listenIndex = listen.mock.calls.findIndex(
      ([event]) => event === "capabilities-ready",
    );
    const paintIndex = invoke.mock.calls.findIndex(
      ([command]) => command === "startup_window_painted",
    );
    expect(listen.mock.invocationCallOrder[listenIndex]).toBeLessThan(
      invoke.mock.invocationCallOrder[paintIndex]!,
    );

    act(() =>
      listeners.get("capabilities-ready")?.({
        payload: {
          capabilities: [
            {
              id: "desktop.active_view",
              backend: "AT-SPI",
              status: { state: "supported" },
            },
          ],
          error: null,
        },
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Diagnostics" }));
    expect(screen.getByText("desktop.active_view")).toBeInTheDocument();
    expect(screen.getByText("AT-SPI")).toBeInTheDocument();
  });

  it("shows explicit private microphone state and disables capture until native STT is ready", () => {
    render(<App />);
    expect(
      screen.getByRole("button", { name: "Start voice capture" }),
    ).toBeDisabled();
    expect(screen.getByText("MICROPHONE PRIVATE")).toBeInTheDocument();
    expect(screen.getByText("STT MISSING")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Install offline voice" }),
    ).toBeEnabled();
  });

  it("collapses the session rail on compact screens and keeps it available as a toggle", () => {
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: 900,
    });
    render(<App />);
    expect(screen.queryByText("＋ New session")).not.toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Toggle session rail" }),
    );
    expect(screen.getByText("＋ New session")).toBeInTheDocument();
  });

  it("exposes every configuration category plus full managed OpenCode JSON", () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const settingsNavigation = within(document.querySelector(".settings-nav")!);
    for (const section of [
      "Persona",
      "Runtime",
      "Agent",
      "Voice",
      "Ui",
      "Workspace",
      "Privacy",
      "Browser",
      "Memory",
      "Automation",
      "Notifications",
      "Updates",
      "Opencode",
      "Providers & models",
      "System",
      "Advanced full config",
    ]) {
      expect(
        settingsNavigation.getByRole("button", { name: section }),
      ).toBeInTheDocument();
    }
    fireEvent.click(
      settingsNavigation.getByRole("button", { name: "Opencode" }),
    );
    expect(
      screen.getByText(/Full OpenCode-compatible JSON/),
    ).toBeInTheDocument();
    fireEvent.click(settingsNavigation.getByRole("button", { name: "System" }));
    expect(screen.getByText("Super + J")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Save all changes" }),
    ).toBeEnabled();
  });

  it("keeps all desktop destinations keyboard reachable", () => {
    render(<App />);
    for (const destination of [
      "Goals & tasks",
      "Browser",
      "Projects & terminal",
      "Artifacts",
      "History",
      "Memory",
      "Automations",
      "Integrations",
      "Skills & agents",
      "Usage & egress",
      "Diagnostics",
      "Settings",
    ]) {
      fireEvent.click(screen.getByRole("button", { name: destination }));
      expect(
        screen.getByRole("heading", { level: 1, name: destination }),
      ).toBeInTheDocument();
    }
    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    expect(
      screen.getByRole("dialog", { name: "COMMAND PALETTE" }),
    ).toBeInTheDocument();
  });

  it("selects multiple sessions and deletes them with one confirmation", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "bootstrap")
        return Promise.resolve({
          config: fallbackConfig,
          projection,
          history: [],
          voice: {
            stt_ready: false,
            tts_ready: false,
            playback_ready: false,
            details: [],
          },
          catalog: {
            sessions: {
              available: true,
              data: [
                { id: "ses_first", title: "First session" },
                { id: "ses_second", title: "Second session" },
              ],
            },
          },
        });
      return baseInvoke(command);
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<App />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Select sessions" }),
    );
    fireEvent.click(
      screen.getByRole("checkbox", { name: "Select session ses_first" }),
    );
    fireEvent.click(
      screen.getByRole("checkbox", { name: "Select session ses_second" }),
    );
    expect(screen.getByText("2 selected")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        "session_action",
        expect.objectContaining({
          action: "delete",
          sessionId: "ses_first",
          confirmed: true,
        }),
      );
      expect(invoke).toHaveBeenCalledWith(
        "session_action",
        expect.objectContaining({
          action: "delete",
          sessionId: "ses_second",
          confirmed: true,
        }),
      );
    });
    expect(window.confirm).toHaveBeenCalledWith(
      "Delete 2 selected sessions permanently?",
    );
  });

  it("keeps only recent sessions visible until older history is requested", async () => {
    const sessionHistory = Array.from({ length: 25 }, (_, index) => ({
      id: `ses_${String(index + 1).padStart(2, "0")}`,
      title: `Session ${String(index + 1).padStart(2, "0")}`,
      time: { updated: index + 1 },
    }));
    invoke.mockImplementation((command: string) => {
      if (command === "bootstrap")
        return Promise.resolve({
          config: fallbackConfig,
          projection,
          history: [],
          voice: {
            stt_ready: false,
            tts_ready: false,
            playback_ready: false,
            details: [],
          },
          catalog: { sessions: { available: true, data: sessionHistory } },
        });
      return baseInvoke(command);
    });
    render(<App />);
    expect(await screen.findByText("Session 25")).toBeInTheDocument();
    expect(screen.getByText("Showing 12 of 25")).toBeInTheDocument();
    expect(screen.queryByText("Session 13")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /New session/ })).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "Show 12 older" }));
    expect(screen.getByText("Session 13")).toBeInTheDocument();
    expect(screen.getByText("Showing 24 of 25")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Show 1 older" }));
    expect(
      screen.getByRole("button", { name: "Hide older" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Hide older" }));
    expect(screen.getByText("Showing 12 of 25")).toBeInTheDocument();
    expect(screen.queryByText("Session 13")).not.toBeInTheDocument();
  });

  it("renders unknown additive runtime events by exact type and origin", async () => {
    render(<App />);
    await waitFor(() => expect(listeners.has("runtime-event")).toBe(true));
    const payload = new TextEncoder().encode(JSON.stringify({ value: 42 }));
    act(() =>
      listeners.get("runtime-event")?.({
        payload: {
          schema_version: 1,
          event_id: "evt_future",
          wall_clock_timestamp: "2026-08-26T12:00:00Z",
          monotonic_sequence: 9,
          origin: "future-fixture",
          profile_id: "default",
          type: "future.additive.event",
          payload_json: Array.from(payload),
        },
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "History" }));
    expect(screen.getByText("future.additive.event")).toBeInTheDocument();
    expect(screen.getByText(/future-fixture/)).toBeInTheDocument();
  });

  it("appends each runtime delta once when ChatView remounts under StrictMode", async () => {
    type Listener = (event: { payload: unknown }) => void;
    const activeListeners = new Map<string, Set<Listener>>();
    const pending: Array<{
      name: string;
      callback: Listener;
      resolve: (unlisten: () => void) => void;
    }> = [];
    listen.mockImplementation((name: string, callback: Listener) => {
      const registered = activeListeners.get(name) ?? new Set<Listener>();
      registered.add(callback);
      activeListeners.set(name, registered);
      return new Promise<() => void>((resolve) => {
        pending.push({ name, callback, resolve });
      });
    });

    render(
      <StrictMode>
        <App />
      </StrictMode>,
    );
    await waitFor(() => expect(pending).toHaveLength(8));
    await act(async () => {
      for (const registration of pending) {
        registration.resolve(() =>
          activeListeners
            .get(registration.name)
            ?.delete(registration.callback),
        );
      }
      await Promise.resolve();
    });
    await waitFor(() =>
      expect(activeListeners.get("runtime-event")).toHaveLength(1),
    );

    fireEvent.change(screen.getByRole("textbox", { name: "Message JARVIS" }), {
      target: { value: "Stream once" },
    });
    fireEvent.click(screen.getByRole("button", { name: "↑" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "chat_send",
        expect.objectContaining({ text: "Stream once" }),
      ),
    );
    const payload = new TextEncoder().encode(
      JSON.stringify({ delta: "Only one delta." }),
    );
    act(() => {
      for (const callback of activeListeners.get("runtime-event") ?? []) {
        callback({
          payload: {
            schema_version: 1,
            event_id: "evt_single_delta",
            wall_clock_timestamp: "2026-08-29T12:00:00Z",
            monotonic_sequence: 1,
            origin: "strict-mode-test",
            profile_id: "default",
            type: "response.delta",
            payload_json: Array.from(payload),
          },
        });
      }
    });
    await waitFor(() =>
      expect(
        document.querySelector(".chat-message.assistant p"),
      ).toHaveTextContent(/^Only one delta\.$/),
    );
  });

  it("submits a real chat request with runtime selection and attachment contract", async () => {
    render(<App />);
    fireEvent.change(screen.getByRole("textbox", { name: "Message JARVIS" }), {
      target: { value: "Inspect this project" },
    });
    fireEvent.click(screen.getByRole("button", { name: "↑" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "chat_send",
        expect.objectContaining({
          text: "Inspect this project",
          attachments: [],
          speakResponse: false,
        }),
      ),
    );
    expect(screen.getByText("Inspect this project")).toBeInTheDocument();
  });

  it("sends the selected provider model on every chat request", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "bootstrap")
        return Promise.resolve({
          config: fallbackConfig,
          projection,
          history: [],
          voice: {
            stt_ready: false,
            tts_ready: false,
            playback_ready: false,
            details: [],
          },
          catalog: {
            models: {
              available: true,
              data: [
                {
                  provider_id: "openai",
                  model_id: "gpt-5",
                  local: false,
                  reasoning: true,
                  tool_calls: true,
                  input_modalities: ["text"],
                  output_modalities: ["text"],
                },
              ],
            },
          },
        });
      return baseInvoke(command);
    });
    render(<App />);
    const selector = await screen.findByRole("button", {
      name: /Model selector:/,
    });
    fireEvent.click(selector);
    const palette = await screen.findByRole("dialog", {
      name: "Model and provider selector",
    });
    fireEvent.click(within(palette).getByRole("button", { name: /gpt-5/ }));
    fireEvent.change(screen.getByRole("textbox", { name: "Message JARVIS" }), {
      target: { value: "Use this model" },
    });
    fireEvent.click(screen.getByRole("button", { name: "↑" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "chat_send",
        expect.objectContaining({
          text: "Use this model",
          model: "openai/gpt-5",
        }),
      ),
    );
  });

  it("always resolves a failed native turn instead of remaining in thinking", async () => {
    render(<App />);
    await waitFor(() =>
      expect(listeners.has("runtime-turn-complete")).toBe(true),
    );
    fireEvent.change(screen.getByRole("textbox", { name: "Message JARVIS" }), {
      target: { value: "Hello" },
    });
    fireEvent.click(screen.getByRole("button", { name: "↑" }));
    await waitFor(() =>
      expect(screen.getByText("Connecting to model")).toBeInTheDocument(),
    );
    act(() =>
      listeners.get("runtime-turn-complete")?.({
        payload: {
          text: "",
          speak: false,
          status: "failed",
          error: "Provider sign-in is required.",
        },
      }),
    );
    expect(
      (await screen.findAllByText("Provider sign-in is required.")).length,
    ).toBeGreaterThan(0);
    expect(screen.getByText("Stopped / failed")).toBeInTheDocument();
    expect(screen.queryByText("Connecting to model")).not.toBeInTheDocument();
  });

  it("recovers a completed reply when the native completion event is missed", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "chat_turn_status")
        return Promise.resolve({
          completed: true,
          text: "Recovered answer.",
          error: null,
        });
      return baseInvoke(command);
    });
    render(<App />);
    fireEvent.change(screen.getByRole("textbox", { name: "Message JARVIS" }), {
      target: { value: "Recover this turn" },
    });
    fireEvent.click(screen.getByRole("button", { name: "↑" }));
    expect(await screen.findByText("Recovered answer.")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith(
      "chat_turn_status",
      expect.objectContaining({
        sessionId: "ses_test",
        promptMessageId: "msg_test",
      }),
    );
    expect(screen.queryByText("Connecting to model")).not.toBeInTheDocument();
  });

  it("runs the private STT and TTS round-trip from Voice settings", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "bootstrap")
        return Promise.resolve({
          config: fallbackConfig,
          projection,
          history: [],
          voice: {
            stt_ready: true,
            tts_ready: true,
            playback_ready: true,
            details: [],
          },
          catalog: {},
        });
      if (command === "voice_self_test")
        return Promise.resolve({
          transcript: "Personal Agent voice test",
          synthesis_ms: 120,
          recognition_ms: 380,
        });
      return baseInvoke(command);
    });
    render(<App />);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Start voice capture" }),
      ).toBeEnabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Test STT + TTS" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("voice_self_test"));
    expect(
      await screen.findByText(/Voice pipeline passed in 500 ms/),
    ).toBeInTheDocument();
  });

  it("starts the provider-advertised OpenCode OAuth flow inside settings", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "bootstrap")
        return Promise.resolve({
          config: fallbackConfig,
          projection,
          history: [],
          voice: {
            stt_ready: true,
            tts_ready: true,
            playback_ready: true,
            details: [],
          },
          catalog: {
            provider_auth: {
              available: true,
              data: {
                openai: [{ type: "oauth", label: "ChatGPT Plus / Pro" }],
              },
            },
            providers: {
              available: true,
              data: { all: [{ id: "openai", name: "OpenAI" }], connected: [] },
            },
            models: {
              available: true,
              data: [
                {
                  provider_id: "openai",
                  model_id: "gpt-5",
                  local: false,
                  reasoning: true,
                  tool_calls: true,
                  input_modalities: ["text"],
                  output_modalities: ["text"],
                },
              ],
            },
          },
        });
      if (command === "provider_oauth_authorize")
        return Promise.resolve({
          url: "https://auth.openai.com/authorize",
          method: "auto",
          instructions: "Complete sign-in in your browser.",
        });
      return baseInvoke(command);
    });
    render(<App />);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Start voice capture" }),
      ).toBeEnabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: /Connect provider/ }));
    expect(
      screen.getByRole("heading", { level: 1, name: "Settings" }),
    ).toBeInTheDocument();
    expect(await screen.findByText("ChatGPT Plus / Pro")).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Continue with browser" }),
    );
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("provider_oauth_authorize", {
        providerId: "openai",
        method: 0,
        inputs: {},
        openBrowser: true,
      }),
    );
    expect(
      await screen.findByRole("dialog", { name: "Complete provider sign in" }),
    ).toBeInTheDocument();
  });
});
