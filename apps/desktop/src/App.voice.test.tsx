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

describe("voice input routing", () => {
  beforeEach(() => {
    voiceCallbacks.final = null;
    voiceCallbacks.partial = null;
    listen.mockReset();
    listen.mockResolvedValue(() => undefined);
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
});
