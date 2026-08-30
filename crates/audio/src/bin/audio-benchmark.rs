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
    worker
        .request(
            "stt_transcribe",
            json!({"wav": wav_path, "vocabulary": []}),
            timeout,
        )
        .await?;
    Ok(())
}

async fn actual_worker_wake_cpu_replay() -> Result<Value, Box<dyn std::error::Error>> {
    let Some(root) = std::env::var_os("PERSONAL_AGENT_VOICE_REPLAY_ROOT").map(PathBuf::from) else {
        return Ok(json!({
            "status": "external-model-assets-required",
            "command": "PERSONAL_AGENT_VOICE_REPLAY_ROOT=<neural-root> PERSONAL_AGENT_VOICE_REPLAY_PYTHON=<venv-python> PERSONAL_AGENT_VOICE_REPLAY_PCM=<signed-pcm16le> PERSONAL_AGENT_VOICE_REPLAY_WAV=<pcm16-wav> cargo run -p personal-agent-audio --bin audio-benchmark --quiet"
        }));
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

    // Warm both real model paths before comparing their steady-state worker CPU.
    replay_wake_once(&mut worker, &samples, timeout).await?;
    replay_stt_once(&mut worker, &wav_path, timeout).await?;

    let wake_ticks_before = worker_cpu_ticks(process_id)?;
    let mut wake_response_latencies = Vec::new();
    let mut detected = false;
    let mut maximum_score = 0.0_f64;
    for _ in 0..repeats {
        let observation = replay_wake_once(&mut worker, &samples, timeout).await?;
        maximum_score = maximum_score.max(observation.maximum_score);
        wake_response_latencies.extend(observation.event_latencies);
        detected |= observation.detected;
    }
    let wake_cpu_ticks = worker_cpu_ticks(process_id)?.saturating_sub(wake_ticks_before);

    let stt_ticks_before = worker_cpu_ticks(process_id)?;
    for _ in 0..repeats {
        replay_stt_once(&mut worker, &wav_path, timeout).await?;
    }
    let stt_cpu_ticks = worker_cpu_ticks(process_id)?.saturating_sub(stt_ticks_before);
    worker.terminate();

    if !detected {
        return Err(format!(
            "pinned openWakeWord model did not detect replay (maximum score {maximum_score})"
        )
        .into());
    }
    let stt_cpu_ticks_float = stt_cpu_ticks.to_string().parse::<f64>()?;
    let wake_cpu_ticks_float = wake_cpu_ticks.max(1).to_string().parse::<f64>()?;
    let reduction_ratio = stt_cpu_ticks_float / wake_cpu_ticks_float;
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
    }
    let ambient_armed_cpu_replay = actual_worker_wake_cpu_replay().await?;
    let report = json!({
        "schema_version": 1, "measurement": "deterministic-replay", "network": "disabled",
        "hotkey_to_listening": summarize_latencies(&hotkey)?,
        "wake_detection_to_listening": summarize_latencies(&wake)?,
        "internal_speaker_stop": summarize_latencies(&stop)?,
        "offline_deterministic_command": summarize_latencies(&offline)?,
        "startup_native_setup": summarize_latencies(&startup_native_setup)?,
        "bootstrap_ipc": summarize_latencies(&bootstrap_ipc)?,
        "desktop_snapshot_warm": summarize_latencies(&desktop_snapshot_warm)?,
        "ambient_armed_cpu_replay": ambient_armed_cpu_replay,
        "replay_scope": {
            "startup_native_setup": "serialized native-state replay; excludes window-system and physical device probes",
            "bootstrap_ipc": "JSON encode/decode replay; excludes WebView transport and paint",
            "desktop_snapshot_warm": "serialized accessibility-tree replay; excludes physical screen capture and input",
            "ambient_armed_cpu": "real pinned worker/model paths when replay env vars are set; otherwise reported as external-model-assets-required"
        },
        "replay_disclaimer": "Replay numbers are not physical microphone, speaker, network, screen-capture, or UI-startup measurements.",
        "external_hardware_required": ["end_to_end_barge_in", "cloud_first_audio", "idle_cpu", "idle_resident_memory", "warm_ui_startup", "physical_desktop_snapshot"]
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    black_box(Duration::ZERO);
    Ok(())
}
