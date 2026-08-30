//! Deterministic audio orchestration benchmark; physical-device runs use the same schema.

use async_trait::async_trait;
use personal_agent_audio::{
    AudioError, AudioFrame, EnrolledWakeDetector, MicrophoneState, NetworkPolicy,
    NeuralVoiceRuntime, SpeechRecognizer, SpeechSynthesizer, Transcript, VoicePipeline,
    WakeTemplate, summarize_latencies,
};
use serde_json::{Value, json};
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct LocalRecognizer;
#[async_trait]
impl SpeechRecognizer for LocalRecognizer {
    fn is_local(&self) -> bool {
        true
    }
    async fn transcribe(
        &self,
        _: &[AudioFrame],
        _: Option<&str>,
        _: &[String],
    ) -> Result<Transcript, AudioError> {
        Ok(Transcript {
            text: "status".into(),
            final_result: true,
            confidence: Some(1.0),
            language: Some("en".into()),
        })
    }
}

struct LocalSynthesizer {
    stopped: AtomicBool,
}
#[async_trait]
impl SpeechSynthesizer for LocalSynthesizer {
    fn is_local(&self) -> bool {
        true
    }
    async fn synthesize(&self, _: &str, _: &str) -> Result<Vec<AudioFrame>, AudioError> {
        Ok(vec![tone_frame()])
    }
    async fn stop(&self) -> Result<(), AudioError> {
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }
}

fn tone_frame() -> AudioFrame {
    AudioFrame {
        samples: (0..320)
            .scan(0.0_f32, |phase, _| {
                let sample = phase.sin() * 0.25;
                *phase += 0.125;
                Some(sample)
            })
            .collect(),
        sample_rate_hz: 16_000,
        channels: 1,
        monotonic_time_ms: 0,
    }
}

const STARTUP_STATE_REPLAY: &str = r#"{
  "config":{"profile":"default","runtime":{"working_directory":"/workspace"}},
  "goals":[{"id":"goal-1","status":"active"},{"id":"goal-2","status":"queued"}],
  "automations":[{"id":"daily-summary","enabled":true}],
  "capabilities":{"desktop":"available","browser":"available","audio":"available"},
  "mcp":{"servers":[{"name":"filesystem","enabled":true}]}
}"#;

const DESKTOP_SNAPSHOT_REPLAY: &str = r#"{
  "window":{"title":"Personal Agent","bounds":[0,0,1440,900]},
  "nodes":[
    {"role":"application","name":"Personal Agent","children":[1,2]},
    {"role":"textbox","name":"Message","bounds":[320,720,800,48]},
    {"role":"button","name":"Send","bounds":[1136,720,96,48]}
  ]
}"#;

fn replay_startup_native_setup() -> Result<(), serde_json::Error> {
    let state: Value = serde_json::from_str(STARTUP_STATE_REPLAY)?;
    let replayed_entries = state
        .get("goals")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
        + state
            .get("automations")
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
        + state
            .pointer("/mcp/servers")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
    black_box((state, replayed_entries));
    Ok(())
}

fn replay_bootstrap_ipc() -> Result<(), serde_json::Error> {
    let payload = json!({
        "config": {"profile": "default", "voice": {"enabled": true}},
        "projection": {"sequence": 100, "active_session": "session-replay"},
        "history": (0..100)
            .map(|sequence| json!({"sequence": sequence, "type": "response.delta"}))
            .collect::<Vec<_>>(),
        "voice": {"state": "ready"}
    });
    let encoded = serde_json::to_vec(&payload)?;
    let decoded: Value = serde_json::from_slice(&encoded)?;
    black_box(decoded);
    Ok(())
}

fn replay_desktop_snapshot_warm() -> Result<(), serde_json::Error> {
    let snapshot: Value = serde_json::from_str(DESKTOP_SNAPSHOT_REPLAY)?;
    let actionable_nodes = snapshot
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|node| {
            matches!(
                node.get("role").and_then(Value::as_str),
                Some("button" | "textbox")
            )
        })
        .count();
    black_box((snapshot, actionable_nodes));
    Ok(())
}

