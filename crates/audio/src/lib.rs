//! Replaceable, privacy-aware audio pipeline and deterministic endpointing primitives.

mod native;
mod neural;

pub use native::{
    NativeVoiceConfig, NativeVoiceStatus, discover_native_voice, play_wav, synthesize_piper,
    transcribe_pcm, transcribe_wav, write_pcm_wav,
};
pub use neural::NeuralVoiceRuntime;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use thiserror::Error;

/// PCM frame passed between capture, VAD, STT, and acoustic processing.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioFrame {
    pub samples: Vec<f32>,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub monotonic_time_ms: u64,
}

/// Visible microphone privacy state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicrophoneState {
    Off,
    WakeOnly,
    Listening,
    Dictation,
    Meeting,
}

/// Partial or final recognition result with honest confidence availability.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
    pub final_result: bool,
    pub confidence: Option<f32>,
    pub language: Option<String>,
}

/// Audio subsystem error that can drive explicit fallback UX.
#[derive(Debug, Error)]
pub enum AudioError {
    #[error("audio capability unavailable: {0}")]
    Unavailable(String),
    #[error("audio device changed: {0}")]
    DeviceChanged(String),
    #[error("audio processing failed: {0}")]
    Processing(String),
    #[error("network-disabled mode rejected a hosted audio adapter")]
    HostedAdapterOffline,
    #[error("no enrolled wake template matched the captured audio")]
    WakeNotDetected,
    #[error("no speech was detected before the capture limit")]
    NoSpeech,
}

/// Network availability is an explicit input; privacy never changes silently.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    Disabled,
    Available,
}

/// Runtime capture mode. Continuous capture is never implied by wake-only mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    WakeOnly,
    Hybrid,
    Continuous,
    PushToTalk,
}

/// One enrolled wake phrase represented by local acoustic features.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WakeTemplate {
    pub phrase: String,
    pub feature_vector: Vec<f32>,
    pub threshold: f32,
}

impl WakeTemplate {
    /// Enroll a phrase from generated or user-provided PCM frames.
    ///
    /// # Errors
    ///
    /// Returns an error for blank phrases or captures without usable audio.
    pub fn enroll(phrase: impl Into<String>, frames: &[AudioFrame]) -> Result<Self, AudioError> {
        let phrase = phrase.into();
        if phrase.trim().is_empty() {
            return Err(AudioError::Processing("wake phrase cannot be blank".into()));
        }
        let feature_vector = acoustic_features(frames);
        if feature_vector
            .iter()
            .all(|value| value.abs() < f32::EPSILON)
        {
            return Err(AudioError::Processing(
                "wake enrollment requires non-silent audio".into(),
            ));
        }
        Ok(Self {
            phrase,
            feature_vector,
            threshold: 0.93,
        })
    }
}

/// Local wake detector with voting and a refractory interval.
#[derive(Clone, Debug)]
pub struct EnrolledWakeDetector {
    templates: Vec<WakeTemplate>,
    voting_window: VecDeque<bool>,
    voting_window_size: usize,
    votes_required: usize,
    refractory_ms: u64,
    last_detection_ms: Option<u64>,
}

impl EnrolledWakeDetector {
    /// Construct a detector. Invalid voting settings are rejected.
    ///
    /// # Errors
    ///
    /// Returns an error when there are no templates or votes cannot fit in the window.
    pub fn new(
        templates: Vec<WakeTemplate>,
        voting_window_size: usize,
        votes_required: usize,
        refractory_ms: u64,
    ) -> Result<Self, AudioError> {
        if templates.is_empty()
            || voting_window_size == 0
            || votes_required == 0
            || votes_required > voting_window_size
        {
            return Err(AudioError::Processing(
                "invalid wake detector settings".into(),
            ));
        }
        Ok(Self {
            templates,
            voting_window: VecDeque::with_capacity(voting_window_size),
            voting_window_size,
            votes_required,
            refractory_ms,
            last_detection_ms: None,
        })
    }

