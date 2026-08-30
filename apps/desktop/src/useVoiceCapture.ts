import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import type { AppConfig, Projection } from "./types";

export type VoiceCaptureState =
  | "idle"
  | "arming"
  | "armed"
  | "wake_detected"
  | "loading_model"
  | "requesting"
  | "listening"
  | "endpointing"
  | "transcribing"
  | "error";

export type WakePhraseMatch = {
  phrase: string;
  remainder: string;
};

export type VoiceTranscriptMeta = {
  final: boolean;
  source: "capture" | "wake";
  audioEndMs?: number;
};

function normalizedWords(value: string) {
  return value
    .toLocaleLowerCase("en-US")
    .replace(/[^a-z0-9']+/g, " ")
    .trim()
    .split(/\s+/)
    .filter(Boolean);
}

/** Match whole spoken words so names such as “Jarvison” cannot trigger the agent. */
export function matchWakePhrase(
  transcript: string,
  phrases: string[],
): WakePhraseMatch | null {
  const words = Array.from(transcript.matchAll(/[a-z0-9']+/gi)).map(
    (match) => ({
      normalized: match[0].toLocaleLowerCase("en-US"),
      end: (match.index ?? 0) + match[0].length,
    }),
  );
  const candidates = phrases
    .map((phrase) => ({ phrase, words: normalizedWords(phrase) }))
    .filter((candidate) => candidate.words.length)
    .sort((left, right) => right.words.length - left.words.length);
  for (const candidate of candidates) {
    for (
      let start = 0;
      start <= words.length - candidate.words.length;
      start += 1
    ) {
      if (
        candidate.words.every(
          (word, offset) => words[start + offset]?.normalized === word,
        )
      ) {
        const end = words[start + candidate.words.length - 1]?.end ?? 0;
        return {
          phrase: candidate.phrase,
          remainder: transcript
            .slice(end)
            .replace(/^[\s,.:;!?—–-]+/, "")
            .trim(),
        };
      }
    }
  }
  return null;
}

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

/** Encode one fixed-rate voice stream frame without per-sample JSON serialization. */
export function encodePcm16Le(samples: ArrayLike<number>): ArrayBuffer {
  const frame = new ArrayBuffer(samples.length * Int16Array.BYTES_PER_ELEMENT);
  const view = new DataView(frame);
  for (let index = 0; index < samples.length; index += 1) {
    const source = samples[index] ?? 0;
    const sample = Number.isFinite(source)
      ? Math.max(-1, Math.min(1, source))
      : 0;
    const pcm = Math.round(sample < 0 ? sample * 32_768 : sample * 32_767);
    view.setInt16(index * Int16Array.BYTES_PER_ELEMENT, pcm, true);
  }
  return frame;
}

export function sendVoiceStreamChunk(samples: ArrayLike<number>) {
  return invoke<{ text?: string }>(
    "voice_stream_chunk",
    encodePcm16Le(samples),
  );
}

export function useVoiceCapture(
  config: AppConfig,
  onTranscript: (text: string, meta: VoiceTranscriptMeta) => void,
  onProjection: (projection: Projection) => void,
  onPartialTranscript?: (text: string, meta: VoiceTranscriptMeta) => void,
  wakeReady = true,
  wakeSuspended = false,
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
  const startRef = useRef<() => Promise<void>>(async () => undefined);
  const onTranscriptRef = useRef(onTranscript);
  const wakeStream = useRef<MediaStream | null>(null);
  const wakeContext = useRef<AudioContext | null>(null);
  const wakeSource = useRef<MediaStreamAudioSourceNode | null>(null);
  const wakeProcessor = useRef<ScriptProcessorNode | null>(null);
  const wakeRolling = useRef<Float32Array[]>([]);
  const wakeCandidate = useRef<Float32Array[]>([]);
  const wakeCandidateSamples = useRef(0);
  const wakeSpeechStarted = useRef(false);
  const wakeNoiseFloor = useRef(0.005);
  const wakeSilenceStartedAt = useRef(0);
  const wakeProcessing = useRef(false);
  const wakeGeneration = useRef(0);
  const lastWakeDetectedAt = useRef(0);

  onTranscriptRef.current = onTranscript;

  const transition = useCallback((next: VoiceCaptureState) => {
    stateRef.current = next;
    setState(next);
  }, []);

  const publishPartial = useCallback(
    (
      text: string,
      meta: VoiceTranscriptMeta = { final: false, source: "capture" },
      notify = true,
    ) => {
      latestPartial.current = text;
      setPartialTranscript(text);
      if (notify) onPartialTranscript?.(text, meta);
    },
    [onPartialTranscript],
  );

  const stopWakeCapture = useCallback(
    (publishPrivacy = true) => {
      wakeGeneration.current += 1;
      wakeProcessor.current?.disconnect();
      wakeSource.current?.disconnect();
      wakeStream.current?.getTracks().forEach((track) => track.stop());
      void wakeContext.current?.close().catch(() => undefined);
      wakeStream.current = null;
      wakeContext.current = null;
      wakeSource.current = null;
      wakeProcessor.current = null;
      wakeRolling.current = [];
      wakeCandidate.current = [];
      wakeCandidateSamples.current = 0;
      wakeSpeechStarted.current = false;
      wakeSilenceStartedAt.current = 0;
      wakeProcessing.current = false;
      setLevel(0);
      if (publishPrivacy)
        void invoke<Projection>("microphone_state", {
          active: false,
          mode: "wake-only",
        })
          .then(onProjection)
          .catch(() => undefined);
      if (["arming", "armed", "wake_detected"].includes(stateRef.current))
        transition("idle");
    },
    [onProjection, transition],
  );

  const armWake = useCallback(async () => {
    if (
      !config.voice.enabled ||
      !config.voice.wake_enabled ||
      !wakeReady ||
      wakeSuspended ||
      wakeStream.current ||
      !["idle", "error"].includes(stateRef.current)
    )
      return;
    if (!navigator.mediaDevices?.getUserMedia) {
      setError("Wake recognition is unavailable in this system webview.");
      transition("error");
      return;
    }
    setError("");
    transition("arming");
    const generation = wakeGeneration.current + 1;
    wakeGeneration.current = generation;
    try {
      const media = await navigator.mediaDevices.getUserMedia({
        audio: {
          deviceId: config.voice.input_device || undefined,
          echoCancellation: config.voice.echo_cancellation,
          noiseSuppression: config.voice.noise_suppression,
          autoGainControl: config.voice.automatic_gain_control,
          channelCount: 1,
        },
      });
      if (wakeGeneration.current !== generation || wakeSuspended) {
        media.getTracks().forEach((track) => track.stop());
        return;
      }
      const audioContext = new AudioContext();
      await audioContext.resume();
      const input = audioContext.createMediaStreamSource(media);
      const node = audioContext.createScriptProcessor(4096, 1, 1);
      const silentOutput = audioContext.createGain();
      silentOutput.gain.value = 0;
      wakeRolling.current = [];
      wakeCandidate.current = [];
      wakeCandidateSamples.current = 0;
      wakeSpeechStarted.current = false;
      wakeNoiseFloor.current = 0.005;
      wakeSilenceStartedAt.current = 0;
      wakeProcessing.current = false;
      node.onaudioprocess = (event) => {
        if (wakeGeneration.current !== generation) return;
        const data = new Float32Array(event.inputBuffer.getChannelData(0));
        const gain = config.voice.input_gain_percent / 100;
        let sum = 0;
        for (let index = 0; index < data.length; index += 1) {
          const sample = Math.max(-1, Math.min(1, (data[index] ?? 0) * gain));
          data[index] = sample;
          sum += sample * sample;
        }
        const rms = Math.sqrt(sum / data.length);
        setLevel(Math.min(1, rms * 8));
        // Keep the microphone open, but do not build a second candidate while the local
        // recognizer owns the first one. This bounds inference and preserves phrase edges.
        if (wakeProcessing.current) return;
        const startThreshold = Math.max(
          0.012,
          wakeNoiseFloor.current * 3,
          (config.voice.vad_start_milli / 1000) * 0.04,
        );
        const stopThreshold = Math.max(
          0.008,
          wakeNoiseFloor.current * 1.8,
          (config.voice.vad_stop_milli / 1000) * 0.04,
        );
        const preRollSamples =
          (audioContext.sampleRate * config.voice.pre_roll_ms) / 1000;
        if (!wakeSpeechStarted.current) {
          wakeNoiseFloor.current = wakeNoiseFloor.current * 0.97 + rms * 0.03;
          wakeRolling.current.push(data);
          let rollingSamples = wakeRolling.current.reduce(
            (total, chunk) => total + chunk.length,
            0,
          );
          while (
            rollingSamples > preRollSamples &&
            wakeRolling.current.length > 1
          ) {
            rollingSamples -= wakeRolling.current.shift()?.length ?? 0;
          }
          if (rms >= startThreshold) {
            wakeSpeechStarted.current = true;
            wakeCandidate.current = [...wakeRolling.current];
            wakeCandidateSamples.current = rollingSamples;
            wakeSilenceStartedAt.current = 0;
          }
          return;
        }
        if (!wakeProcessing.current) {
          wakeCandidate.current.push(data);
          wakeCandidateSamples.current += data.length;
        }
        const now = performance.now();
        if (rms <= stopThreshold) {
          if (!wakeSilenceStartedAt.current) wakeSilenceStartedAt.current = now;
        } else wakeSilenceStartedAt.current = 0;
        const silenceMs = Math.max(
          350,
          Math.min(900, config.voice.vad_stop_milli),
        );
        const timedOut =
          wakeCandidateSamples.current >= audioContext.sampleRate * 4;
        const ended =
          wakeSilenceStartedAt.current > 0 &&
          now - wakeSilenceStartedAt.current >= silenceMs;
        if ((!ended && !timedOut) || wakeProcessing.current) return;
        const candidate = mergeChunks(wakeCandidate.current);
        wakeCandidate.current = [];
        wakeCandidateSamples.current = 0;
        wakeRolling.current = [];
        wakeSpeechStarted.current = false;
        wakeSilenceStartedAt.current = 0;
        if (candidate.length < audioContext.sampleRate / 4) return;
        wakeProcessing.current = true;
        const requestGeneration = wakeGeneration.current;
        void invoke<{ text: string }>("voice_transcribe", {
          samples: downsample(candidate, audioContext.sampleRate),
          sampleRateHz: 16_000,
        })
          .then(async (result) => {
            if (wakeGeneration.current !== requestGeneration) return;
            const match = matchWakePhrase(
              result.text,
              config.voice.wake_phrases,
            );
            if (!match) return;
            const detectedAt = performance.now();
            if (
              detectedAt - lastWakeDetectedAt.current <
              config.voice.refractory_ms
            )
              return;
            lastWakeDetectedAt.current = detectedAt;
            publishPartial(
              match.phrase,
              { final: false, source: "wake" },
              false,
            );
            stopWakeCapture();
            transition("wake_detected");
            if (match.remainder) {
              onTranscriptRef.current(match.remainder, {
                final: true,
                source: "wake",
              });
              transition("idle");
            } else {
              await startRef.current();
            }
          })
          .catch((caught) => {
            if (wakeGeneration.current !== requestGeneration) return;
            setError(`Wake recognition failed: ${String(caught)}`);
            stopWakeCapture();
            transition("error");
          })
          .finally(() => {
            if (wakeGeneration.current === requestGeneration)
              wakeProcessing.current = false;
          });
      };
      input.connect(node);
      node.connect(silentOutput);
      silentOutput.connect(audioContext.destination);
      wakeStream.current = media;
      wakeContext.current = audioContext;
      wakeSource.current = input;
      wakeProcessor.current = node;
      const projection = await invoke<Projection>("microphone_state", {
        active: true,
        mode: "wake-only",
      });
      onProjection(projection);
      transition("armed");
    } catch (caught) {
      if (wakeGeneration.current !== generation) return;
      stopWakeCapture();
      setError(`Could not arm wake recognition: ${String(caught)}`);
      transition("error");
    }
  }, [
    config.voice,
    onProjection,
    publishPartial,
    stopWakeCapture,
    transition,
    wakeReady,
    wakeSuspended,
  ]);

  const queueStreamChunk = useCallback(
    (audio: Float32Array, sampleRate: number) => {
      if (!neuralStreaming.current || !audio.length) return;
      const samples = downsample(audio, sampleRate);
      const audioEndMs = performance.now();
      streamQueue.current = streamQueue.current
        .then(async () => {
          if (streamFailure.current) return;
          const result = await sendVoiceStreamChunk(samples);
          if (result.text?.trim())
            publishPartial(result.text.trim(), {
              final: false,
              source: "capture",
              audioEndMs,
            });
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
      const audioEndMs = performance.now();
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
        publishPartial(
          transcript.text.trim(),
          { final: true, source: "capture", audioEndMs },
          false,
        );
        onTranscript(transcript.text.trim(), {
          final: true,
          source: "capture",
          audioEndMs,
        });
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
    stopWakeCapture();
    stopRequested.current = false;
    stopInFlight.current = false;
    streamFailure.current = "";
    streamQueue.current = Promise.resolve();
    speechStarted.current = false;
    silenceStartedAt.current = 0;
    turnCheckInFlight.current = false;
    turnDeferrals.current = 0;
    noiseFloor.current = 0.005;
    publishPartial("", { final: false, source: "capture" }, false);
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
    stopWakeCapture,
    transition,
  ]);

  startRef.current = start;

  const cancel = useCallback(() => {
    stopRequested.current = true;
    stopWakeCapture();
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
  }, [config.voice.mode, onProjection, stopWakeCapture, transition]);

  const wakeShouldRun =
    config.voice.enabled &&
    config.voice.wake_enabled &&
    wakeReady &&
    !wakeSuspended;

  useEffect(() => {
    // Increment the generation even while getUserMedia is pending so a newly suspended
    // turn cannot finish arming a stale wake listener behind model/TTS activity.
    if (!wakeShouldRun) stopWakeCapture();
  }, [stopWakeCapture, wakeShouldRun]);

  useEffect(() => {
    if (!wakeShouldRun || state !== "idle") return;
    const timer = window.setTimeout(() => void armWake(), 650);
    return () => window.clearTimeout(timer);
  }, [armWake, state, wakeShouldRun]);

  useEffect(() => () => cancel(), [cancel]);

  return {
    state,
    error,
    level,
    partialTranscript,
    start,
    stop,
    cancel,
    armWake,
    wakeArmed: state === "armed",
  };
}