fn replay_tts_first_audio(frame: &[i16]) -> Result<Duration, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut first_audio = Duration::ZERO;
    std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
        let (clause_producer, clause_consumer) = mpsc::sync_channel::<&str>(1);
        let (pcm_producer, pcm_sink) = mpsc::sync_channel(1);
        scope.spawn(move || {
            while let Ok(clause) = clause_consumer.recv() {
                let mut synthesized = frame.to_vec();
                if let Some(sample) = synthesized.first_mut() {
                    *sample = i16::try_from(clause.len()).unwrap_or(i16::MAX);
                }
                if pcm_producer.send(synthesized).is_err() {
                    return;
                }
            }
        });

        clause_producer.send("First sentence.")?;
        let first_frame = pcm_sink.recv()?;
        first_audio = started.elapsed();
        black_box(first_frame.first().copied());
        clause_producer.send("Second sentence.")?;
        clause_producer.send("Third sentence.")?;
        drop(clause_producer);
        for frame in pcm_sink {
            black_box(frame.first().copied());
        }
        Ok(())
    })?;
    Ok(first_audio)
}

fn worker_cpu_ticks(process_id: u32) -> Result<u64, Box<dyn std::error::Error>> {
    if !cfg!(target_os = "linux") {
        return Err("worker CPU-tick replay currently requires Linux /proc".into());
    }
    let stat = std::fs::read_to_string(format!("/proc/{process_id}/stat"))?;
    let fields = stat
        .rsplit_once(") ")
        .ok_or("invalid worker /proc stat")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let user_ticks = fields
        .get(11)
        .ok_or("missing worker user ticks")?
        .parse::<u64>()?;
    let system_ticks = fields
        .get(12)
        .ok_or("missing worker system ticks")?
        .parse::<u64>()?;
    Ok(user_ticks.saturating_add(system_ticks))
}

fn pcm16le_samples(path: &Path) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    if bytes.is_empty() || !bytes.len().is_multiple_of(2) {
        return Err("voice replay PCM must be non-empty signed PCM16LE".into());
    }
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|sample| f32::from(i16::from_le_bytes(*sample)) / 32_768.0)
        .collect())
}

#[derive(Default)]
struct WakeReplayObservation {
    detected: bool,
    maximum_score: f64,
    event_latencies: Vec<Duration>,
}

struct EndpointReplayObservation {
    decision_latency: Duration,
    maximum_speech_probability: f64,
    decision: String,
    complete: bool,
}

async fn replay_wake_once(
    worker: &mut NeuralVoiceRuntime,
    samples: &[f32],
    timeout: Duration,
) -> Result<WakeReplayObservation, Box<dyn std::error::Error>> {
    worker
        .request(
            "wake_start",
            json!({"phrases": ["hey jarvis"], "threshold_milli": 930}),
            timeout,
        )
        .await?;
    let mut observation = WakeReplayObservation::default();
    for chunk in samples.chunks(1_280) {
        let started = Instant::now();
        let result = worker
            .request(
                "wake_chunk",
                json!({"samples": chunk, "sample_rate_hz": 16_000}),
                timeout,
            )
            .await?;
        observation.maximum_score = observation
            .maximum_score
            .max(result.get("score").and_then(Value::as_f64).unwrap_or(0.0));
        if result.get("wake").and_then(Value::as_bool) == Some(true) {
            observation.event_latencies.push(started.elapsed());
            observation.detected = true;
        }
    }
    worker.request("wake_stop", json!({}), timeout).await?;
    Ok(observation)
}

async fn replay_stt_once(
    worker: &mut NeuralVoiceRuntime,
    wav_path: &Path,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = worker
        .request(
            "stt_transcribe",
            json!({"wav": wav_path, "vocabulary": []}),
            timeout,
        )
        .await?;
    if result.get("moonshine_thread_mode").and_then(Value::as_str) != Some("default") {
        return Err("batch Moonshine did not use its normal throughput mode".into());
    }
    Ok(())
}