    /// Observe one candidate capture and return the matched phrase after voting.
    pub fn observe(&mut self, frames: &[AudioFrame], now_ms: u64) -> Option<String> {
        if self
            .last_detection_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < self.refractory_ms)
        {
            return None;
        }
        let features = acoustic_features(frames);
        let matched = self
            .templates
            .iter()
            .find(|template| {
                cosine_similarity(&features, &template.feature_vector) >= template.threshold
            })
            .map(|template| template.phrase.clone());
        if self.voting_window.len() == self.voting_window_size {
            self.voting_window.pop_front();
        }
        self.voting_window.push_back(matched.is_some());
        if self.voting_window.iter().filter(|vote| **vote).count() >= self.votes_required {
            self.voting_window.clear();
            self.last_detection_ms = Some(now_ms);
            matched
        } else {
            None
        }
    }
}

fn acoustic_features(frames: &[AudioFrame]) -> Vec<f32> {
    let mut samples = frames
        .iter()
        .flat_map(|frame| frame.samples.iter().copied());
    let Some(first) = samples.next() else {
        return vec![0.0, 0.0, 0.0, 0.0];
    };
    let mut count = 1.0_f32;
    let mut sum_sq = first * first;
    let mut peak = first.abs();
    let mut crossings = 0.0_f32;
    let mut prior = first;
    let mut delta = 0.0_f32;
    for sample in samples {
        count += 1.0;
        sum_sq += sample * sample;
        peak = peak.max(sample.abs());
        if sample.is_sign_positive() != prior.is_sign_positive() {
            crossings += 1.0;
        }
        delta += (sample - prior).abs();
        prior = sample;
    }
    vec![
        (sum_sq / count).sqrt(),
        peak,
        crossings / count,
        delta / count,
    ]
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let dot: f32 = left.iter().zip(right).map(|(a, b)| a * b).sum();
    let left_norm: f32 = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm: f32 = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    if left_norm <= f32::EPSILON || right_norm <= f32::EPSILON {
        0.0
    } else {
        dot / (left_norm * right_norm)
    }
}

/// Bounded pre-roll buffer so the first phonemes survive VAD activation.
#[derive(Clone, Debug)]
pub struct PreRollBuffer {
    frames: VecDeque<AudioFrame>,
    maximum_ms: u64,
}

impl PreRollBuffer {
    #[must_use]
    pub fn new(maximum_ms: u64) -> Self {
        Self {
            frames: VecDeque::new(),
            maximum_ms,
        }
    }

    pub fn push(&mut self, frame: AudioFrame) {
        self.frames.push_back(frame);
        let latest = self.frames.back().map_or(0, |item| item.monotonic_time_ms);
        while self
            .frames
            .front()
            .is_some_and(|item| latest.saturating_sub(item.monotonic_time_ms) > self.maximum_ms)
        {
            self.frames.pop_front();
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<AudioFrame> {
        self.frames.iter().cloned().collect()
    }
}

/// Capture diagnostics used for visible device and clipping state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptureDiagnostics {
    pub peak: f32,
    pub rms: f32,
    pub clipped_samples: usize,
    pub gain: f32,
}

/// Apply bounded input gain and report clipping instead of hiding it.
#[must_use]
pub fn apply_input_gain(frame: &AudioFrame, gain: f32) -> (AudioFrame, CaptureDiagnostics) {
    let gain = gain.clamp(0.0, 8.0);
    let mut clipped_samples = 0;
    let mut sample_count = 0.0_f32;
    let mut sum_sq = 0.0;
    let mut peak = 0.0_f32;
    let samples = frame
        .samples
        .iter()
        .map(|sample| {
            sample_count += 1.0;
            let amplified = *sample * gain;
            if amplified.abs() > 1.0 {
                clipped_samples += 1;
            }
            let bounded = amplified.clamp(-1.0, 1.0);
            sum_sq += bounded * bounded;
            peak = peak.max(bounded.abs());
            bounded
        })
        .collect::<Vec<_>>();
    let rms = if sample_count == 0.0 {
        0.0
    } else {
        (sum_sq / sample_count).sqrt()
    };
    (
        AudioFrame {
            samples,
            ..frame.clone()
        },
        CaptureDiagnostics {
            peak,
            rms,
            clipped_samples,
            gain,
        },
    )
}

/// Echo suppression decision based on normalized capture/playback correlation.
#[must_use]
pub fn is_probable_self_echo(captured: &[f32], playback: &[f32], threshold: f32) -> bool {
    if captured.len() != playback.len() || captured.is_empty() {
        return false;
    }
    cosine_similarity(captured, playback) >= threshold.clamp(0.0, 1.0)
}

/// Local or hosted speech recognizer behind one privacy-aware interface.
#[async_trait]
pub trait SpeechRecognizer: Send + Sync {
    fn is_local(&self) -> bool;
    async fn transcribe(
        &self,
        frames: &[AudioFrame],
        language: Option<&str>,
        vocabulary: &[String],
    ) -> Result<Transcript, AudioError>;
}

/// Streaming speech synthesis; stop must discard buffered audio promptly.
#[async_trait]
pub trait SpeechSynthesizer: Send + Sync {
    fn is_local(&self) -> bool;
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<AudioFrame>, AudioError>;
    async fn stop(&self) -> Result<(), AudioError>;
}

/// A complete local turn result with measured orchestration latencies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OfflineTurn {
    pub wake_phrase: String,
    pub transcript: Transcript,
    pub response_text: String,
    pub audio_frames: usize,
    pub wake_to_listening_ms: u64,
    pub deterministic_command_ms: u64,
}

