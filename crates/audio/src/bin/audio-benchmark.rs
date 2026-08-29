//! Deterministic audio orchestration benchmark; physical-device runs use the same schema.

use async_trait::async_trait;
use personal_agent_audio::{
    AudioError, AudioFrame, EnrolledWakeDetector, MicrophoneState, NetworkPolicy, SpeechRecognizer,
    SpeechSynthesizer, Transcript, VoicePipeline, WakeTemplate, summarize_latencies,
};
use serde_json::{Value, json};
use std::hint::black_box;
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
    let report = json!({
        "schema_version": 1, "measurement": "deterministic-replay", "network": "disabled",
        "hotkey_to_listening": summarize_latencies(&hotkey)?,
        "wake_detection_to_listening": summarize_latencies(&wake)?,
        "internal_speaker_stop": summarize_latencies(&stop)?,
        "offline_deterministic_command": summarize_latencies(&offline)?,
        "startup_native_setup": summarize_latencies(&startup_native_setup)?,
        "bootstrap_ipc": summarize_latencies(&bootstrap_ipc)?,
        "desktop_snapshot_warm": summarize_latencies(&desktop_snapshot_warm)?,
        "replay_scope": {
            "startup_native_setup": "serialized native-state replay; excludes window-system and physical device probes",
            "bootstrap_ipc": "JSON encode/decode replay; excludes WebView transport and paint",
            "desktop_snapshot_warm": "serialized accessibility-tree replay; excludes physical screen capture and input"
        },
        "replay_disclaimer": "Replay numbers are not physical microphone, speaker, network, screen-capture, or UI-startup measurements.",
        "external_hardware_required": ["end_to_end_barge_in", "cloud_first_audio", "idle_cpu", "idle_resident_memory", "warm_ui_startup", "physical_desktop_snapshot"]
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    black_box(Duration::ZERO);
    Ok(())
}
