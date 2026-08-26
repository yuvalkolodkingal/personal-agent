//! Deterministic audio orchestration benchmark; physical-device runs use the same schema.

use async_trait::async_trait;
use personal_agent_audio::{
    AudioError, AudioFrame, EnrolledWakeDetector, MicrophoneState, NetworkPolicy, SpeechRecognizer,
    SpeechSynthesizer, Transcript, VoicePipeline, WakeTemplate, summarize_latencies,
};
use serde_json::json;
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
    }
    let report = json!({
        "schema_version": 1, "measurement": "deterministic-replay", "network": "disabled",
        "hotkey_to_listening": summarize_latencies(&hotkey)?,
        "wake_detection_to_listening": summarize_latencies(&wake)?,
        "internal_speaker_stop": summarize_latencies(&stop)?,
        "offline_deterministic_command": summarize_latencies(&offline)?,
        "external_hardware_required": ["end_to_end_barge_in", "cloud_first_audio", "idle_cpu", "idle_resident_memory", "warm_ui_startup"]
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    black_box(Duration::ZERO);
    Ok(())
}
