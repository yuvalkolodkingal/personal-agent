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
  if (command === "goals_snapshot")
    return Promise.resolve({
      goals: [],
      activities: [],
      resident_active: false,
      recovered_tasks: 0,
      maximum_parallelism: 0,
    });
  if (command === "artifact_snapshot")
    return Promise.resolve({ artifacts: [], cards: [], order: [], focused: null });
  if (command === "automation_snapshot")
    return Promise.resolve({
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
        action_guidance: "Unavailable in the test fixture.",
      },
    });
  if (command === "skills_agents_snapshot")
    return Promise.resolve({
      agents: { available: false },
      commands: { available: false },
      skills: { available: false },
      managed_documents: [],
      default_agent: "build",
    });
  if (command === "usage_snapshot")
    return Promise.resolve({
      records: [],
      egress: [],
      turns: {},
      sessions: {},
      days: {},
      scopes: {},
      usage_total: 0,
      egress_total: 0,
      limit: 50,
      offset: 0,
      pricing_policy: "Only provider-reported cost is totaled.",
    });
  if (command === "connector_list") return Promise.resolve([]);
  if (command === "mcp_manager_snapshot")
    return Promise.resolve({
      servers: [],
      audit_events: [],
      protocol_version: "2026-07-28",
    });
  if (command === "runtime_resource") return Promise.resolve([]);
  if (command === "desktop_status")
    return Promise.resolve({
      connected: false,
      connection_detail: "Unavailable in the test fixture.",
      plan: {
        platform: "test",
        session: "headless",
        screen_capture_backend: "none",
        accessibility_backend: "none",
        input_backend: "none",
        capabilities: [],
      },
      permissions: {
        accessibility: { state: "unavailable" },
        screen_capture: { state: "unavailable" },
        input_control: { state: "unavailable" },
      },
    });
  if (command === "portal_status")
    return Promise.resolve({
      interfaces: {
        available_source_types: 0,
        available_cursor_modes: 0,
      },
      phase: "idle",
      consent: "unavailable",
      streams: [],
      pipewire_transport: false,
      detail: "Unavailable in the test fixture.",
    });
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
    // The capability truth table and the derived implementation audit both
    // render live capabilities, so scope to the table this assertion is about.
    const truth = await screen.findByLabelText("Capability and resource truth");
    expect(within(truth).getByText("desktop.active_view")).toBeInTheDocument();
    expect(within(truth).getByText("AT-SPI")).toBeInTheDocument();
    // The audit is derived from the same probe, never a hardcoded table.
    const audit = screen.getByLabelText("Implementation audit");
    expect(within(audit).getByText("desktop.active_view")).toBeInTheDocument();
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

  it("exposes every configuration category plus full managed OpenCode JSON", async () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    await screen.findByRole("heading", { level: 2, name: "Voice Lab" });
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

  it("keeps all desktop destinations keyboard reachable and renderable", async () => {
    render(<App />);
    const destinations = [
      ["Goals & tasks", "Goals & tasks"],
      ["Browser", "Browser automation boundaries"],
      ["Projects & terminal", "Projects, files, VCS and terminals"],
      ["Artifacts", "Encrypted, versioned work products"],
      ["History", "History and audit trail"],
      ["Memory", "Trusted, reviewable memory"],
      ["Automations", "Schedules that survive restart"],
      ["Integrations", "Providers, MCP servers and integrations"],
      ["Skills & agents", "Skills & agents"],
      ["Usage & egress", "Usage & egress"],
      ["Diagnostics", "Diagnostics"],
      ["Settings", "Voice Lab"],
    ] as const;
    for (const [destination, viewHeading] of destinations) {
      fireEvent.click(screen.getByRole("button", { name: destination }));
      expect(
        screen.getByRole("heading", { level: 1, name: destination }),
      ).toBeInTheDocument();
      expect(
        await screen.findByRole("heading", { level: 2, name: viewHeading }),
      ).toBeInTheDocument();
      if (destination === "Browser") {
        expect(
          await screen.findByRole("heading", {
            level: 2,
            name: "See and control the active view",
          }),
        ).toBeInTheDocument();
      }
      if (destination === "Integrations") {
        expect(
          await screen.findByRole("heading", { level: 1, name: "MCP Manager" }),
        ).toBeInTheDocument();
      }
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
        });
      if (command === "runtime_catalog")
        return Promise.resolve({
          sessions: {
            available: true,
            data: [
              { id: "ses_first", title: "First session" },
              { id: "ses_second", title: "Second session" },
            ],
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
      await screen.findByRole("checkbox", {
        name: "Select session ses_first",
      }),
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
        });
      if (command === "runtime_catalog")
        return Promise.resolve({
          sessions: { available: true, data: sessionHistory },
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

  it("releases the startup shield after slim bootstrap without waiting for the catalog", async () => {
    let resolveBootstrap!: (value: unknown) => void;
    let resolveCatalog!: (value: unknown) => void;
    invoke.mockImplementation((command: string) => {
      if (command === "bootstrap")
        return new Promise((resolve) => {
          resolveBootstrap = resolve;
        });
      if (command === "runtime_catalog")
        return new Promise((resolve) => {
          resolveCatalog = resolve;
        });
      return baseInvoke(command);
    });
    render(<App />);
    expect(screen.getByRole("status")).toHaveTextContent(
      "Starting your private agent",
    );

    act(() =>
      resolveBootstrap({
        config: fallbackConfig,
        projection,
        history: [],
        voice: {
          stt_ready: false,
          tts_ready: false,
          playback_ready: false,
          details: [],
        },
      }),
    );

    await waitFor(() =>
      expect(screen.queryByRole("status")).not.toBeInTheDocument(),
    );
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("runtime_catalog", {
        directory: fallbackConfig.runtime.working_directory,
        includeMemory: false,
      }),
    );
    act(() => resolveCatalog({}));
  });

  it("loads settings and memory-system data only from their lazy catalogs", async () => {
    invoke.mockImplementation(
      (command: string, arguments_: { includeMemory?: boolean } = {}) => {
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
          });
        if (command === "runtime_catalog" && arguments_.includeMemory)
          return Promise.resolve({
            memories: {
              available: true,
              data: [
                {
                  id: "memory-1",
                  content: "Remember the espresso setting",
                  trust: "confirmed",
                  confidence: 1,
                },
              ],
            },
            memory_styles: {
              available: true,
              data: [
                {
                  id: "style-1",
                  description: "Use terse release notes",
                  reviewed: true,
                  confidence: 0.9,
                },
              ],
            },
            memory_projects: {
              available: true,
              data: {
                nodes: [
                  {
                    id: "project-1",
                    name: "personal-agent",
                    kind: "repository",
                  },
                ],
                relations: [],
              },
            },
          });
        if (command === "runtime_catalog")
          return Promise.resolve({
            provider_auth: {
              available: true,
              data: {
                openai: [{ type: "oauth", label: "ChatGPT Plus / Pro" }],
              },
            },
            providers: {
              available: true,
              data: {
                all: [{ id: "openai", name: "OpenAI" }],
                connected: [],
              },
            },
            models: { available: true, data: [] },
          });
        return baseInvoke(command);
      },
    );

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Providers & models" }),
    );
    expect(await screen.findByText("ChatGPT Plus / Pro")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("runtime_catalog", {
      directory: fallbackConfig.runtime.working_directory,
      includeMemory: false,
    });

    fireEvent.click(
      within(document.querySelector(".sidebar nav")!).getByRole("button", {
        name: "Memory",
      }),
    );
    expect(
      await screen.findByText("Remember the espresso setting"),
    ).toBeInTheDocument();
    expect(screen.getByText("Use terse release notes")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Projects" }));
    expect(screen.getByText("personal-agent")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("runtime_catalog", {
      includeMemory: true,
    });
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
    expect(await screen.findByText("future.additive.event")).toBeInTheDocument();
    expect(await screen.findByText(/future-fixture/)).toBeInTheDocument();
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
        });
      if (command === "runtime_catalog")
        return Promise.resolve({
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
    const interval = vi.spyOn(window, "setInterval");
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
    await waitFor(() =>
      expect(screen.getByText("Connecting to model")).toBeInTheDocument(),
    );
    const safetyPoll = await waitFor(() => {
      const call = interval.mock.calls.find(([, delay]) => delay === 15_000);
      expect(call).toBeDefined();
      return call?.[0];
    });
    expect(
      invoke.mock.calls.some(([command]) => command === "chat_turn_status"),
    ).toBe(false);
    expect(typeof safetyPoll).toBe("function");
    act(() => {
      if (typeof safetyPoll === "function") safetyPoll();
    });
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
        });
      if (command === "runtime_catalog")
        return Promise.resolve({
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
  const runtimeEvent = (
    type: string,
    payload: Record<string, unknown>,
    at = "2026-08-30T12:00:00.000Z",
    id = `evt-${type}-${at}`,
  ) => ({
    schema_version: 1,
    event_id: id,
    wall_clock_timestamp: at,
    monotonic_sequence: 1,
    origin: "fixture-turn",
    profile_id: "default",
    type,
    payload_json: Array.from(
      new TextEncoder().encode(JSON.stringify(payload)),
    ),
  });

  const streamAssistantText = async (prompt: string, text: string) => {
    render(<App />);
    await waitFor(() => expect(listeners.has("runtime-event")).toBe(true));
    fireEvent.change(screen.getByRole("textbox", { name: "Message JARVIS" }), {
      target: { value: prompt },
    });
    fireEvent.click(screen.getByRole("button", { name: "↑" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "chat_send",
        expect.objectContaining({ text: prompt }),
      ),
    );
    act(() =>
      listeners.get("runtime-event")?.({
        payload: runtimeEvent("response.delta", { delta: text }),
      }),
    );
    act(() =>
      listeners.get("runtime-turn-complete")?.({
        payload: { text: "", speak: false, status: "completed" },
      }),
    );
  };

  it("renders assistant markdown, code blocks and per-block copy in the transcript", async () => {
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    await streamAssistantText(
      "Explain",
      [
        "## Plan",
        "",
        "Use **ripgrep** and see [the docs](https://example.com/rg).",
        "",
        "```sh",
        "rg --files",
        "```",
      ].join("\n"),
    );
    const transcript = await screen.findByRole("heading", {
      name: "Plan",
      level: 2,
    });
    expect(transcript).toBeInTheDocument();
    expect(document.querySelector(".chat-message.assistant strong"))
      .toBeInTheDocument();
    expect(screen.getByRole("link", { name: "the docs" })).toHaveAttribute(
      "href",
      "https://example.com/rg",
    );
    expect(
      document.querySelector('.markdown-code[data-language="sh"] code'),
    ).toHaveTextContent("rg --files");
    fireEvent.click(screen.getByRole("button", { name: "Copy sh code block" }));
    expect(writeText).toHaveBeenLastCalledWith("rg --files");
  });

  it("neutralises injected markup in a streamed assistant reply", async () => {
    await streamAssistantText(
      "Summarise",
      'Careful: <img src=x onerror="alert(1)"> and <script>alert(2)</script> and [x](javascript:alert(3)).',
    );
    const assistant = await waitFor(() => {
      const node = document.querySelector(".chat-message.assistant .markdown-body");
      expect(node).not.toBeNull();
      return node as HTMLElement;
    });
    expect(assistant.querySelector("img, script")).toBeNull();
    expect(assistant.querySelector("a")).toBeNull();
    expect(
      assistant.querySelector(".markdown-link-blocked"),
    ).toBeInTheDocument();
    expect(assistant.innerHTML).not.toMatch(/<script/i);
    expect(assistant.innerHTML).not.toMatch(/onerror/i);
  });

  it("renders a streamed fixture turn as collapsible tool cards without inventing arguments", async () => {
    render(<App />);
    await waitFor(() => expect(listeners.has("runtime-event")).toBe(true));
    fireEvent.change(screen.getByRole("textbox", { name: "Message JARVIS" }), {
      target: { value: "Search the repo" },
    });
    fireEvent.click(screen.getByRole("button", { name: "↑" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "chat_send",
        expect.objectContaining({ text: "Search the repo" }),
      ),
    );
    const emit = (
      type: string,
      payload: Record<string, unknown>,
      at: string,
    ) =>
      act(() =>
        listeners.get("runtime-event")?.({
          payload: runtimeEvent(type, payload, at, `${type}-${at}`),
        }),
      );
    emit("reasoning.available", { assistantMessageID: "m1" }, "2026-08-30T12:00:00.000Z");
    emit(
      "tool.started",
      { callID: "call_1", tool: "grep", status: "running" },
      "2026-08-30T12:00:00.000Z",
    );
    emit(
      "tool.completed",
      { callID: "call_1", tool: "grep", status: "completed" },
      "2026-08-30T12:00:01.250Z",
    );
    emit(
      "tool.started",
      { callID: "call_2", tool: "read", status: "running" },
      "2026-08-30T12:00:02.000Z",
    );

    const activity = await screen.findByRole("region", {
      name: "Tool activity",
    });
    expect(within(activity).getByText("grep")).toBeInTheDocument();
    expect(within(activity).getByText("read")).toBeInTheDocument();
    expect(within(activity).getByText("completed")).toBeInTheDocument();
    expect(within(activity).getByText("1.25 s")).toBeInTheDocument();
    expect(within(activity).getByText("running")).toBeInTheDocument();
    expect(within(activity).getByText("in progress")).toBeInTheDocument();
    expect(
      activity.querySelectorAll("details.tool-card"),
    ).toHaveLength(2);
    // fallbackConfig has show_tool_details on, but the sidecar boundary keeps
    // no arguments, so the card must say so rather than fabricate them.
    expect(
      within(activity).getAllByText(
        /Arguments and results are discarded at the runtime boundary/,
      ),
    ).toHaveLength(2);
    expect(
      within(activity).getByText(/Reasoning available/),
    ).toBeInTheDocument();
  });

  it("hides tool details and the reasoning indicator when the settings are off", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "bootstrap")
        return Promise.resolve({
          config: {
            ...fallbackConfig,
            ui: {
              ...fallbackConfig.ui,
              show_tool_details: false,
              show_reasoning: false,
            },
          },
          projection,
          history: [],
          voice: {
            stt_ready: false,
            tts_ready: false,
            playback_ready: false,
            details: [],
          },
        });
      return baseInvoke(command);
    });
    render(<App />);
    await waitFor(() => expect(listeners.has("runtime-event")).toBe(true));
    fireEvent.change(screen.getByRole("textbox", { name: "Message JARVIS" }), {
      target: { value: "Search the repo" },
    });
    fireEvent.click(screen.getByRole("button", { name: "↑" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "chat_send",
        expect.objectContaining({ text: "Search the repo" }),
      ),
    );
    act(() =>
      listeners.get("runtime-event")?.({
        payload: runtimeEvent("reasoning.available", { assistantMessageID: "m1" }),
      }),
    );
    act(() =>
      listeners.get("runtime-event")?.({
        payload: runtimeEvent(
          "tool.started",
          { callID: "call_1", tool: "grep", status: "running" },
          "2026-08-30T12:00:00.000Z",
        ),
      }),
    );
    const activity = await screen.findByRole("region", {
      name: "Tool activity",
    });
    expect(within(activity).getByText("grep")).toBeInTheDocument();
    expect(
      within(activity).queryByText(
        /Arguments and results are discarded at the runtime boundary/,
      ),
    ).toBeNull();
    expect(within(activity).queryByText(/Reasoning available/)).toBeNull();
  });

  it("creates a session before running a slash command typed with none active", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "session_action")
        return Promise.resolve({ session_id: "ses_created" });
      if (command === "runtime_operation")
        return Promise.resolve({
          info: { id: "msg_command", role: "assistant" },
          parts: [{ type: "text", text: "Command output." }],
        });
      return baseInvoke(command);
    });
    render(<App />);
    const attach = screen.getByLabelText("Attach files") as HTMLInputElement;
    fireEvent.change(attach, {
      target: {
        files: [new File(["hello"], "notes.txt", { type: "text/plain" })],
      },
    });
    expect(
      await screen.findByRole("button", { name: "Remove notes.txt" }),
    ).toBeInTheDocument();

    fireEvent.change(screen.getByRole("textbox", { name: "Message JARVIS" }), {
      target: { value: "/compact keep the plan" },
    });
    fireEvent.click(screen.getByRole("button", { name: "↑" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("session_action", {
        action: "new",
        sessionId: null,
        directory: fallbackConfig.runtime.working_directory,
        title: null,
        confirmed: false,
      }),
    );
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "runtime_operation",
        expect.objectContaining({
          kind: "session_command",
          sessionId: "ses_created",
          payload: expect.objectContaining({
            command: "compact",
            arguments: "keep the plan",
          }),
        }),
      ),
    );
    expect(
      invoke.mock.calls.some(([command]) => command === "chat_send"),
    ).toBe(false);
    expect(await screen.findByText("Command output.")).toBeInTheDocument();
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Remove notes.txt" }),
      ).toBeNull(),
    );
  });

  it("states only measured platform, profile and pipeline facts in the shell chrome", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "microphone_state") return Promise.resolve(projection);
      return baseInvoke(command);
    });
    render(<App />);
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("startup_window_painted"),
    );
    expect(await screen.findByText("TEST · TEST")).toBeInTheDocument();
    expect(screen.getByText("ANALYTICS OFF")).toBeInTheDocument();
    expect(screen.queryByText("PRIVATE MODE")).toBeNull();
    expect(screen.queryByText("LINUX · X86_64")).toBeNull();
    expect(screen.queryByText("Studio profile")).toBeNull();
    expect(screen.queryByText("⌘K")).toBeNull();
    expect(screen.getByText("default")).toBeInTheDocument();
    expect(screen.getByText("Personal Agent v0.1.0")).toBeInTheDocument();
    expect(document.querySelector(".nav-heading b")).toBeNull();
    expect(screen.queryByText("Balanced")).toBeNull();
    expect(screen.getByText("missing → missing")).toBeInTheDocument();
  });

  describe("voice barge-in", () => {
    class MockWorkletNode {
      static instances: MockWorkletNode[] = [];
      readonly connect = vi.fn();
      readonly disconnect = vi.fn();
      readonly port = {
        onmessage: null as ((event: MessageEvent<Float32Array>) => void) | null,
      };

      constructor() {
        MockWorkletNode.instances.push(this);
      }

      emit(frame: Float32Array) {
        this.port.onmessage?.({ data: frame } as MessageEvent<Float32Array>);
      }
    }

    class MockAudioContext {
      readonly sampleRate = 16_000;
      readonly destination = {} as AudioDestinationNode;
      readonly audioWorklet = {
        addModule: vi.fn(async () => undefined),
      } as unknown as AudioWorklet;
      readonly close = vi.fn(async () => undefined);
      readonly resume = vi.fn(async () => undefined);
      readonly createMediaStreamSource = vi.fn(() => ({
        connect: vi.fn(),
        disconnect: vi.fn(),
      }));
      readonly createGain = vi.fn(() => ({
        gain: { value: 1 },
        connect: vi.fn(),
      }));
      readonly createScriptProcessor = vi.fn();
    }

    const trackStop = vi.fn();
    const getUserMedia = vi.fn();
    const track = {
      stop: trackStop,
      applyConstraints: vi.fn(async () => undefined),
      getSettings: () => ({ echoCancellation: true }),
    };
    const voiceReadyConfig = {
      ...fallbackConfig,
      voice: { ...fallbackConfig.voice, wake_enabled: true, barge_in: true },
    };
    const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

    beforeEach(() => {
      MockWorkletNode.instances = [];
      trackStop.mockClear();
      track.applyConstraints.mockClear();
      getUserMedia.mockReset().mockResolvedValue({
        getTracks: () => [track],
        getAudioTracks: () => [track],
      });
      vi.stubGlobal("AudioContext", MockAudioContext);
      vi.stubGlobal("AudioWorkletNode", MockWorkletNode);
      Object.defineProperty(navigator, "mediaDevices", {
        configurable: true,
        value: { getUserMedia },
      });
      invoke.mockImplementation((command: string) => {
        if (command === "bootstrap")
          return Promise.resolve({
            config: voiceReadyConfig,
            projection,
            history: [],
            voice: {
              stt_ready: true,
              tts_ready: true,
              playback_ready: true,
              details: [],
            },
          });
        if (command === "voice_wake_start")
          return Promise.resolve({ fallback: "stt-match" });
        if (command === "voice_wake_chunk")
          return Promise.resolve({
            wake: false,
            score: 0,
            fallback: "stt-match",
            speech_prob: 0.95,
          });
        if (command === "voice_stream_start")
          return Promise.resolve({ streaming: true });
        if (command === "voice_stream_chunk")
          return Promise.resolve({ speech_prob: 0.8 });
        if (command === "microphone_state") return Promise.resolve(projection);
        return baseInvoke(command);
      });
    });

    afterEach(() => {
      vi.unstubAllGlobals();
      Object.defineProperty(navigator, "mediaDevices", {
        configurable: true,
        value: undefined,
      });
    });

    it("keeps wake capture live through playback and stops the sink on speech", async () => {
      render(<App />);
      await waitFor(
        () => expect(invoke).toHaveBeenCalledWith("voice_wake_start"),
        { timeout: 3_000 },
      );
      await waitFor(() => expect(MockWorkletNode.instances).toHaveLength(1));

      await act(async () => {
        listeners.get("voice-state")?.({
          payload: { state: "speaking", engine: "qwen3-tts" },
        });
        await flush();
      });

      // The wake stream survives playback instead of being suspended, and the
      // rail card reports the measured capability rather than the config flag.
      expect(trackStop).not.toHaveBeenCalled();
      expect(track.applyConstraints).toHaveBeenCalledWith({
        echoCancellation: true,
      });
      const bargeInRow = screen.getByText("Barge-in").closest("div");
      expect(bargeInRow).not.toBeNull();
      await waitFor(() =>
        expect(
          within(bargeInRow as HTMLElement).getByText("listening · AEC on"),
        ).toBeInTheDocument(),
      );
      // Chrome status and rail card both render the measured capability.
      expect(
        screen.getAllByText("qwen3-tts · barge-in listening · AEC on"),
      ).toHaveLength(2);

      const node = MockWorkletNode.instances[0];
      await act(async () => {
        for (let frame = 0; frame < 20; frame += 1)
          node?.emit(new Float32Array(320).fill(0.3));
        await flush();
      });

      await waitFor(() => expect(invoke).toHaveBeenCalledWith("voice_stop"));
      await waitFor(() =>
        expect(
          screen.getByRole("button", { name: /^Listening\./ }),
        ).toBeInTheDocument(),
      );
    });
  });
});