async fn replay_endpoint_once(
    worker: &mut NeuralVoiceRuntime,
    samples: &[f32],
    timeout: Duration,
) -> Result<EndpointReplayObservation, Box<dyn std::error::Error>> {
    const START_THRESHOLD: f64 = 0.6;
    const STOP_THRESHOLD: f64 = 0.35;
    const SILERO_WINDOW_SAMPLES: usize = 512;

    let started = worker
        .request("stt_start", json!({"vocabulary": []}), timeout)
        .await?;
    if started.get("moonshine_thread_mode").and_then(Value::as_str) != Some("single") {
        return Err("streaming Moonshine did not recreate in endpoint-safe mode".into());
    }
    let mut maximum_speech_probability = 0.0_f64;
    let mut speech_seen = false;
    let replay = samples
        .iter()
        .copied()
        .chain(std::iter::repeat_n(0.0_f32, 16_000))
        .collect::<Vec<_>>();
    for chunk in replay.chunks(SILERO_WINDOW_SAMPLES) {
        let mut frame = [0.0_f32; SILERO_WINDOW_SAMPLES];
        frame[..chunk.len()].copy_from_slice(chunk);
        // This starts at the first sample of the candidate silence frame, so
        // the measurement includes real Silero inference plus exactly one
        // real Smart Turn request made by the endpoint-fusion path.
        let frame_started = Instant::now();
        let result = worker
            .request(
                "stt_chunk",
                json!({"samples": frame.as_slice(), "sample_rate_hz": 16_000}),
                timeout,
            )
            .await?;
        if result.get("vad_model").and_then(Value::as_str) != Some("silero-vad-v5.1.2")
            || result.get("vad_frames").and_then(Value::as_u64) != Some(1)
        {
            return Err("worker did not exercise the pinned Silero v5 512-sample contract".into());
        }
        let speech_probability = result
            .get("speech_prob")
            .and_then(Value::as_f64)
            .ok_or("worker omitted Silero speech probability")?;
        maximum_speech_probability = maximum_speech_probability.max(speech_probability);
        if speech_probability >= START_THRESHOLD {
            speech_seen = true;
            continue;
        }
        if !speech_seen || speech_probability > STOP_THRESHOLD {
            continue;
        }

        let endpoint = worker
            .request("turn_complete", json!({"threshold": 0.5}), timeout)
            .await?;
        let decision_latency = frame_started.elapsed();
        let decision = endpoint
            .get("decision")
            .and_then(Value::as_str)
            .ok_or("endpoint worker omitted its decision provenance")?
            .to_owned();
        let complete = endpoint
            .get("complete")
            .and_then(Value::as_bool)
            .ok_or("endpoint worker omitted the completion decision")?;
        worker.request("stt_cancel", json!({}), timeout).await?;
        return Ok(EndpointReplayObservation {
            decision_latency,
            maximum_speech_probability,
            decision,
            complete,
        });
    }
    let _ = worker.request("stt_cancel", json!({}), timeout).await;
    Err("Silero v5 replay did not observe speech followed by acoustic silence".into())
}

async fn measure_wake_cpu_replay(
    worker: &mut NeuralVoiceRuntime,
    process_id: u32,
    samples: &[f32],
    wav_path: &Path,
    timeout: Duration,
    repeats: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    replay_wake_once(worker, samples, timeout).await?;
    replay_stt_once(worker, wav_path, timeout).await?;
    let wake_ticks_before = worker_cpu_ticks(process_id)?;
    let mut wake_response_latencies = Vec::new();
    let mut detected = false;
    let mut maximum_score = 0.0_f64;
    for _ in 0..repeats {
        let observation = replay_wake_once(worker, samples, timeout).await?;
        maximum_score = maximum_score.max(observation.maximum_score);
        wake_response_latencies.extend(observation.event_latencies);
        detected |= observation.detected;
    }
    let wake_cpu_ticks = worker_cpu_ticks(process_id)?.saturating_sub(wake_ticks_before);
    let stt_ticks_before = worker_cpu_ticks(process_id)?;
    for _ in 0..repeats {
        replay_stt_once(worker, wav_path, timeout).await?;
    }
    let stt_cpu_ticks = worker_cpu_ticks(process_id)?.saturating_sub(stt_ticks_before);
    if !detected {
        return Err(format!(
            "pinned openWakeWord model did not detect replay (maximum score {maximum_score})"
        )
        .into());
    }
    let reduction_ratio = stt_cpu_ticks.to_string().parse::<f64>()?
        / wake_cpu_ticks.max(1).to_string().parse::<f64>()?;
    if reduction_ratio < 5.0 {
        return Err(format!(
            "real worker CPU replay improved only {reduction_ratio:.2}x; expected at least 5x"
        )
        .into());
    }
    let latency = summarize_latencies(&wake_response_latencies)?;
    if latency.maximum_microseconds >= 250_000 {
        return Err("wake-to-listen worker replay exceeded 250 ms".into());
    }
    Ok(json!({
        "status": "measured",
        "measurement": "Linux worker user+system CPU ticks over identical replay PCM",
        "sample_count": repeats,
        "openwakeword_cpu_ticks": wake_cpu_ticks,
        "legacy_stt_cpu_ticks": stt_cpu_ticks,
        "reduction_ratio": reduction_ratio,
        "minimum_reduction_ratio": 5.0,
        "wake_event_response": latency,
        "maximum_score": maximum_score,
    }))
}

