import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import {
  advanceVadEndpoint,
  bargeInWakeThresholdMilli,
  describeBargeIn,
  matchWakePhrase,
  passesVoicePreGate,
  sendVoiceStreamChunk,
  sendWakeStreamChunk,
  useVoiceCapture,
  vadProbabilityThreshold,
  type VoicePlaybackContext,
} from "./useVoiceCapture";
import type { AppConfig, Projection } from "./types";

const projection = {} as Projection;

function voiceConfig(
  wakeEnabled = false,
  overrides: Record<string, unknown> = {},
) {
  return {
    voice: {
      enabled: true,
      mode: "push-to-talk",
      input_device: "",
      input_gain_percent: 100,
      wake_phrases: ["hey jarvis", "jarvis"],
      wake_threshold_milli: 930,
      vad_start_milli: 600,
      vad_stop_milli: 350,
      pre_roll_ms: 300,
      refractory_ms: 1_000,
      wake_enabled: wakeEnabled,
      stt_backend: "moonshine",
      echo_cancellation: true,
      noise_suppression: true,
      automatic_gain_control: true,
      barge_in: true,
      ...overrides,
    },
  } as unknown as AppConfig;
}

const addModule = vi.fn<() => Promise<void>>();
const getUserMedia = vi.fn();
const trackStop = vi.fn();
const applyConstraints = vi.fn<(constraints: MediaTrackConstraints) => void>();
const sourceConnect = vi.fn();
const sourceDisconnect = vi.fn();

class MockWorkletNode {
  static instances: MockWorkletNode[] = [];

  readonly connect = vi.fn();
  readonly disconnect = vi.fn();
  readonly port = {
    onmessage: null as ((event: MessageEvent<Float32Array>) => void) | null,
  };
  readonly name: string;
  readonly options: AudioWorkletNodeOptions;

  constructor(
    _context: AudioContext,
    name: string,
    options: AudioWorkletNodeOptions,
  ) {
    this.name = name;
    this.options = options;
    MockWorkletNode.instances.push(this);
  }

  emit(frame: Float32Array) {
    this.port.onmessage?.({ data: frame } as MessageEvent<Float32Array>);
  }
}

class MockScriptProcessorNode {
  readonly connect = vi.fn();
  readonly disconnect = vi.fn();
  onaudioprocess: ((event: AudioProcessingEvent) => void) | null = null;

  emit(samples: Float32Array) {
    this.onaudioprocess?.({
      inputBuffer: { getChannelData: () => samples },
    } as unknown as AudioProcessingEvent);
  }
}

class MockAudioContext {
  static instances: MockAudioContext[] = [];

  readonly sampleRate = 48_000;
  readonly destination = {} as AudioDestinationNode;
  readonly audioWorklet = { addModule } as unknown as AudioWorklet;
  readonly close = vi.fn(async () => undefined);
  readonly resume = vi.fn(async () => undefined);
  readonly scriptProcessor = new MockScriptProcessorNode();
  readonly createScriptProcessor = vi.fn(() => this.scriptProcessor);
  readonly createMediaStreamSource = vi.fn(() => ({
    connect: sourceConnect,
    disconnect: sourceDisconnect,
  }));
  readonly createGain = vi.fn(() => ({
    gain: { value: 1 },
    connect: vi.fn(),
  }));

  constructor() {
    MockAudioContext.instances.push(this);
  }
}

/**
 * Mirror a real capture track: the browser reports what AEC it actually
 * granted, and `undefined` models a webview that exposes no setting at all.
 */
