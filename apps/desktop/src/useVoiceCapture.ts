import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import type { AppConfig, Projection } from "./types";

export type VoiceCaptureState =
  | "idle"
  | "loading_model"
  | "requesting"
  | "listening"
  | "endpointing"
  | "transcribing"
  | "error";

function downsample(
  samples: Float32Array,
  sourceRate: number,
  targetRate = 16_000,
) {
  if (sourceRate === targetRate) return Array.from(samples);
  const ratio = sourceRate / targetRate;
  const output = new Array<number>(Math.floor(samples.length / ratio));
  for (let index = 0; index < output.length; index += 1) {
    const start = Math.floor(index * ratio);
    const end = Math.min(samples.length, Math.floor((index + 1) * ratio));
    let sum = 0;
    for (let source = start; source < end; source += 1)
      sum += samples[source] ?? 0;
    output[index] = sum / Math.max(1, end - start);
  }
  return output;
}

function mergeChunks(chunks: Float32Array[]) {
  const length = chunks.reduce((total, chunk) => total + chunk.length, 0);
  const merged = new Float32Array(length);
  let offset = 0;
  chunks.forEach((chunk) => {
    merged.set(chunk, offset);
    offset += chunk.length;
  });
  return merged;
}

export function useVoiceCapture(
  config: AppConfig,
  onTranscript: (text: string) => void,
  onProjection: (projection: Projection) => void,
  onPartialTranscript?: (text: string) => void,
) {
  const [state, setState] = useState<VoiceCaptureState>("idle");
  const [error, setError] = useState("");
  const [level, setLevel] = useState(0);
  const [partialTranscript, setPartialTranscript] = useState("");
  const stateRef = useRef<VoiceCaptureState>("idle");
  const stopRequested = useRef(false);
  const stopInFlight = useRef(false);
  const stream = useRef<MediaStream | null>(null);
  const context = useRef<AudioContext | null>(null);
  const source = useRef<MediaStreamAudioSourceNode | null>(null);
  const processor = useRef<ScriptProcessorNode | null>(null);
  const chunks = useRef<Float32Array[]>([]);
  const streamingChunks = useRef<Float32Array[]>([]);
  const streamingSamples = useRef(0);
  const neuralStreaming = useRef(false);
  const streamFailure = useRef("");
  const streamQueue = useRef<Promise<void>>(Promise.resolve());
  const speechStarted = useRef(false);
  const noiseFloor = useRef(0.005);
  const silenceStartedAt = useRef(0);
  const latestPartial = useRef("");
  const turnCheckInFlight = useRef(false);
  const turnDeferrals = useRef(0);

  const transition = useCallback((next: VoiceCaptureState) => {
    stateRef.current = next;
    setState(next);
  }, []);

  const publishPartial = useCallback(
    (text: string) => {
      latestPartial.current = text;
      setPartialTranscript(text);
      onPartialTranscript?.(text);
    },
    [onPartialTranscript],
  );

  const queueStreamChunk = useCallback(
    (audio: Float32Array, sampleRate: number) => {
      if (!neuralStreaming.current || !audio.length) return;
      const samples = downsample(audio, sampleRate);
      streamQueue.current = streamQueue.current
        .then(async () => {
          if (streamFailure.current) return;
          const result = await invoke<{ text?: string }>("voice_stream_chunk", {
            samples,
            sampleRateHz: 16_000,
          });
          if (result.text?.trim()) publishPartial(result.text.trim());
        })
        .catch((caught) => {
          streamFailure.current = String(caught);
        });
    },
    [publishPartial],
  );

  const stop = useCallback(
    async (endpointDetected = false) => {
      if (stopInFlight.current) return;
      if (
        stateRef.current === "loading_model" ||
        (stateRef.current === "requesting" &&
          (!stream.current || !context.current))
      ) {
        stopRequested.current = true;
        return;
      }
      if (!stream.current || !context.current) return;
      stopInFlight.current = true;
      transition(endpointDetected ? "endpointing" : "transcribing");
      processor.current?.disconnect();
      source.current?.disconnect();
      stream.current.getTracks().forEach((track) => track.stop());
      const sampleRate = context.current.sampleRate;
      await context.current.close();
      if (streamingChunks.current.length) {
        queueStreamChunk(mergeChunks(streamingChunks.current), sampleRate);
        streamingChunks.current = [];
        streamingSamples.current = 0;
      }
      const merged = mergeChunks(chunks.current);
      chunks.current = [];
      stream.current = null;
      context.current = null;
      processor.current = null;
      source.current = null;
      setLevel(0);
      try {
        const projection = await invoke<Projection>("microphone_state", {
          active: false,
          mode: config.voice.mode,
        });
        onProjection(projection);
        if (merged.length < sampleRate / 10)
          throw new Error(
            "No speech was captured. Hold the microphone button a little longer and try again.",
          );
        await streamQueue.current;
        let transcript: { text: string };
        if (neuralStreaming.current && !streamFailure.current) {
          transition("transcribing");
          transcript = await invoke<{ text: string }>("voice_stream_stop");
        } else {
          if (neuralStreaming.current)
            void invoke("voice_stream_cancel").catch(() => undefined);
          transcript = await invoke<{ text: string }>("voice_transcribe", {
            samples: downsample(merged, sampleRate),
            sampleRateHz: 16_000,
          });
        }
        if (!transcript.text.trim())
          throw new Error(
            "No speech was detected. Check the selected microphone and input level.",
          );
        publishPartial(transcript.text.trim());
        onTranscript(transcript.text.trim());
        transition("idle");
      } catch (caught) {
        setError(
          streamFailure.current
            ? `${String(caught)} Streaming detail: ${streamFailure.current}`
            : String(caught),
        );
        transition("error");
      } finally {
        neuralStreaming.current = false;
        streamFailure.current = "";
        stopInFlight.current = false;
        speechStarted.current = false;
        silenceStartedAt.current = 0;
        turnCheckInFlight.current = false;
        turnDeferrals.current = 0;
      }
    },
    [
      config.voice.mode,
      onProjection,
      onTranscript,
      publishPartial,
      queueStreamChunk,
      transition,
    ],
  );

  const considerEndpoint = useCallback(async () => {
    if (
      turnCheckInFlight.current ||
      stopInFlight.current ||
      stateRef.current !== "listening"
    )
      return;
    turnCheckInFlight.current = true;
    transition("endpointing");
    try {
      const audioContext = context.current;
      if (audioContext && streamingChunks.current.length) {
        queueStreamChunk(
          mergeChunks(streamingChunks.current),
          audioContext.sampleRate,
        );
        streamingChunks.current = [];
        streamingSamples.current = 0;
      }
      await streamQueue.current;
      const result =
        neuralStreaming.current && !streamFailure.current
          ? await invoke<{ complete: boolean; probability?: number }>(
              "voice_turn_complete",
            )
          : { complete: true };
      if (result.complete || turnDeferrals.current >= 2) {
        await stop(true);
        return;
      }
      turnDeferrals.current += 1;
      silenceStartedAt.current = performance.now();
      transition("listening");
    } catch {
      // A missing or failed semantic endpoint model must never strand capture.
      await stop(true);
    } finally {
      turnCheckInFlight.current = false;
    }
  }, [queueStreamChunk, stop, transition]);

  const start = useCallback(async () => {
    if (
      [
        "listening",
        "loading_model",
        "requesting",
        "endpointing",
        "transcribing",
      ].includes(stateRef.current)
    )
      return;
    stopRequested.current = false;
    stopInFlight.current = false;
    streamFailure.current = "";
    streamQueue.current = Promise.resolve();
    speechStarted.current = false;
    silenceStartedAt.current = 0;
    turnCheckInFlight.current = false;
    turnDeferrals.current = 0;
    noiseFloor.current = 0.005;
    publishPartial("");
    setError("");
    transition("loading_model");
    try {
      if (config.voice.stt_backend === "moonshine") {
        try {
          const result = await invoke<{ streaming: boolean }>(
            "voice_stream_start",
          );
          neuralStreaming.current = result.streaming;
        } catch (caught) {
          neuralStreaming.current = false;
          streamFailure.current = String(caught);
        }
      } else neuralStreaming.current = false;
      if (stopRequested.current) {
        if (neuralStreaming.current)
          void invoke("voice_stream_cancel").catch(() => undefined);
        transition("idle");
        return;
      }
      transition("requesting");
      if (!navigator.mediaDevices?.getUserMedia)
        throw new Error(
          "Microphone capture is unavailable in this system webview.",
        );
      const media = await navigator.mediaDevices.getUserMedia({
        audio: {
          deviceId: config.voice.input_device || undefined,
          echoCancellation: config.voice.echo_cancellation,
          noiseSuppression: config.voice.noise_suppression,
          autoGainControl: config.voice.automatic_gain_control,
          channelCount: 1,
        },
      });
      if (stopRequested.current) {
        media.getTracks().forEach((track) => track.stop());
        if (neuralStreaming.current)
          void invoke("voice_stream_cancel").catch(() => undefined);
        transition("idle");
        return;
      }
      const audioContext = new AudioContext();
      await audioContext.resume();
      const input = audioContext.createMediaStreamSource(media);
      const node = audioContext.createScriptProcessor(4096, 1, 1);
      const silentOutput = audioContext.createGain();
      silentOutput.gain.value = 0;
      chunks.current = [];
      streamingChunks.current = [];
      streamingSamples.current = 0;
      node.onaudioprocess = (event) => {
        const data = new Float32Array(event.inputBuffer.getChannelData(0));
        const gain = config.voice.input_gain_percent / 100;
        let sum = 0;
        for (let index = 0; index < data.length; index += 1) {
          const sample = Math.max(-1, Math.min(1, (data[index] ?? 0) * gain));
          data[index] = sample;
          sum += sample * sample;
        }
        const rms = Math.sqrt(sum / data.length);
        chunks.current.push(data);
        streamingChunks.current.push(data);
        streamingSamples.current += data.length;
        if (streamingSamples.current >= audioContext.sampleRate * 0.45) {
          queueStreamChunk(
            mergeChunks(streamingChunks.current),
            audioContext.sampleRate,
          );
          streamingChunks.current = [];
          streamingSamples.current = 0;
        }
        setLevel(Math.min(1, rms * 8));
        const now = performance.now();
        if (!speechStarted.current)
          noiseFloor.current = noiseFloor.current * 0.97 + rms * 0.03;
        const startThreshold = Math.max(
          0.012,
          noiseFloor.current * 3,
          (config.voice.vad_start_milli / 1000) * 0.04,
        );
        const stopThreshold = Math.max(
          0.008,
          noiseFloor.current * 1.8,
          (config.voice.vad_stop_milli / 1000) * 0.04,
        );
        if (rms >= startThreshold) {
          speechStarted.current = true;
          silenceStartedAt.current = 0;
        } else if (speechStarted.current && rms <= stopThreshold) {
          if (!silenceStartedAt.current) silenceStartedAt.current = now;
          const looksComplete = /[.!?]["')\]]?$/.test(
            latestPartial.current.trim(),
          );
          const endpointMs = looksComplete
            ? config.voice.endpoint_short_ms
            : config.voice.endpoint_long_ms;
          if (
            now - silenceStartedAt.current >= endpointMs &&
            stateRef.current === "listening"
          )
            void considerEndpoint();
        } else if (speechStarted.current) silenceStartedAt.current = 0;
      };
      input.connect(node);
      node.connect(silentOutput);
      silentOutput.connect(audioContext.destination);
      stream.current = media;
      context.current = audioContext;
      source.current = input;
      processor.current = node;
      const projection = await invoke<Projection>("microphone_state", {
        active: true,
        mode: config.voice.mode,
      });
      onProjection(projection);
      transition("listening");
      if (stopRequested.current) await stop();
    } catch (caught) {
      processor.current?.disconnect();
      source.current?.disconnect();
      stream.current?.getTracks().forEach((track) => track.stop());
      void context.current?.close();
      if (neuralStreaming.current)
        void invoke("voice_stream_cancel").catch(() => undefined);
      stream.current = null;
      context.current = null;
      processor.current = null;
      source.current = null;
      setLevel(0);
      void invoke<Projection>("microphone_state", {
        active: false,
        mode: config.voice.mode,
      })
        .then(onProjection)
        .catch(() => undefined);
      setError(String(caught));
      transition("error");
    }
  }, [
    config.voice,
    considerEndpoint,
    onProjection,
    publishPartial,
    queueStreamChunk,
    stop,
    transition,
  ]);

  const cancel = useCallback(() => {
    stopRequested.current = true;
    processor.current?.disconnect();
    source.current?.disconnect();
    stream.current?.getTracks().forEach((track) => track.stop());
    void context.current?.close();
    if (neuralStreaming.current)
      void invoke("voice_stream_cancel").catch(() => undefined);
    stream.current = null;
    context.current = null;
    neuralStreaming.current = false;
    turnCheckInFlight.current = false;
    turnDeferrals.current = 0;
    void invoke<Projection>("microphone_state", {
      active: false,
      mode: config.voice.mode,
    })
      .then(onProjection)
      .catch(() => undefined);
    transition("idle");
  }, [config.voice.mode, onProjection, transition]);

  useEffect(() => () => cancel(), [cancel]);

  return { state, error, level, partialTranscript, start, stop, cancel };
}
