import "@testing-library/jest-dom/vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
const listen = vi.hoisted(() => vi.fn());
const listeners = vi.hoisted(
  () => new Map<string, (event: { payload: unknown }) => void>(),
);

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

import { ChatView, fallbackConfig, type ChatMessage } from "./App";
import type { Projection, VoiceStatus } from "./types";

const projection: Projection = {
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

const voiceStatus: VoiceStatus = {
  stt_ready: false,
  tts_ready: true,
  playback_ready: false,
  configured_stt_backend: "moonshine",
  configured_tts_backend: "qwen3-tts",
  active_stt_backend: "moonshine",
  active_tts_backend: "qwen3-tts",
  degraded: false,
  neural_runtime_ready: false,
  moonshine_ready: false,
  smart_turn_ready: false,
  qwen_ready: false,
  details: [],
};

const noop = () => undefined;

function ChatHarness({
  initialMessages,
  onMessageProfile,
}: {
  initialMessages: ChatMessage[];
  onMessageProfile?: React.ProfilerProps["onRender"];
}) {
  const [messages, setMessages] = useState(initialMessages);
  return (
    <ChatView
      config={fallbackConfig}
      catalog={{}}
      projection={projection}
      voiceStatus={voiceStatus}
      model=""
      setModel={noop}
      messages={messages}
      setMessages={setMessages}
      activeSession="session-performance"
      setActiveSession={noop}
      onProjection={noop}
      onHistory={noop}
      onCatalog={noop}
      onVoice={noop}
      onOpenProviders={noop}
      onVoicePresentation={noop}
      onMessageProfile={onMessageProfile}
    />
  );
}

function deltaEvent(index: number, delta = "x") {
  return {
    schema_version: 1,
    event_id: `evt-delta-${index}`,
    wall_clock_timestamp: "2026-08-30T00:00:00Z",
    monotonic_sequence: index,
    origin: "performance-test",
    profile_id: "default",
    type: "response.delta",
    payload_json: Array.from(
      new TextEncoder().encode(JSON.stringify({ delta })),
    ),
  };
}

describe("streaming transcript render hygiene", () => {
  const frames = new Map<number, FrameRequestCallback>();
  let nextFrame = 0;

  beforeEach(() => {
    listeners.clear();
    invoke.mockReset().mockImplementation((command: string) => {
      if (command === "microphone_state") return Promise.resolve(projection);
      if (command === "runtime_catalog") return Promise.resolve({});
      return Promise.resolve({});
    });
    listen
      .mockReset()
      .mockImplementation(
        (name: string, callback: (event: { payload: unknown }) => void) => {
          listeners.set(name, callback);
          return Promise.resolve(() => listeners.delete(name));
        },
      );
    frames.clear();
    nextFrame = 0;
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        nextFrame += 1;
        frames.set(nextFrame, callback);
        return nextFrame;
      }),
    );
    vi.stubGlobal(
      "cancelAnimationFrame",
      vi.fn((frame: number) => frames.delete(frame)),
    );
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  const flushFrames = () => {
    const pending = [...frames.entries()];
    frames.clear();
    for (const [, callback] of pending) callback(performance.now());
  };

  it("coalesces 500 deltas, isolates completed rows, windows, and stays pinned", async () => {
    const initialMessages: ChatMessage[] = Array.from(
      { length: 205 },
      (_, index) => ({
        id: `stable-${index}`,
        role: index % 2 === 0 ? "assistant" : "user",
        text: `Completed message ${index}`,
        revision: 0,
      }),
    );
    initialMessages.push({
      id: "streaming",
      role: "assistant",
      text: "",
      revision: 0,
      streaming: true,
    });
    const profiledRenders = vi.fn();

    render(
      <ChatHarness
        initialMessages={initialMessages}
        onMessageProfile={profiledRenders}
      />,
    );
    await waitFor(() => expect(listeners.has("runtime-event")).toBe(true));

    const transcript = document.querySelector(".messages") as HTMLDivElement;
    const scrollTo = vi.fn();
    transcript.scrollTo = scrollTo;
    Object.defineProperty(transcript, "scrollHeight", {
      configurable: true,
      get: () => 42_000,
    });
    expect(transcript.querySelectorAll(".chat-message")).toHaveLength(200);
    expect(screen.getByTestId("transcript-window-spacer")).toHaveStyle({
      height: "528px",
    });

    for (let batch = 0; batch < 5; batch += 1) {
      await act(async () => {
        for (let offset = 0; offset < 100; offset += 1) {
          const index = batch * 100 + offset;
          listeners.get("runtime-event")?.({ payload: deltaEvent(index) });
        }
      });
      expect(frames).toHaveLength(1);
      act(flushFrames);
    }

    expect(screen.getByText("x".repeat(500))).toBeInTheDocument();
    expect(
      document.querySelector('[data-message-id="streaming"]'),
    ).toHaveAttribute("data-message-revision", "5");
    const stableRowRenders = profiledRenders.mock.calls.filter(
      ([id]) => id === "message:stable-204",
    );
    expect(stableRowRenders).toHaveLength(1);
    expect(stableRowRenders.length).toBeLessThanOrEqual(Math.ceil(500 / 100));
    expect(scrollTo).toHaveBeenLastCalledWith({
      top: 42_000,
      behavior: "smooth",
    });
  });

  it("flushes a queued delta before completion and speaks the accumulated text", async () => {
    render(
      <ChatHarness
        initialMessages={[
          {
            id: "streaming",
            role: "assistant",
            text: "",
            revision: 0,
            streaming: true,
          },
        ]}
      />,
    );
    await waitFor(() =>
      expect(listeners.has("runtime-turn-complete")).toBe(true),
    );

    act(() => {
      listeners.get("runtime-event")?.({
        payload: deltaEvent(1, "final tail"),
      });
    });
    expect(frames).toHaveLength(1);
    act(() => {
      listeners.get("runtime-turn-complete")?.({
        payload: { text: "", speak: true, status: "completed" },
      });
    });

    expect(frames).toHaveLength(0);
    expect(screen.getByText("final tail")).toBeInTheDocument();
    expect(document.querySelector('[data-message-id="streaming"]')).toBeNull();
    expect(invoke).toHaveBeenCalledWith("voice_speak", { text: "final tail" });

    act(() => {
      listeners.get("runtime-event")?.({
        payload: deltaEvent(2, "late delta"),
      });
    });
    expect(frames).toHaveLength(0);
    expect(screen.queryByText("late delta")).not.toBeInTheDocument();
  });
});
