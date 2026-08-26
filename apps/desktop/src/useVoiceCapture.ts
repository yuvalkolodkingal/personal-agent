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
  const stream = useRef<MediaStream | null>(null);
  const context = useRef<AudioContext | null>(null);
  const source = useRef<MediaStreamAudioSourceNode | null>(null);
  const processor = useRef<ScriptProcessorNode | null>(null);
  const chunks = useRef<Float32Array[]>([]);

  const stop = useCallback(async () => {
    if (!stream.current || !context.current) return;
    setState("transcribing");
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
      const transcript = await invoke<{ text: string }>("voice_transcribe", {
        samples: downsample(merged, sampleRate), sampleRateHz: 16_000,
      });
      onTranscript(transcript.text);
      setState("idle");
    } catch (caught) {
      setError(String(caught));
      setState("error");
    }
  }, [config.voice.mode, onProjection, onTranscript]);

  const start = useCallback(async () => {
    if (state === "listening" || state === "requesting") return;
    setState("requesting");
    setError("");
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
      const audioContext = new AudioContext();
      const input = audioContext.createMediaStreamSource(media);
      const node = audioContext.createScriptProcessor(4096, 1, 1);
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
      node.connect(audioContext.destination);
      stream.current = media;
      context.current = audioContext;
      source.current = input;
      processor.current = node;
      const projection = await invoke<Projection>("microphone_state", { active: true, mode: config.voice.mode });
      onProjection(projection);
      setState("listening");
    } catch (caught) {
      setError(String(caught));
      setState("error");
    }
  }, [config.voice, onProjection, state]);

  useEffect(() => () => {
    processor.current?.disconnect();
    source.current?.disconnect();
    stream.current?.getTracks().forEach((track) => track.stop());
    void context.current?.close();
  }, []);

  return { state, error, level, start, stop };
}