function installCaptureMocks(
  workletSupported = true,
  echoCancellation: boolean | undefined = undefined,
) {
  MockWorkletNode.instances = [];
  MockAudioContext.instances = [];
  addModule.mockReset().mockResolvedValue(undefined);
  applyConstraints.mockReset();
  const track = {
    stop: trackStop,
    applyConstraints: (constraints: MediaTrackConstraints) => {
      applyConstraints(constraints);
      return Promise.resolve();
    },
    getSettings: () => ({ echoCancellation }),
  };
  getUserMedia.mockReset().mockResolvedValue({
    getTracks: () => [track],
    getAudioTracks: () => [track],
  });
  trackStop.mockReset();
  sourceConnect.mockReset();
  sourceDisconnect.mockReset();
  vi.stubGlobal("AudioContext", MockAudioContext);
  vi.stubGlobal(
    "AudioWorkletNode",
    workletSupported ? MockWorkletNode : undefined,
  );
  vi.stubGlobal("navigator", {
    mediaDevices: { getUserMedia },
  });
}

function installVoiceInvoke() {
  invoke.mockImplementation((command: string) => {
    if (command === "voice_stream_start")
      return Promise.resolve({ streaming: true });
    if (command === "voice_stream_chunk")
      return Promise.resolve({ speech_prob: 0.8 });
    if (command === "voice_wake_start") return Promise.resolve({});
    if (command === "voice_wake_chunk")
      return Promise.resolve({ wake: false, score: 0.01 });
    if (command === "microphone_state") return Promise.resolve(projection);
    return Promise.resolve({});
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("binary voice streaming", () => {
  beforeEach(() => invoke.mockReset());

  it("sends little-endian PCM16 as an ArrayBuffer without per-sample JSON", async () => {
    invoke.mockResolvedValue({ text: "partial" });

    await sendVoiceStreamChunk(new Float32Array([-1, -0.5, 0, 0.5, 1]));

    expect(invoke).toHaveBeenCalledOnce();
    const [command, body] = invoke.mock.calls[0] ?? [];
    expect(command).toBe("voice_stream_chunk");
    expect(body).toBeInstanceOf(ArrayBuffer);
    expect(Array.isArray(body)).toBe(false);
    const view = new DataView(body as ArrayBuffer);
    expect(
      Array.from({ length: view.byteLength / 2 }, (_, index) =>
        view.getInt16(index * 2, true),
      ),
    ).toEqual([-32_768, -16_384, 0, 16_384, 32_767]);
  });

  it("routes ambient wake frames through the raw wake command", async () => {
    invoke.mockResolvedValue({ wake: false, score: 0.02 });

    await sendWakeStreamChunk(new Float32Array([-1, 0, 1]));

    expect(invoke).toHaveBeenCalledOnce();
    const [command, body] = invoke.mock.calls[0] ?? [];
    expect(command).toBe("voice_wake_chunk");
    expect(body).toBeInstanceOf(ArrayBuffer);
    expect(Array.from(new Int16Array(body as ArrayBuffer))).toEqual([
      -32_768, 0, 32_767,
    ]);
  });
});

describe("wake phrase matching", () => {
  it("matches configured wake words despite punctuation and returns the command", () => {
    expect(
      matchWakePhrase("Hey, JARVIS! What's on my calendar?", [
        "hey jarvis",
        "jarvis",
      ]),
    ).toEqual({
      phrase: "hey jarvis",
      remainder: "What's on my calendar?",
    });
  });

  it("prefers the longest configured phrase", () => {
    expect(matchWakePhrase("Hey Jarvis", ["jarvis", "hey jarvis"])).toEqual({
      phrase: "hey jarvis",
      remainder: "",
    });
  });

  it("does not trigger on partial words", () => {
    expect(matchWakePhrase("Ask Jarvison to help", ["jarvis"])).toBeNull();
  });

  it("preserves proper names and punctuation after the wake phrase", () => {
    expect(
      matchWakePhrase("Hey Jarvis, email Yuval about GitHub.", ["hey jarvis"]),
    ).toEqual({
      phrase: "hey jarvis",
      remainder: "email Yuval about GitHub.",
    });
  });
});

describe("Silero endpoint thresholds", () => {
  it("treats VAD config millivals as probabilities, never durations", () => {
    expect(vadProbabilityThreshold(600)).toBe(0.6);
    expect(vadProbabilityThreshold(350)).toBe(0.35);
    expect(vadProbabilityThreshold(-10)).toBe(0);
    expect(vadProbabilityThreshold(1_500)).toBe(1);
  });

  it("uses the frontend RMS check only as a digital-silence pre-gate", () => {
    expect(passesVoicePreGate(0)).toBe(false);
    expect(passesVoicePreGate(0.00049)).toBe(false);
    expect(passesVoicePreGate(0.0005)).toBe(true);
    expect(passesVoicePreGate(Number.NaN)).toBe(false);
  });

  it("consults Smart Turn once per Silero silence episode", () => {
    const initial = {
      speechStarted: false,
      semanticConsultedForSilence: false,
    };
    const speech = advanceVadEndpoint(initial, 0.82, 600, 350);
    expect(speech.consultSmartTurn).toBe(false);
    const firstSilence = advanceVadEndpoint(speech.state, 0.2, 600, 350);
    expect(firstSilence.consultSmartTurn).toBe(true);
    const sameSilence = advanceVadEndpoint(firstSilence.state, 0.1, 600, 350);
    expect(sameSilence.consultSmartTurn).toBe(false);
    const resumedSpeech = advanceVadEndpoint(sameSilence.state, 0.75, 600, 350);
    expect(
      advanceVadEndpoint(resumedSpeech.state, 0.15, 600, 350).consultSmartTurn,
    ).toBe(true);
  });
});

describe("AudioWorklet capture integration", () => {
  beforeEach(() => {
    invoke.mockReset();
    installCaptureMocks();
    installVoiceInvoke();
  });

  it("streams each active-capture worklet frame over binary IPC", async () => {
    const onTranscript = vi.fn();
    const onProjection = vi.fn();
    const hook = renderHook(() =>
      useVoiceCapture(
        voiceConfig(),
        onTranscript,
        onProjection,
        undefined,
        false,
      ),
    );

    await act(async () => hook.result.current.start());

    expect(hook.result.current.state).toBe("listening");
    expect(addModule).toHaveBeenCalledOnce();
    expect(MockWorkletNode.instances).toHaveLength(1);
    const node = MockWorkletNode.instances[0];
    expect(node?.name).toBe("personal-agent-voice-capture");
    expect(node?.options.processorOptions).toEqual({ gain: 1 });

    act(() => node?.emit(new Float32Array(320).fill(0.25)));
    await waitFor(() =>
      expect(
        invoke.mock.calls.some(([command]) => command === "voice_stream_chunk"),
      ).toBe(true),
    );
    const chunkCall = invoke.mock.calls.find(
      ([command]) => command === "voice_stream_chunk",
    );
    expect(chunkCall?.[1]).toBeInstanceOf(ArrayBuffer);
    expect((chunkCall?.[1] as ArrayBuffer).byteLength).toBe(640);

    act(() => hook.unmount());
    expect(node?.port.onmessage).toBeNull();
    expect(node?.disconnect).toHaveBeenCalled();
  });

  it("coalesces audio-meter state writes to the newest worklet frame per paint", async () => {
    const frames = new Map<number, FrameRequestCallback>();
    let nextFrame = 0;
    const requestFrame = vi.fn((callback: FrameRequestCallback) => {
      nextFrame += 1;
      frames.set(nextFrame, callback);
      return nextFrame;
    });
    const cancelFrame = vi.fn((frame: number) => frames.delete(frame));
    vi.stubGlobal("requestAnimationFrame", requestFrame);
    vi.stubGlobal("cancelAnimationFrame", cancelFrame);
    const config = voiceConfig();
    const onTranscript = vi.fn();
    const onProjection = vi.fn();
    const hook = renderHook(() =>
      useVoiceCapture(config, onTranscript, onProjection, undefined, false),
    );

    await act(async () => hook.result.current.start());
    const node = MockWorkletNode.instances[0];
    expect(node).toBeDefined();
    act(() => {
      node?.emit(new Float32Array(320).fill(0.025));
      node?.emit(new Float32Array(320).fill(0.075));
    });

    expect(requestFrame).toHaveBeenCalledOnce();
    expect(hook.result.current.level).toBe(0);
    const [firstFrame, paint] = [...frames.entries()][0] ?? [];
    expect(firstFrame).toBe(1);
    act(() => {
      frames.delete(firstFrame!);
      paint?.(performance.now());
    });
    expect(hook.result.current.level).toBeCloseTo(0.6);

    act(() => node?.emit(new Float32Array(320).fill(0.05)));
    expect(requestFrame).toHaveBeenCalledTimes(2);
    const pendingFrame = [...frames.keys()][0];
    act(() => hook.unmount());
    expect(cancelFrame).toHaveBeenCalledWith(pendingFrame);
  });

  it("uses the same 16 kHz binary worklet frames for wake capture", async () => {
    const onTranscript = vi.fn();
    const onProjection = vi.fn();
    const hook = renderHook(() =>
      useVoiceCapture(
        voiceConfig(true),
        onTranscript,
        onProjection,
        undefined,
        true,
      ),
    );

    await act(async () => hook.result.current.armWake());

    expect(hook.result.current.state).toBe("armed");
    expect(addModule).toHaveBeenCalledOnce();
    const node = MockWorkletNode.instances[0];
    act(() => node?.emit(new Float32Array(320).fill(0.2)));
    await waitFor(() =>
      expect(
        invoke.mock.calls.some(([command]) => command === "voice_wake_chunk"),
      ).toBe(true),
    );
    const chunkCall = invoke.mock.calls.find(
      ([command]) => command === "voice_wake_chunk",
    );
    expect(chunkCall?.[1]).toBeInstanceOf(ArrayBuffer);
    expect((chunkCall?.[1] as ArrayBuffer).byteLength).toBe(640);

    act(() => hook.unmount());
    expect(node?.port.onmessage).toBeNull();
    expect(node?.disconnect).toHaveBeenCalled();
  });

  it("keeps ScriptProcessor behind AudioWorklet feature detection", async () => {
    installCaptureMocks(false);
    const onTranscript = vi.fn();
    const onProjection = vi.fn();
    const hook = renderHook(() =>
      useVoiceCapture(
        voiceConfig(),
        onTranscript,
        onProjection,
        undefined,
        false,
      ),
    );

    await act(async () => hook.result.current.start());

    const audioContext = MockAudioContext.instances[0];
    expect(addModule).not.toHaveBeenCalled();
    expect(audioContext?.createScriptProcessor).toHaveBeenCalledWith(
      4_096,
      1,
      1,
    );
    act(() =>
      audioContext?.scriptProcessor.emit(new Float32Array(4_096).fill(0.2)),
    );
    await waitFor(() =>
      expect(
        invoke.mock.calls.some(([command]) => command === "voice_stream_chunk"),
      ).toBe(true),
    );

    act(() => hook.unmount());
    expect(audioContext?.scriptProcessor.onaudioprocess).toBeNull();
    expect(audioContext?.scriptProcessor.disconnect).toHaveBeenCalled();
  });

  it.each(["resolve", "reject"] as const)(
    "does not reconnect when capture is cancelled while addModule will %s",
    async (settlement) => {
      let settleModule: (() => void) | undefined;
      addModule.mockImplementation(
        () =>
          new Promise<void>((resolve, reject) => {
            settleModule = () =>
              settlement === "resolve"
                ? resolve()
                : reject(new Error("context closed"));
          }),
      );
      const onTranscript = vi.fn();
      const onProjection = vi.fn();
      const hook = renderHook(() =>
        useVoiceCapture(
          voiceConfig(),
          onTranscript,
          onProjection,
          undefined,
          false,
        ),
      );
      let startPromise: Promise<void> | undefined;

      await act(async () => {
        startPromise = hook.result.current.start();
        await Promise.resolve();
        await Promise.resolve();
      });
      await waitFor(() => expect(addModule).toHaveBeenCalledOnce());
      act(() => hook.result.current.cancel());
      await act(async () => {
        settleModule?.();
        await startPromise;
      });

      expect(hook.result.current.state).toBe("idle");
      expect(hook.result.current.error).toBe("");
      expect(sourceConnect).not.toHaveBeenCalled();
      expect(
        invoke.mock.calls.some(
          ([command, payload]) =>
            command === "microphone_state" &&
            (payload as { active?: boolean } | undefined)?.active === true,
        ),
      ).toBe(false);
      if (settlement === "resolve") {
        const node = MockWorkletNode.instances[0];
        expect(node?.port.onmessage).toBeNull();
        expect(node?.disconnect).toHaveBeenCalledOnce();
      }
      act(() => hook.unmount());
    },
  );
});

describe("barge-in during playback", () => {
  const idlePlayback: VoicePlaybackContext = {
    active: false,
    betweenClauses: false,
    bargeIn: true,
  };

  function installWakeInvoke(
    wakeStart: Record<string, unknown>,
    chunk: () => Record<string, unknown>,
  ) {
    invoke.mockImplementation((command: string) => {
      if (command === "voice_wake_start") return Promise.resolve(wakeStart);
      if (command === "voice_wake_chunk") return Promise.resolve(chunk());
      if (command === "voice_stream_start")
        return Promise.resolve({ streaming: true });
      if (command === "voice_stream_chunk")
        return Promise.resolve({ speech_prob: 0.8 });
      if (command === "microphone_state") return Promise.resolve(projection);
      return Promise.resolve({});
    });
  }

  /** 320 samples at 16 kHz is one 20 ms wake frame. */
  async function emitWakeFrames(count: number) {
    const node = MockWorkletNode.instances[0];
    expect(node).toBeDefined();
    await act(async () => {
      for (let index = 0; index < count; index += 1)
        node?.emit(new Float32Array(320).fill(0.3));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  function renderVoice(config: AppConfig, onBargeIn: () => void) {
    // Stable callback identities: a fresh `onProjection` per render would
    // re-run the unmount cleanup and tear the wake stream down.
    const onTranscript = vi.fn();
    const onProjection = vi.fn();
    return renderHook(
      ({ playback }: { playback: VoicePlaybackContext }) =>
        useVoiceCapture(
          config,
          onTranscript,
          onProjection,
          undefined,
          true,
          false,
          { ...playback, onBargeIn },
        ),
      { initialProps: { playback: idlePlayback } },
    );
  }

  beforeEach(() => invoke.mockReset());

  it("stops the sink and listens after 400 ms of speech through live playback", async () => {
    installCaptureMocks(true, true);
    installWakeInvoke({ fallback: "stt-match" }, () => ({
      wake: false,
      score: 0,
      fallback: "stt-match",
      speech_prob: 0.95,
    }));
    const onBargeIn = vi.fn();
    const hook = renderVoice(voiceConfig(true), onBargeIn);

    await act(async () => hook.result.current.armWake());
    expect(hook.result.current.state).toBe("armed");
    expect(hook.result.current.bargeIn.monitoring).toBe(false);

    await act(async () => {
      hook.rerender({ playback: { ...idlePlayback, active: true } });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // The wake stream survives playback instead of being suspended.
    expect(trackStop).not.toHaveBeenCalled();
    expect(applyConstraints).toHaveBeenCalledWith({ echoCancellation: true });
    await waitFor(() =>
      expect(hook.result.current.bargeIn).toEqual({
        enabled: true,
        monitoring: true,
        echoCancellation: "on",
        duplex: "full",
      }),
    );

    await emitWakeFrames(20);

    await waitFor(() => expect(invoke).toHaveBeenCalledWith("voice_stop"));
    await waitFor(() => expect(hook.result.current.state).toBe("listening"));
    expect(onBargeIn).toHaveBeenCalledWith("speech");
    act(() => hook.unmount());
  });

  it("never interrupts playback at or below the barge-in probability floor", async () => {
    installCaptureMocks(true, true);
    installWakeInvoke({ fallback: "stt-match" }, () => ({
      wake: false,
      score: 0,
      fallback: "stt-match",
      speech_prob: 0.9,
    }));
    const onBargeIn = vi.fn();
    const hook = renderVoice(voiceConfig(true), onBargeIn);

    await act(async () => hook.result.current.armWake());
    await act(async () => {
      hook.rerender({ playback: { ...idlePlayback, active: true } });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await emitWakeFrames(40);

    expect(
      invoke.mock.calls.some(([command]) => command === "voice_stop"),
    ).toBe(false);
    expect(onBargeIn).not.toHaveBeenCalled();
    expect(hook.result.current.state).toBe("armed");
    act(() => hook.unmount());
  });

  it("listens only between clauses when the device reports no echo cancellation", async () => {
    installCaptureMocks(true, false);
    installWakeInvoke({ fallback: "stt-match" }, () => ({
      wake: false,
      score: 0,
      fallback: "stt-match",
      speech_prob: 0.95,
    }));
    const onBargeIn = vi.fn();
    const hook = renderVoice(voiceConfig(true), onBargeIn);

    await act(async () => hook.result.current.armWake());
    await act(async () => {
      hook.rerender({ playback: { ...idlePlayback, active: true } });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await waitFor(() =>
      expect(hook.result.current.bargeIn).toEqual({
        enabled: true,
        monitoring: true,
        echoCancellation: "off",
        duplex: "half",
      }),
    );

    invoke.mockClear();
    await emitWakeFrames(30);
    expect(
      invoke.mock.calls.some(([command]) => command === "voice_wake_chunk"),
    ).toBe(false);
    expect(hook.result.current.state).toBe("armed");

    await act(async () => {
      hook.rerender({
        playback: { ...idlePlayback, active: true, betweenClauses: true },
      });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await emitWakeFrames(20);

    expect(
      invoke.mock.calls.some(([command]) => command === "voice_wake_chunk"),
    ).toBe(true);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("voice_stop"));
    await waitFor(() => expect(hook.result.current.state).toBe("listening"));
    act(() => hook.unmount());
  });

  it("raises the wake score bar while our own speaker audio is in the room", async () => {
    installCaptureMocks(true, true);
    let score = 0.94;
    installWakeInvoke({}, () => ({ wake: true, score }));
    const onBargeIn = vi.fn();
    const hook = renderVoice(voiceConfig(true), onBargeIn);

    await act(async () => hook.result.current.armWake());
    await act(async () => {
      hook.rerender({ playback: { ...idlePlayback, active: true } });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // 0.94 clears the configured 0.930 threshold but not the playback bar.
    await emitWakeFrames(2);
    expect(
      invoke.mock.calls.some(([command]) => command === "voice_stop"),
    ).toBe(false);
    expect(hook.result.current.state).toBe("armed");

    score = 0.98;
    await emitWakeFrames(1);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("voice_stop"));
    await waitFor(() => expect(hook.result.current.state).toBe("listening"));
    expect(onBargeIn).toHaveBeenCalledWith("wake");
    act(() => hook.unmount());
  });

  it("derives the barge-in threshold and UI string from measured state", () => {
    expect(bargeInWakeThresholdMilli(930)).toBe(965);
    expect(bargeInWakeThresholdMilli(0)).toBe(500);
    expect(bargeInWakeThresholdMilli(1_000)).toBe(1_000);
    expect(bargeInWakeThresholdMilli(-50)).toBe(500);

    const base = {
      enabled: true,
      monitoring: false,
      echoCancellation: "unknown",
      duplex: "full",
    } as const;
    expect(describeBargeIn({ ...base, enabled: false })).toBe("disabled");
    expect(describeBargeIn(base)).toBe("enabled · not measured");
    expect(describeBargeIn({ ...base, echoCancellation: "off" })).toBe(
      "enabled · AEC off",
    );
    expect(
      describeBargeIn({
        ...base,
        monitoring: true,
        echoCancellation: "on",
      }),
    ).toBe("listening · AEC on");
    expect(
      describeBargeIn({
        ...base,
        monitoring: true,
        echoCancellation: "off",
        duplex: "half",
      }),
    ).toBe("listening · half-duplex (AEC off)");
    expect(describeBargeIn({ ...base, monitoring: true })).toBe(
      "listening · AEC unmeasured",
    );
  });
});
