import { describe, expect, it } from "vitest";
import { matchWakePhrase } from "./useVoiceCapture";

describe("wake phrase matching", () => {
  it("matches configured wake words despite punctuation and returns the command", () => {
    expect(
      matchWakePhrase("Hey, JARVIS! What's on my calendar?", [
        "hey jarvis",
        "jarvis",
      ]),
    ).toEqual({
      phrase: "hey jarvis",
      remainder: "what's on my calendar",
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
});
