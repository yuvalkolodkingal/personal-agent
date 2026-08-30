//! Native streaming playback with prompt, generation-safe cancellation.

use crate::AudioError;
use cpal::traits::{DeviceTrait, HostTrait};
use rodio::buffer::SamplesBuffer;
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink};
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Why one native playback session ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackEnd {
    /// Every queued source reached the output device.
    Completed,
    /// The queue was discarded by an explicit interruption.
    Stopped,
}

struct CompletionSignal {
    sender: Mutex<Option<oneshot::Sender<PlaybackEnd>>>,
}

impl CompletionSignal {
    fn send(&self, outcome: PlaybackEnd) {
        if let Ok(mut sender) = self.sender.lock()
            && let Some(sender) = sender.take()
        {
            let _ = sender.send(outcome);
        }
    }
}

/// Thread-safe control retained by the desktop host while synthesis is active.
///
/// The control does not own the cpal stream. [`NativePlaybackSink`] keeps that
/// stream alive until synthesis is finished, then its completion thread owns it
/// until the queue drains or this control stops the rodio sink.
#[derive(Clone)]
pub struct NativePlaybackControl {
    sink: Arc<Sink>,
    completion: Arc<CompletionSignal>,
    base_volume: f32,
    ducking_percent: u16,
    stopped: Arc<AtomicBool>,
}

impl NativePlaybackControl {
    /// Queue one signed 16-bit interleaved PCM frame immediately.
    ///
    /// # Errors
    ///
    /// Rejects empty frames, invalid formats, and appends after interruption.
    pub fn append_pcm(
        &self,
        samples: &[i16],
        sample_rate_hz: u32,
        channels: u16,
    ) -> Result<(), AudioError> {
        append_pcm_to_sink(&self.sink, &self.stopped, samples, sample_rate_hz, channels)
    }

    /// Discard buffered audio synchronously and wake completion waiters.
    #[must_use]
    pub fn stop(&self) -> Duration {
        let started = Instant::now();
        self.stopped.store(true, Ordering::SeqCst);
        self.sink.stop();
        self.completion.send(PlaybackEnd::Stopped);
        started.elapsed()
    }

    /// Apply or remove the configured STT-capture ducking reduction.
    pub fn set_capturing(&self, capturing: bool) {
        self.sink.set_volume(effective_volume(
            self.base_volume,
            self.ducking_percent,
            capturing,
        ));
    }

    /// Whether an interruption has already invalidated this playback session.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

/// The producing side of one native rodio playback queue.
pub struct NativePlaybackSink {
    sink: Arc<Sink>,
    stream: Option<OutputStream>,
    completion: Arc<CompletionSignal>,
    stopped: Arc<AtomicBool>,
    finished: bool,
}

impl NativePlaybackSink {
    /// Open the selected cpal output device and create an empty rodio queue.
    ///
    /// # Errors
    ///
    /// Returns an explicit unavailable error when cpal cannot discover or open
    /// the configured output. The desktop host may then use its `pw-play`
    /// compatibility fallback.
    pub fn open(
        output_device: &str,
        volume_percent: u16,
        ducking_percent: u16,
        capturing: bool,
    ) -> Result<(Self, NativePlaybackControl, oneshot::Receiver<PlaybackEnd>), AudioError> {
        let device = select_output_device(output_device)?;
        let device_name = device
            .name()
            .unwrap_or_else(|_| "configured output".to_owned());
        let stream = OutputStreamBuilder::from_device(device)
            .and_then(|builder| builder.open_stream_or_fallback())
            .map_err(|error| {
                AudioError::Unavailable(format!(
                    "cpal could not open audio output device `{device_name}`: {error}"
                ))
            })?;
        let sink = Arc::new(Sink::connect_new(stream.mixer()));
        let (completion_sender, completion_receiver) = oneshot::channel();
        let completion = Arc::new(CompletionSignal {
            sender: Mutex::new(Some(completion_sender)),
        });
        let stopped = Arc::new(AtomicBool::new(false));
        let base_volume = f32::from(volume_percent.min(200)) / 100.0;
        let control = NativePlaybackControl {
            sink: Arc::clone(&sink),
            completion: Arc::clone(&completion),
            base_volume,
            ducking_percent: ducking_percent.min(100),
            stopped: Arc::clone(&stopped),
        };
        control.set_capturing(capturing);
        Ok((
            Self {
                sink,
                stream: Some(stream),
                completion,
                stopped,
                finished: false,
            },
            control,
            completion_receiver,
        ))
    }

    /// Queue one signed 16-bit interleaved PCM frame immediately.
    ///
    /// # Errors
    ///
    /// Rejects empty frames, invalid formats, and appends after interruption.
    pub fn append_pcm(
        &self,
        samples: &[i16],
        sample_rate_hz: u32,
        channels: u16,
    ) -> Result<(), AudioError> {
        append_pcm_to_sink(&self.sink, &self.stopped, samples, sample_rate_hz, channels)
    }

