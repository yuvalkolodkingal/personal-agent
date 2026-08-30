import { afterEach, describe, expect, it, vi } from "vitest";

type ProcessorInstance = {
  process(inputs: Float32Array[][]): boolean;
};

type ProcessorConstructor = new (options?: {
  processorOptions?: { gain?: number };
}) => ProcessorInstance;

describe("voice capture AudioWorklet", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  it("mixes and resamples 48 kHz input into transferred 20 ms 16 kHz frames", async () => {
    const posted: Float32Array[] = [];
    const transfers: Transferable[][] = [];
    let processorName = "";
    let Processor: ProcessorConstructor | undefined;

    class MockAudioWorkletProcessor {
      readonly port = {
        postMessage: (frame: Float32Array, transfer: Transferable[]) => {
          posted.push(Float32Array.from(frame));
          transfers.push(transfer);
        },
      };
    }

    vi.stubGlobal("sampleRate", 48_000);
    vi.stubGlobal("AudioWorkletProcessor", MockAudioWorkletProcessor);
    vi.stubGlobal(
      "registerProcessor",
      (name: string, constructor: ProcessorConstructor) => {
        processorName = name;
        Processor = constructor;
      },
    );

    await import("./audio-worklet");
    expect(processorName).toBe("personal-agent-voice-capture");
    if (!Processor)
      throw new Error("AudioWorklet processor was not registered");
    const processor = new Processor({ processorOptions: { gain: 0.5 } });

    for (let block = 0; block < 15; block += 1) {
      const left = new Float32Array(128).fill(0.8);
      const right = new Float32Array(128).fill(0.4);
      expect(processor.process([[left, right]])).toBe(true);
    }

    expect(posted).toHaveLength(2);
    for (const frame of posted) {
      expect(frame).toHaveLength(320);
      expect(frame.every((sample) => Math.abs(sample - 0.3) < 0.000_001)).toBe(
        true,
      );
    }
    expect(transfers).toHaveLength(2);
    expect(transfers[0]).toHaveLength(1);
    expect(transfers[1]).toHaveLength(1);
  });
});
