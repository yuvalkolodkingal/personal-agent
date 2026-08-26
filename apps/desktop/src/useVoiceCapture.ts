import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import type { AppConfig, Projection } from "./types";

export type VoiceCaptureState = "idle" | "requesting" | "listening" | "transcribing" | "error";

function downsample(samples: Float32Array, sourceRate: number, targetRate = 16_000) {
  if (sourceRate === targetRate) return Array.from(samples);
  const ratio = sourceRate / targetRate;
  const output = new Array<number>(Math.floor(samples.length / ratio));
  for (let index = 0; index < output.length; index += 1) {
    const start = Math.floor(index * ratio);
    const end = Math.min(samples.length, Math.floor((index + 1) * ratio));
    let sum = 0;
    for (let source = start; source < end; source += 1) sum += samples[source] ?? 0;
    output[index] = sum / Math.max(1, end - start);
  }
  return output;
}

export function useVoiceCapture(
  config: AppConfig,
  onTranscript: (text: string) => void,
  onProjection: (projection: Projection) => void,
) {
  const [state, setState] = useState<VoiceCaptureState>("idle");
  const [error, setError] = useState("");
  const [level, setLevel] = useState(0);
  const stateRef = useRef<VoiceCaptureState>("idle");
  const stopRequested = useRef(false);
  const stream = useRef<MediaStream | null>(null);
  const context = useRef<AudioContext | null>(null);
  const source = useRef<MediaStreamAudioSourceNode | null>(null);
  const processor = useRef<ScriptProcessorNode | null>(null);
  const chunks = useRef<Float32Array[]>([]);

  const transition = useCallback((next: VoiceCaptureState) => {
    stateRef.current = next;
    setState(next);
  }, []);

  const stop = useCallback(async () => {
    if (stateRef.current === "requesting" && (!stream.current || !context.current)) {
      stopRequested.current = true;
      return;
    }
    if (!stream.current || !context.current) return;
    transition("transcribing");
    processor.current?.disconnect();
    source.current?.disconnect();
    stream.current.getTracks().forEach((track) => track.stop());
    const sampleRate = context.current.sampleRate;
    await context.current.close();
    const length = chunks.current.reduce((total, chunk) => total + chunk.length, 0);
    const merged = new Float32Array(length);
    let offset = 0;
    chunks.current.forEach((chunk) => { merged.set(chunk, offset); offset += chunk.length; });
    chunks.current = [];
    stream.current = null;
    context.current = null;
    processor.current = null;
    source.current = null;
    setLevel(0);
    try {
      const projection = await invoke<Projection>("microphone_state", { active: false, mode: config.voice.mode });
      onProjection(projection);
      if (merged.length < sampleRate / 10) throw new Error("No speech was captured. Hold the microphone button a little longer and try again.");
      const transcript = await invoke<{ text: string }>("voice_transcribe", {
        samples: downsample(merged, sampleRate), sampleRateHz: 16_000,
      });
      if (!transcript.text.trim()) throw new Error("No speech was detected. Check the selected microphone and input level.");
      onTranscript(transcript.text);
      transition("idle");
    } catch (caught) {
      setError(String(caught));
      transition("error");
    }
  }, [config.voice.mode, onProjection, onTranscript, transition]);

  const start = useCallback(async () => {
    if (["listening", "requesting", "transcribing"].includes(stateRef.current)) return;
    stopRequested.current = false;
    transition("requesting");
    setError("");
    try {
      if (!navigator.mediaDevices?.getUserMedia) throw new Error("Microphone capture is unavailable in this system webview.");
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
      node.onaudioprocess = (event) => {
        const data = new Float32Array(event.inputBuffer.getChannelData(0));
        const gain = config.voice.input_gain_percent / 100;
        let sum = 0;
        for (let index = 0; index < data.length; index += 1) {
          const sample = Math.max(-1, Math.min(1, (data[index] ?? 0) * gain));
          data[index] = sample;
          sum += sample * sample;
        }
        chunks.current.push(data);
        setLevel(Math.min(1, Math.sqrt(sum / data.length) * 8));
      };
      input.connect(node);
      node.connect(silentOutput);
      silentOutput.connect(audioContext.destination);
      stream.current = media;
      context.current = audioContext;
      source.current = input;
      processor.current = node;
      const projection = await invoke<Projection>("microphone_state", { active: true, mode: config.voice.mode });
      onProjection(projection);
      transition("listening");
      if (stopRequested.current) await stop();
    } catch (caught) {
      processor.current?.disconnect();
      source.current?.disconnect();
      stream.current?.getTracks().forEach((track) => track.stop());
      void context.current?.close();
      stream.current = null;
      context.current = null;
      processor.current = null;
      source.current = null;
      setLevel(0);
      void invoke<Projection>("microphone_state", { active: false, mode: config.voice.mode }).then(onProjection).catch(() => undefined);
      setError(String(caught));
      transition("error");
    }
  }, [config.voice, onProjection, stop, transition]);

  useEffect(() => () => {
    processor.current?.disconnect();
    source.current?.disconnect();
    stream.current?.getTracks().forEach((track) => track.stop());
    void context.current?.close();
  }, []);

  return { state, error, level, start, stop };
}
