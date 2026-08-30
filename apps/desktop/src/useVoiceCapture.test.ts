import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import {
  advanceVadEndpoint,
  matchWakePhrase,
  passesVoicePreGate,
  sendVoiceStreamChunk,
  sendWakeStreamChunk,
  vadProbabilityThreshold,
} from "./useVoiceCapture";

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
    const sameSilence = advanceVadEndpoint(
      firstSilence.state,
      0.1,
      600,
      350,
    );
    expect(sameSilence.consultSmartTurn).toBe(false);
    const resumedSpeech = advanceVadEndpoint(
      sameSilence.state,
      0.75,
      600,
      350,
    );
    expect(
      advanceVadEndpoint(resumedSpeech.state, 0.15, 600, 350)
        .consultSmartTurn,
    ).toBe(true);
  });
});