async fn measure_endpoint_replay(
    worker: &mut NeuralVoiceRuntime,
    samples: &[f32],
    timeout: Duration,
    repeats: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let warm_endpoint = replay_endpoint_once(worker, samples, timeout).await?;
    if warm_endpoint.decision != "smart-turn" {
        return Err("endpoint replay did not consult the installed Smart Turn model".into());
    }
    let mut endpoint_latencies = Vec::new();
    let mut decisions = Vec::new();
    let mut maximum_speech_probability = warm_endpoint.maximum_speech_probability;
    for _ in 0..repeats {
        let observation = replay_endpoint_once(worker, samples, timeout).await?;
        maximum_speech_probability =
            maximum_speech_probability.max(observation.maximum_speech_probability);
        endpoint_latencies.push(observation.decision_latency);
        decisions.push(json!({
            "decision": observation.decision,
            "complete": observation.complete,
        }));
    }
    let endpoint_latency = summarize_latencies(&endpoint_latencies)?;
    if endpoint_latency.maximum_microseconds >= 250_000 {
        return Err("Silero + Smart Turn endpoint replay exceeded 250 ms".into());
    }
    Ok(json!({
        "status": "measured",
        "measurement": "real pinned Silero v5 512-sample frame followed by one real Smart Turn v3.2 request",
        "vad_model": "silero-vad-v5.1.2",
        "vad_start_probability": 0.6,
        "vad_stop_probability": 0.35,
        "smart_turn_consultations_per_silence": 1,
        "maximum_speech_probability": maximum_speech_probability,
        "endpoint_decision": endpoint_latency,
        "decisions": decisions,
    }))
}

