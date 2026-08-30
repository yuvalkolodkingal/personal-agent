import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { matchWakePhrase, sendVoiceStreamChunk } from "./useVoiceCapture";

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