    /// Decode and queue an existing WAV produced by the compatibility engine.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be opened or decoded.
    pub fn append_wav(&self, path: &Path) -> Result<(), AudioError> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err(AudioError::Processing(
                "native playback was interrupted".into(),
            ));
        }
        let file = File::open(path).map_err(|error| AudioError::Processing(error.to_string()))?;
        let decoded = Decoder::try_from(file)
            .map_err(|error| AudioError::Processing(format!("invalid playback WAV: {error}")))?;
        self.sink.append(decoded);
        Ok(())
    }

    /// Seal the queue and let a dedicated sink thread signal natural completion.
    ///
    /// # Errors
    ///
    /// Returns an error only when the completion thread cannot be created.
    pub fn finish(mut self) -> Result<(), AudioError> {
        let stream = self.stream.take().ok_or_else(|| {
            AudioError::Processing("native playback stream was already finished".into())
        })?;
        let sink = Arc::clone(&self.sink);
        let completion = Arc::clone(&self.completion);
        let completion_on_error = Arc::clone(&self.completion);
        let stopped = Arc::clone(&self.stopped);
        let spawned = std::thread::Builder::new()
            .name("voice-playback-sink".into())
            .spawn(move || {
                sink.sleep_until_end();
                drop(stream);
                completion.send(if stopped.load(Ordering::SeqCst) {
                    PlaybackEnd::Stopped
                } else {
                    PlaybackEnd::Completed
                });
            })
            .map(|_| ());
        match spawned {
            Ok(()) => {
                self.finished = true;
                Ok(())
            }
            Err(error) => {
                completion_on_error.send(PlaybackEnd::Stopped);
                Err(AudioError::Processing(error.to_string()))
            }
        }
    }
}

/// Return the cpal output device selected by the current voice configuration.
///
/// This is an enumeration-only status probe; playback still reports a precise
/// open error and activates the subprocess fallback if the selected device
/// cannot create a stream.
///
/// # Errors
///
/// Returns an explicit unavailable reason when cpal cannot enumerate a usable
/// default/first output or cannot match the configured device name.
pub fn native_output_device_name(output_device: &str) -> Result<String, AudioError> {
    let device = select_output_device(output_device)?;
    device.default_output_config().map_err(|error| {
        AudioError::Unavailable(format!(
            "cpal output has no supported default stream configuration: {error}"
        ))
    })?;
    device.name().map_err(|error| {
        AudioError::Unavailable(format!("cpal output has no usable name: {error}"))
    })
}

fn select_output_device(output_device: &str) -> Result<cpal::Device, AudioError> {
    let host = cpal::default_host();
    let requested = output_device.trim();
    if requested.is_empty() {
        if let Some(device) = host.default_output_device() {
            return Ok(device);
        }
        return host
            .output_devices()
            .map_err(|error| {
                AudioError::Unavailable(format!(
                    "cpal could not enumerate audio output devices: {error}"
                ))
            })?
            .next()
            .ok_or_else(|| AudioError::Unavailable("cpal found no audio output device".into()));
    }
    host.output_devices()
        .map_err(|error| {
            AudioError::Unavailable(format!(
                "cpal could not enumerate audio output devices: {error}"
            ))
        })?
        .find(|device| device.name().is_ok_and(|name| name == requested))
        .ok_or_else(|| {
            AudioError::Unavailable(format!("cpal output device `{requested}` is unavailable"))
        })
}

impl Drop for NativePlaybackSink {
    fn drop(&mut self) {
        if !self.finished {
            self.stopped.store(true, Ordering::SeqCst);
            self.sink.stop();
            self.completion.send(PlaybackEnd::Stopped);
        }
    }
}

fn effective_volume(base_volume: f32, ducking_percent: u16, capturing: bool) -> f32 {
    if capturing {
        base_volume * (1.0 - f32::from(ducking_percent.min(100)) / 100.0)
    } else {
        base_volume
    }
}

fn append_pcm_to_sink(
    sink: &Sink,
    stopped: &AtomicBool,
    samples: &[i16],
    sample_rate_hz: u32,
    channels: u16,
) -> Result<(), AudioError> {
    if samples.is_empty()
        || !(8_000..=192_000).contains(&sample_rate_hz)
        || channels == 0
        || !samples.len().is_multiple_of(usize::from(channels))
    {
        return Err(AudioError::Processing(
            "native playback received an invalid PCM frame".into(),
        ));
    }
    if stopped.load(Ordering::SeqCst) {
        return Err(AudioError::Processing(
            "native playback was interrupted".into(),
        ));
    }
    let normalized = samples
        .iter()
        .map(|sample| f32::from(*sample) / 32_768.0)
        .collect::<Vec<_>>();
    sink.append(SamplesBuffer::new(channels, sample_rate_hz, normalized));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_control(
        base_volume: f32,
        ducking_percent: u16,
    ) -> (NativePlaybackControl, oneshot::Receiver<PlaybackEnd>) {
        let (sink, _queue) = Sink::new();
        let (sender, receiver) = oneshot::channel();
        let completion = Arc::new(CompletionSignal {
            sender: Mutex::new(Some(sender)),
        });
        (
            NativePlaybackControl {
                sink: Arc::new(sink),
                completion,
                base_volume,
                ducking_percent,
                stopped: Arc::new(AtomicBool::new(false)),
            },
            receiver,
        )
    }

    #[tokio::test]
    async fn stop_mid_stream_signals_within_fifty_milliseconds() {
        let (control, completion) = fake_control(1.0, 30);
        control
            .sink
            .append(SamplesBuffer::new(1, 24_000, vec![0.25_f32; 24_000]));
        assert_eq!(control.sink.len(), 1);

        let latency = control.stop();
        assert!(latency < Duration::from_millis(50), "{latency:?}");
        assert!(control.is_stopped());
        assert_eq!(
            completion.await.expect("completion signal"),
            PlaybackEnd::Stopped
        );
    }

    #[test]
    fn ducking_percent_reduces_and_restores_sink_volume() {
        assert!((effective_volume(1.2, 30, true) - 0.84).abs() < f32::EPSILON);
        let (control, _completion) = fake_control(1.2, 30);
        control.set_capturing(true);
        assert!((control.sink.volume() - 0.84).abs() < f32::EPSILON);
        control.set_capturing(false);
        assert!((control.sink.volume() - 1.2).abs() < f32::EPSILON);
    }
}