async fn actual_worker_voice_replays() -> Result<(Value, Value), Box<dyn std::error::Error>> {
    let Some(root) = std::env::var_os("PERSONAL_AGENT_VOICE_REPLAY_ROOT").map(PathBuf::from) else {
        let external = json!({
            "status": "external-model-assets-required",
            "command": "PERSONAL_AGENT_VOICE_REPLAY_ROOT=<neural-root> PERSONAL_AGENT_VOICE_REPLAY_PYTHON=<venv-python> PERSONAL_AGENT_VOICE_REPLAY_PCM=<signed-pcm16le> PERSONAL_AGENT_VOICE_REPLAY_WAV=<pcm16-wav> cargo run -p personal-agent-audio --bin audio-benchmark --quiet"
        });
        return Ok((external.clone(), external));
    };
    let python = PathBuf::from(
        std::env::var_os("PERSONAL_AGENT_VOICE_REPLAY_PYTHON")
            .ok_or("PERSONAL_AGENT_VOICE_REPLAY_PYTHON is required")?,
    );
    let pcm_path = PathBuf::from(
        std::env::var_os("PERSONAL_AGENT_VOICE_REPLAY_PCM")
            .ok_or("PERSONAL_AGENT_VOICE_REPLAY_PCM is required")?,
    );
    let wav_path = PathBuf::from(
        std::env::var_os("PERSONAL_AGENT_VOICE_REPLAY_WAV")
            .ok_or("PERSONAL_AGENT_VOICE_REPLAY_WAV is required")?,
    );
    let replay_parent = root
        .parent()
        .ok_or("voice replay root has no parent")?
        .canonicalize()?;
    let wav_path = wav_path.canonicalize()?;
    if !wav_path.starts_with(&replay_parent) {
        return Err("voice replay WAV must be inside the neural root's parent".into());
    }
    let samples = pcm16le_samples(&pcm_path)?;
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/voice-runtime.py")
        .canonicalize()?;
    let mut worker = NeuralVoiceRuntime::start(&python, &script, &root).await?;
    let process_id = worker
        .process_id()
        .ok_or("voice worker has no process id")?;
    let timeout = Duration::from_secs(120);
    let repeats = 5_u64;

    let ambient_armed_cpu_replay = measure_wake_cpu_replay(
        &mut worker,
        process_id,
        &samples,
        &wav_path,
        timeout,
        repeats,
    )
    .await?;
    let stt_endpoint_replay =
        measure_endpoint_replay(&mut worker, &samples, timeout, repeats).await?;
    worker.terminate();
    Ok((ambient_armed_cpu_replay, stt_endpoint_replay))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    const SAMPLES: usize = 100;
    let frame = tone_frame();
    let template = WakeTemplate::enroll("jarvis", std::slice::from_ref(&frame))?;
    let pipeline = VoicePipeline::new(
        LocalRecognizer,
        LocalSynthesizer {
            stopped: AtomicBool::new(false),
        },
        NetworkPolicy::Disabled,
    );
    let mut hotkey = Vec::with_capacity(SAMPLES);
    let mut wake = Vec::with_capacity(SAMPLES);
    let mut stop = Vec::with_capacity(SAMPLES);
    let mut offline = Vec::with_capacity(SAMPLES);
    let mut startup_native_setup = Vec::with_capacity(SAMPLES);
    let mut bootstrap_ipc = Vec::with_capacity(SAMPLES);
    let mut desktop_snapshot_warm = Vec::with_capacity(SAMPLES);
    let mut tts_first_audio = Vec::with_capacity(SAMPLES);
    let tts_frame = vec![512_i16; 480];
    for _ in 0..SAMPLES {
        let started = Instant::now();
        black_box(MicrophoneState::Listening);
        hotkey.push(started.elapsed());
        let mut detector = EnrolledWakeDetector::new(vec![template.clone()], 1, 1, 0)?;
        let started = Instant::now();
        black_box(detector.observe(std::slice::from_ref(&frame), 1));
        wake.push(started.elapsed());
        let started = Instant::now();
        pipeline
            .run_turn(
                "jarvis",
                std::slice::from_ref(&frame),
                Some("en"),
                &[],
                |_| "ready".into(),
            )
            .await?;
        offline.push(started.elapsed());
        stop.push(pipeline.barge_in().await?);

        let started = Instant::now();
        replay_startup_native_setup()?;
        startup_native_setup.push(started.elapsed());

        let started = Instant::now();
        replay_bootstrap_ipc()?;
        bootstrap_ipc.push(started.elapsed());

        let started = Instant::now();
        replay_desktop_snapshot_warm()?;
        desktop_snapshot_warm.push(started.elapsed());

        tts_first_audio.push(replay_tts_first_audio(&tts_frame)?);
    }
    let (ambient_armed_cpu_replay, stt_endpoint_replay) = actual_worker_voice_replays().await?;
    let report = json!({
        "schema_version": 1, "measurement": "deterministic-replay", "network": "disabled",
        "hotkey_to_listening": summarize_latencies(&hotkey)?,
        "wake_detection_to_listening": summarize_latencies(&wake)?,
        "internal_speaker_stop": summarize_latencies(&stop)?,
        "offline_deterministic_command": summarize_latencies(&offline)?,
        "startup_native_setup": summarize_latencies(&startup_native_setup)?,
        "bootstrap_ipc": summarize_latencies(&bootstrap_ipc)?,
        "desktop_snapshot_warm": summarize_latencies(&desktop_snapshot_warm)?,
        "tts_first_audio_ms": summarize_latencies(&tts_first_audio)?,
        "ambient_armed_cpu_replay": ambient_armed_cpu_replay,
        "stt_endpoint_replay": stt_endpoint_replay,
        "replay_scope": {
            "startup_native_setup": "serialized native-state replay; excludes window-system and physical device probes",
            "bootstrap_ipc": "JSON encode/decode replay; excludes WebView transport and paint",
            "desktop_snapshot_warm": "serialized accessibility-tree replay; excludes physical screen capture and input",
            "tts_first_audio_ms": "three-clause fake-engine turn through a one-clause prebuffer and bounded in-memory PCM sink; excludes physical device startup",
            "ambient_armed_cpu": "real pinned worker/model paths when replay env vars are set; otherwise reported as external-model-assets-required",
            "stt_endpoint": "real pinned Silero v5 recurrent-state inference plus one Smart Turn v3.2 consultation when replay env vars are set"
        },
        "replay_disclaimer": "Replay numbers are not physical microphone, speaker, network, screen-capture, or UI-startup measurements.",
        "external_hardware_required": ["end_to_end_barge_in", "cloud_first_audio", "idle_cpu", "idle_resident_memory", "warm_ui_startup", "physical_desktop_snapshot"]
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    black_box(Duration::ZERO);
    Ok(())
}
