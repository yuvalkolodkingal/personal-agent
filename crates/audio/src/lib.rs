//! Replaceable audio pipeline contracts and deterministic endpointing primitives.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
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
}
