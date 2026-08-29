import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { VoiceTranscriptMeta } from "./useVoiceCapture";

const invoke = vi.hoisted(() => vi.fn());
const listen = vi.hoisted(() => vi.fn());
const voiceCallbacks = vi.hoisted(() => ({
  final: null as null | ((text: string, meta: VoiceTranscriptMeta) => void),
  partial: null as null | ((text: string, meta: VoiceTranscriptMeta) => void),
}));
const eventCallbacks = vi.hoisted(
  () =>
    ({}) as Record<
      string,
      (event: { payload: Record<string, unknown> }) => void
    >,
);

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
vi.mock("./useVoiceCapture", () => ({
  useVoiceCapture: (
    _config: unknown,
    onFinal: (text: string, meta: VoiceTranscriptMeta) => void,
    _onProjection: unknown,
    onPartial: (text: string, meta: VoiceTranscriptMeta) => void,
  ) => {
    voiceCallbacks.final = onFinal;
    voiceCallbacks.partial = onPartial;
    return {
      state: "idle",
      error: "",
      level: 0,
      partialTranscript: "",
      start: vi.fn(async () => undefined),
      stop: vi.fn(async () => undefined),
      cancel: vi.fn(),
      armWake: vi.fn(async () => undefined),
      wakeArmed: false,
    };
  },
}));

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

const meta: VoiceTranscriptMeta = {
  final: true,
  source: "capture",
  audioEndMs: 100,
};

const nativeContract = {
  platform: "linux",
  session: "wayland",
  adapter: "wtype",
  availability: "degraded",
  review_before_insert: true,
  supports_text_insertion: true,
  supports_live_revisions: true,
  supports_verified_edits: false,
  detail: "Wayland text insertion is available but unverified.",
  remediation: "Verify the destination field visually.",
};

const nativeTarget = {
  application_id: "code",
  title: "Project notes",
  window_id: "0xabc",
  secure: false,
};

const nativeStatus = (pending: Record<string, unknown> | null = null) => ({
  contract: nativeContract,
  armed_target: nativeTarget,
  pending,
  undo_available: false,
  metrics: { apply_samples: 0 },
});

