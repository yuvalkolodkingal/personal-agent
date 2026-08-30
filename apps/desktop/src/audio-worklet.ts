const PROCESSOR_NAME = "personal-agent-voice-capture";
const TARGET_SAMPLE_RATE = 16_000;
const FRAME_DURATION_MS = 20;
const FRAME_SAMPLES = (TARGET_SAMPLE_RATE * FRAME_DURATION_MS) / 1000;

type VoiceCaptureProcessorOptions = {
  processorOptions?: { gain?: number };
};

declare const sampleRate: number;
declare class AudioWorkletProcessor {
  readonly port: MessagePort;
  constructor();
}
declare function registerProcessor(
  name: string,
  processorCtor: typeof AudioWorkletProcessor,
): void;

/**
 * Resample capture off the renderer's main thread and transfer one fixed-size
 * Float32 frame every 20 ms of input audio. The next stage converts these
 * frames to PCM16 for Tauri's raw request body.
 */
class VoiceCaptureProcessor extends AudioWorkletProcessor {
  private readonly gain: number;
  private readonly sourcePerOutputSample: number;
  private readonly sourceBuffer = new Float32Array(4_096);
  private sourceLength = 0;
  private sourcePosition = 0;
  private outputFrame = new Float32Array(FRAME_SAMPLES);
  private outputLength = 0;

  constructor(options?: VoiceCaptureProcessorOptions) {
    super();
    const configuredGain = options?.processorOptions?.gain;
    this.gain = Number.isFinite(configuredGain)
      ? Math.max(0, configuredGain ?? 1)
      : 1;
    this.sourcePerOutputSample = sampleRate / TARGET_SAMPLE_RATE;
  }

  process(inputs: Float32Array[][]): boolean {
    const channels = inputs[0];
    const samples = channels?.[0]?.length ?? 0;
    if (!channels?.length || !samples) return true;

    for (let index = 0; index < samples; index += 1) {
      let mono = 0;
      for (const channel of channels) mono += channel[index] ?? 0;
      mono = (mono / channels.length) * this.gain;
      this.sourceBuffer[this.sourceLength] = Math.max(-1, Math.min(1, mono));
      this.sourceLength += 1;
    }

    while (this.sourcePosition + 1 < this.sourceLength) {
      const lower = Math.floor(this.sourcePosition);
      const fraction = this.sourcePosition - lower;
      const first = this.sourceBuffer[lower] ?? 0;
      const second = this.sourceBuffer[lower + 1] ?? first;
      this.outputFrame[this.outputLength] = first + (second - first) * fraction;
      this.outputLength += 1;
      this.sourcePosition += this.sourcePerOutputSample;

      if (this.outputLength === FRAME_SAMPLES) {
        const completed = this.outputFrame;
        this.port.postMessage(completed, [completed.buffer]);
        this.outputFrame = new Float32Array(FRAME_SAMPLES);
        this.outputLength = 0;
      }
    }

    const consumed = Math.min(
      Math.floor(this.sourcePosition),
      this.sourceLength,
    );
    if (consumed > 0) {
      this.sourceBuffer.copyWithin(0, consumed, this.sourceLength);
      this.sourceLength -= consumed;
      this.sourcePosition -= consumed;
    }
    return true;
  }
}

registerProcessor(PROCESSOR_NAME, VoiceCaptureProcessor);

export {};