/// Offline-first orchestration. Hosted adapters are rejected when networking is disabled.
pub struct VoicePipeline<R, S> {
    recognizer: R,
    synthesizer: S,
    network: NetworkPolicy,
}

impl<R, S> VoicePipeline<R, S>
where
    R: SpeechRecognizer,
    S: SpeechSynthesizer,
{
    #[must_use]
    pub fn new(recognizer: R, synthesizer: S, network: NetworkPolicy) -> Self {
        Self {
            recognizer,
            synthesizer,
            network,
        }
    }

    fn enforce_privacy(&self) -> Result<(), AudioError> {
        if self.network == NetworkPolicy::Disabled
            && (!self.recognizer.is_local() || !self.synthesizer.is_local())
        {
            return Err(AudioError::HostedAdapterOffline);
        }
        Ok(())
    }

    /// Execute a wake-to-spoken-response turn using only the configured adapters.
    ///
    /// # Errors
    ///
    /// Propagates privacy, recognition, and synthesis failures.
    pub async fn run_turn<F>(
        &self,
        wake_phrase: impl Into<String>,
        frames: &[AudioFrame],
        language: Option<&str>,
        vocabulary: &[String],
        respond: F,
    ) -> Result<OfflineTurn, AudioError>
    where
        F: FnOnce(&Transcript) -> String,
    {
        self.enforce_privacy()?;
        if frames.is_empty() {
            return Err(AudioError::NoSpeech);
        }
        let started = Instant::now();
        let transcript = self
            .recognizer
            .transcribe(frames, language, vocabulary)
            .await?;
        let response_text = respond(&transcript);
        let recognition_ms = duration_ms(started.elapsed());
        let rendered = self
            .synthesizer
            .synthesize(&response_text, "default")
            .await?;
        Ok(OfflineTurn {
            wake_phrase: wake_phrase.into(),
            transcript,
            response_text,
            audio_frames: rendered.len(),
            wake_to_listening_ms: 0,
            deterministic_command_ms: recognition_ms,
        })
    }

    /// Stop buffered playback and return the internal cancellation latency.
    ///
    /// # Errors
    ///
    /// Propagates a synthesizer cancellation error.
    pub async fn barge_in(&self) -> Result<Duration, AudioError> {
        let started = Instant::now();
        self.synthesizer.stop().await?;
        Ok(started.elapsed())
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Latency distribution in microseconds. Hardware reports can convert units
/// without losing sub-millisecond internal measurements.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LatencySummary {
    pub p50_microseconds: u64,
    pub p95_microseconds: u64,
    pub maximum_microseconds: u64,
    pub sample_count: usize,
}

/// Summarize a non-empty latency sample using nearest-rank percentiles.
///
/// # Errors
///
/// Returns an error when no samples are supplied.
pub fn summarize_latencies(samples: &[Duration]) -> Result<LatencySummary, AudioError> {
    if samples.is_empty() {
        return Err(AudioError::Processing(
            "latency sample cannot be empty".into(),
        ));
    }
    let mut micros = samples
        .iter()
        .map(|sample| u64::try_from(sample.as_micros()).unwrap_or(u64::MAX))
        .collect::<Vec<_>>();
    micros.sort_unstable();
    let percentile = |percent: usize| {
        let index = (micros.len().saturating_sub(1) * percent).div_ceil(100);
        micros[index]
    };
    Ok(LatencySummary {
        p50_microseconds: percentile(50),
        p95_microseconds: percentile(95),
        maximum_microseconds: *micros.last().unwrap_or(&u64::MAX),
        sample_count: micros.len(),
    })
}

/// Hysteretic voice-activity classifier, avoiding noisy threshold chatter.
#[derive(Clone, Debug)]
pub struct VadHysteresis {
    start_threshold: f32,
    stop_threshold: f32,
    speaking: bool,
}

impl VadHysteresis {
    /// # Errors
    ///
    /// Returns an error when thresholds are outside 0–1 or the stop threshold
    /// is not lower than the start threshold.
    pub fn new(start_threshold: f32, stop_threshold: f32) -> Result<Self, AudioError> {
        if !(0.0..=1.0).contains(&stop_threshold)
            || !(0.0..=1.0).contains(&start_threshold)
            || stop_threshold >= start_threshold
        {
            return Err(AudioError::Processing(
                "VAD stop threshold must be lower than start threshold".into(),
            ));
        }
        Ok(Self {
            start_threshold,
            stop_threshold,
            speaking: false,
        })
    }

    pub fn observe(&mut self, probability: f32) -> bool {
        self.speaking = if self.speaking {
            probability > self.stop_threshold
        } else {
            probability >= self.start_threshold
        };
        self.speaking
    }
}

/// Adaptive silence required to end a turn.
#[must_use]
pub fn endpoint_silence_ms(
    spoken_ms: u64,
    short_ms: u64,
    long_ms: u64,
    long_utterance_ms: u64,
) -> u64 {
    if long_utterance_ms == 0 || spoken_ms >= long_utterance_ms {
        return long_ms.max(short_ms);
    }
    short_ms + (long_ms.saturating_sub(short_ms) * spoken_ms / long_utterance_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    struct ReplayRecognizer {
        local: bool,
    }

    #[async_trait]
    impl SpeechRecognizer for ReplayRecognizer {
        fn is_local(&self) -> bool {
            self.local
        }
        async fn transcribe(
            &self,
            frames: &[AudioFrame],
            language: Option<&str>,
            _: &[String],
        ) -> Result<Transcript, AudioError> {
            if frames
                .iter()
                .all(|frame| frame.samples.iter().all(|sample| sample.abs() < 0.01))
            {
                return Err(AudioError::NoSpeech);
            }
            Ok(Transcript {
                text: "what time is it".into(),
                final_result: true,
                confidence: Some(0.99),
                language: language.map(str::to_owned),
            })
        }
    }

    struct ReplaySynthesizer {
        local: bool,
        stopped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl SpeechSynthesizer for ReplaySynthesizer {
        fn is_local(&self) -> bool {
            self.local
        }
        async fn synthesize(&self, _: &str, _: &str) -> Result<Vec<AudioFrame>, AudioError> {
            Ok(vec![tone_frame(0)])
        }
        async fn stop(&self) -> Result<(), AudioError> {
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn tone_frame(at_ms: u64) -> AudioFrame {
        AudioFrame {
            samples: (0..320)
                .scan(0.0_f32, |phase, _| {
                    let sample = (phase.sin() * 0.3).clamp(-1.0, 1.0);
                    *phase += 0.125;
                    Some(sample)
                })
                .collect(),
            sample_rate_hz: 16_000,
            channels: 1,
            monotonic_time_ms: at_ms,
        }
    }
    #[test]
    fn vad_uses_separate_start_and_stop_thresholds() {
        let mut vad = VadHysteresis::new(0.6, 0.35).expect("vad");
        assert!(!vad.observe(0.5));
        assert!(vad.observe(0.7));
        assert!(vad.observe(0.4));
        assert!(!vad.observe(0.3));
    }
    #[test]
    fn endpoint_grows_with_utterance_length() {
        assert_eq!(endpoint_silence_ms(0, 700, 1400, 4000), 700);
        assert_eq!(endpoint_silence_ms(2000, 700, 1400, 4000), 1050);
        assert_eq!(endpoint_silence_ms(8000, 700, 1400, 4000), 1400);
    }

    #[test]
    fn latency_report_contains_required_distribution_fields() {
        let samples = (1..=100).map(Duration::from_micros).collect::<Vec<_>>();
        let report = summarize_latencies(&samples).expect("summary");
        assert_eq!(report.p50_microseconds, 51);
        assert_eq!(report.p95_microseconds, 96);
        assert_eq!(report.maximum_microseconds, 100);
        assert_eq!(report.sample_count, 100);
    }

    #[test]
    fn wake_enrollment_votes_and_observes_refractory_period() {
        let frames = vec![tone_frame(0), tone_frame(20)];
        let template = WakeTemplate::enroll("jarvis", &frames).expect("enroll");
        let mut detector =
            EnrolledWakeDetector::new(vec![template], 3, 2, 2_000).expect("detector");
        assert_eq!(detector.observe(&frames, 100), None);
        assert_eq!(detector.observe(&frames, 120), Some("jarvis".into()));
        assert_eq!(detector.observe(&frames, 140), None);
        assert_eq!(detector.observe(&frames, 2_200), None);
        assert_eq!(detector.observe(&frames, 2_220), Some("jarvis".into()));
    }

    #[test]
    fn pre_roll_gain_and_echo_are_bounded_and_visible() {
        let mut buffer = PreRollBuffer::new(40);
        for at in [0, 20, 40, 60] {
            buffer.push(tone_frame(at));
        }
        assert_eq!(buffer.snapshot().len(), 3);
        let mut loud = tone_frame(0);
        loud.samples[0] = 0.8;
        let (amplified, diagnostics) = apply_input_gain(&loud, 2.0);
        assert!((amplified.samples[0] - 1.0).abs() < f32::EPSILON);
        assert!(diagnostics.clipped_samples > 0);
        assert!(is_probable_self_echo(&[0.2, -0.2], &[0.2, -0.2], 0.99));
    }

    #[tokio::test]
    async fn network_disabled_replay_completes_local_wake_to_speech() {
        let stopped = Arc::new(AtomicBool::new(false));
        let pipeline = VoicePipeline::new(
            ReplayRecognizer { local: true },
            ReplaySynthesizer {
                local: true,
                stopped: Arc::clone(&stopped),
            },
            NetworkPolicy::Disabled,
        );
        let turn = pipeline
            .run_turn("jarvis", &[tone_frame(0)], Some("en"), &[], |_| {
                "It is fixture time.".into()
            })
            .await
            .expect("offline turn");
        assert_eq!(turn.transcript.text, "what time is it");
        assert_eq!(turn.response_text, "It is fixture time.");
        assert_eq!(turn.audio_frames, 1);
        assert!(turn.deterministic_command_ms < 500);
        let stop_latency = pipeline.barge_in().await.expect("barge in");
        assert!(stopped.load(Ordering::SeqCst));
        assert!(stop_latency < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn hosted_adapter_cannot_silently_run_offline() {
        let pipeline = VoicePipeline::new(
            ReplayRecognizer { local: false },
            ReplaySynthesizer {
                local: true,
                stopped: Arc::new(AtomicBool::new(false)),
            },
            NetworkPolicy::Disabled,
        );
        assert!(matches!(
            pipeline
                .run_turn("jarvis", &[tone_frame(0)], None, &[], |_| "answer".into())
                .await,
            Err(AudioError::HostedAdapterOffline)
        ));
    }
}