describe("voice input routing", () => {
  beforeEach(() => {
    voiceCallbacks.final = null;
    voiceCallbacks.partial = null;
    for (const name of Object.keys(eventCallbacks)) delete eventCallbacks[name];
    listen.mockReset();
    listen.mockImplementation(
      (name: string, callback: (event: { payload: Record<string, unknown> }) => void) => {
        eventCallbacks[name] = callback;
        return Promise.resolve(() => undefined);
      },
    );
    invoke.mockReset();
    invoke.mockImplementation((command: string, payload?: Record<string, unknown>) => {
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
      if (command === "diagnostics")
        return Promise.resolve({
          product: "Personal Agent",
          version: "0.1.0",
          platform: "test",
          arch: "test",
          opencode: { pinned: "1.18.23", topology: "test" },
          capabilities: [],
        });
      if (command === "voice_route") {
        const transcript = String(payload?.transcript ?? "");
        if (transcript === "start dictation")
          return Promise.resolve({
            route: "commands",
            commands: [{ kind: "start_dictation" }],
          });
        if (payload?.context === "dictation")
          return Promise.resolve({ route: "dictation", text: transcript });
        return Promise.resolve({ route: "agent_goal", prompt: transcript });
      }
      if (command === "dictation_reset") return Promise.resolve();
      if (command === "dictation_apply")
        return Promise.resolve({ receipts: [], rejected: [] });
      if (command === "dictation_ingest") {
        const event = payload?.event as {
          text: string;
          final_result: boolean;
        };
        if (!event.final_result)
          return Promise.resolve({
            transaction_id: 1,
            mode: "natural",
            tokens: [],
            rendered_text: event.text,
            final_result: false,
            operations: [
              {
                kind: "replace_provisional_tail",
                transaction_id: 1,
                retain_utf16: 0,
                delete_utf16: 0,
                insert: event.text,
                stable_prefix_utf16: 0,
              },
            ],
          });
        return Promise.resolve({
          transaction_id: 1,
          mode: "natural",
          tokens: [],
          rendered_text: event.text,
          final_result: true,
          operations: [
            {
              kind: "replace_provisional_tail",
              transaction_id: 1,
              retain_utf16: 7,
              delete_utf16: 2,
              insert: "orld",
              stable_prefix_utf16: 6,
            },
            { kind: "commit_transaction", transaction_id: 1 },
          ],
        });
      }
      if (command === "native_dictation_status")
        return Promise.resolve({ ...nativeStatus(), armed_target: null });
      if (command === "native_dictation_arm")
        return Promise.resolve(nativeStatus());
      if (command === "native_dictation_disarm" || command === "native_dictation_discard")
        return Promise.resolve({ ...nativeStatus(), armed_target: null });
      if (command === "native_dictation_stage") {
        const update = payload?.update as { rendered_text: string; final_result: boolean };
        return Promise.resolve(
          nativeStatus({
            transaction_id: 1,
            text: update.rendered_text,
            final_result: update.final_result,
            kind: "insert",
            preview_latency_ms: 1,
          }),
        );
      }
      if (command === "native_dictation_confirm")
        return Promise.resolve({
          submitted: true,
          verified: false,
          adapter: "wtype",
          elapsed_ms: 2,
          detail: "Verify visually",
          status: nativeStatus(),
        });
      if (command === "chat_send")
        return Promise.resolve({
          session_id: "ses_voice",
          message_id: "msg_voice",
          projection,
        });
      if (command === "autostart_status") return Promise.resolve(false);
      return Promise.resolve({});
    });
  });

  afterEach(() => cleanup());

  it("dictates partial revisions into the composer without sending chat", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Dictation" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("dictation_reset", { mode: "natural" }),
    );

    act(() =>
      voiceCallbacks.partial?.("hello wur", {
        ...meta,
        final: false,
      }),
    );
    await waitFor(() =>
      expect(screen.getByRole("textbox", { name: "Message JARVIS" })).toHaveValue(
        "hello wur",
      ),
    );
    act(() => voiceCallbacks.final?.("hello world", meta));
    await waitFor(() =>
      expect(screen.getByRole("textbox", { name: "Message JARVIS" })).toHaveValue(
        "hello world",
      ),
    );
    expect(invoke).not.toHaveBeenCalledWith("chat_send", expect.anything());

    fireEvent.click(screen.getByRole("button", { name: "↑" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "chat_send",
        expect.objectContaining({ text: "hello world" }),
      ),
    );
  });

  it("routes agent speech once even if a final callback is duplicated", async () => {
    render(<App />);
    await screen.findByRole("button", { name: "Agent" });
    act(() => {
      voiceCallbacks.final?.("inspect this project", meta);
      voiceCallbacks.final?.("inspect this project", meta);
    });
    await waitFor(() =>
      expect(
        invoke.mock.calls.filter(([command]) => command === "chat_send"),
      ).toHaveLength(1),
    );
    expect(invoke).toHaveBeenCalledWith(
      "chat_send",
      expect.objectContaining({
        text: "inspect this project",
        speakResponse: true,
      }),
    );
  });

  it("refreshes pending runtime approvals before a tool turn completes", async () => {
    render(<App />);
    await screen.findByRole("button", { name: "Agent" });
    act(() => voiceCallbacks.final?.("run the reviewed checks", meta));
    await waitFor(() => expect(eventCallbacks["runtime-event"]).toBeTypeOf("function"));

    act(() =>
      eventCallbacks["runtime-event"]!({
        payload: {
          schemaVersion: 1,
          eventId: "evt-approval",
          wallClockTimestamp: new Date().toISOString(),
          monotonicSequence: 1,
          origin: "opencode",
          profileId: "default",
          sessionId: "ses_voice",
          type: "approval.requested",
          payloadJson: JSON.stringify({
            id: "perm-1",
            permission: "bash",
          }),
        },
      }),
    );

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("runtime_catalog", {
        directory: fallbackConfig.runtime.working_directory,
      }),
    );
    expect(screen.getByText("Waiting for your approval")).toBeInTheDocument();
  });

  it("handles the deterministic start-dictation command without a model turn", async () => {
    render(<App />);
    act(() => voiceCallbacks.final?.("start dictation", meta));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Dictation" })).toHaveAttribute(
        "aria-pressed",
        "true",
      ),
    );
    expect(invoke).not.toHaveBeenCalledWith("chat_send", expect.anything());
  });

  it("reviews focused-app dictation and requires an explicit delayed apply", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Dictation" }));
    fireEvent.click(screen.getByRole("button", { name: "Focused app" }));
    await screen.findByRole("button", { name: "Arm in 3 seconds" });
    fireEvent.click(screen.getByRole("button", { name: "Arm in 3 seconds" }));
    await screen.findByText(/Armed: Project notes/);

    act(() => voiceCallbacks.final?.("hello world", meta));
    await screen.findByText("hello world");
    expect(invoke).not.toHaveBeenCalledWith("chat_send", expect.anything());

    fireEvent.click(screen.getByRole("button", { name: "Apply in 3s · code" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("native_dictation_confirm", {
        confirmed: true,
        delayMs: 2_500,
      }),
    );
  });
});
