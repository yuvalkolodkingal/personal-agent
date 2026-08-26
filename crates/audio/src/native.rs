//! Native offline STT/TTS process adapters used by the desktop host.

use crate::{AudioError, Transcript};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

/// Resolved local voice engines and models. Missing components remain explicit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeVoiceStatus {
    pub stt_ready: bool,
    pub tts_ready: bool,
    pub playback_ready: bool,
    pub whisper_executable: Option<PathBuf>,
    pub whisper_model: Option<PathBuf>,
    pub piper_executable: Option<PathBuf>,
    pub piper_model: Option<PathBuf>,
    pub playback_command: Option<PathBuf>,
    pub details: Vec<String>,
}

/// Explicit paths and language choices for an offline voice request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeVoiceConfig {
    pub whisper_executable: PathBuf,
    pub whisper_model: PathBuf,
    pub piper_executable: PathBuf,
    pub piper_model: PathBuf,
    pub piper_config: Option<PathBuf>,
    pub language: String,
    pub voice: String,
    pub speech_rate_percent: u16,
    pub volume_percent: u16,
    pub working_directory: PathBuf,
}

/// Locate bundled/downloaded engines, honoring validated explicit overrides.
#[must_use]
pub fn discover_native_voice(
    voice_root: &Path,
    whisper_override: &str,
    whisper_model_override: &str,
    piper_override: &str,
    piper_model_override: &str,
) -> NativeVoiceStatus {
    let executable_name = |name: &str| {
        if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        }
    };
    let whisper_executable = first_file(&[
        path_if_set(whisper_override),
        Some(
            voice_root
                .join("whisper/bin")
                .join(executable_name("whisper-cli")),
        ),
        command_path("whisper-cli"),
        command_path("whisper-cpp"),
        command_path("main"),
    ]);
    let whisper_model = first_file(&[
        path_if_set(whisper_model_override),
        Some(voice_root.join("whisper/models/ggml-base.bin")),
        Some(voice_root.join("whisper/models/ggml-small.bin")),
        Some(voice_root.join("whisper/models/ggml-tiny.bin")),
    ]);
    let piper_executable = first_file(&[
        path_if_set(piper_override),
        Some(voice_root.join("piper/bin").join(executable_name("piper"))),
        Some(voice_root.join("piper").join(executable_name("piper"))),
        command_path("piper"),
    ]);
    let piper_model = first_file(&[
        path_if_set(piper_model_override),
        Some(voice_root.join("piper/voices/en_US-lessac-medium.onnx")),
        Some(voice_root.join("piper/voices/en_US-hfc_male-medium.onnx")),
    ]);
    let playback_command = ["pw-play", "paplay", "aplay", "ffplay"]
        .iter()
        .find_map(|name| command_path(name));
    let mut details = Vec::new();
    if whisper_executable.is_none() {
        details.push("Whisper executable is not installed; download the offline STT engine in Voice settings.".into());
    }
    if whisper_model.is_none() {
        details.push(
            "Whisper model is not installed; download a local model in Voice settings.".into(),
        );
    }
    if piper_executable.is_none() {
        details.push(
            "Piper executable is not installed; download the offline TTS engine in Voice settings."
                .into(),
        );
    }
    if piper_model.is_none() {
        details.push(
            "Piper voice is not installed; download or select a local voice in Voice settings."
                .into(),
        );
    }
    if playback_command.is_none() {
        details.push("No supported native WAV playback command is available.".into());
    }
    NativeVoiceStatus {
        stt_ready: whisper_executable.is_some() && whisper_model.is_some(),
        tts_ready: piper_executable.is_some() && piper_model.is_some(),
        playback_ready: playback_command.is_some(),
        whisper_executable,
        whisper_model,
        piper_executable,
        piper_model,
        playback_command,
        details,
    }
}

fn path_if_set(value: &str) -> Option<PathBuf> {
    (!value.trim().is_empty()).then(|| PathBuf::from(value))
}

fn first_file(candidates: &[Option<PathBuf>]) -> Option<PathBuf> {
    candidates
        .iter()
        .flatten()
        .find(|path| path.is_file())
        .cloned()
}

fn command_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn request_stem(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{prefix}-{}-{nanos}", std::process::id())
}

#[allow(clippy::cast_possible_truncation)] // Samples are explicitly clamped to the i16 range.
fn write_pcm16_wav(path: &Path, samples: &[f32], sample_rate_hz: u32) -> Result<(), AudioError> {
    if samples.is_empty() || !(8_000..=192_000).contains(&sample_rate_hz) {
        return Err(AudioError::Processing(
            "voice capture has no usable PCM samples".into(),
        ));
    }
    let data_bytes = u32::try_from(samples.len().saturating_mul(2))
        .map_err(|_| AudioError::Processing("voice capture is too large".into()))?;
    let file = File::create(path).map_err(|error| AudioError::Processing(error.to_string()))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(b"RIFF")
        .and_then(|()| writer.write_all(&(36_u32 + data_bytes).to_le_bytes()))
        .and_then(|()| writer.write_all(b"WAVEfmt "))
        .and_then(|()| writer.write_all(&16_u32.to_le_bytes()))
        .and_then(|()| writer.write_all(&1_u16.to_le_bytes()))
        .and_then(|()| writer.write_all(&1_u16.to_le_bytes()))
        .and_then(|()| writer.write_all(&sample_rate_hz.to_le_bytes()))
        .and_then(|()| writer.write_all(&(sample_rate_hz * 2).to_le_bytes()))
        .and_then(|()| writer.write_all(&2_u16.to_le_bytes()))
        .and_then(|()| writer.write_all(&16_u16.to_le_bytes()))
        .and_then(|()| writer.write_all(b"data"))
        .and_then(|()| writer.write_all(&data_bytes.to_le_bytes()))
        .map_err(|error| AudioError::Processing(error.to_string()))?;
    for sample in samples {
        let pcm = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        writer
            .write_all(&pcm.to_le_bytes())
            .map_err(|error| AudioError::Processing(error.to_string()))?;
    }
    writer
        .flush()
        .map_err(|error| AudioError::Processing(error.to_string()))
}

/// Transcribe mono PCM using a private, local `whisper.cpp` process.
///
/// # Errors
///
/// Returns an error when inputs are invalid, the local engine cannot run, or
/// the engine reports no usable speech.
pub async fn transcribe_pcm(
    executable: &Path,
    model: &Path,
    working_directory: &Path,
    samples: &[f32],
    sample_rate_hz: u32,
    language: &str,
) -> Result<Transcript, AudioError> {
    if !executable.is_file() || !model.is_file() {
        return Err(AudioError::Unavailable(
            "offline Whisper executable or model is missing".into(),
        ));
    }
    fs::create_dir_all(working_directory)
        .map_err(|error| AudioError::Processing(error.to_string()))?;
    let stem = request_stem("stt");
    let wav = working_directory.join(format!("{stem}.wav"));
    let output = working_directory.join(&stem);
    write_pcm16_wav(&wav, samples, sample_rate_hz)?;
    let result = Command::new(executable)
        .args(["-m"])
        .arg(model)
        .args(["-f"])
        .arg(&wav)
        .args(["-l", language, "-otxt", "-of"])
        .arg(&output)
        .args(["-nt", "-np"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| AudioError::Processing(error.to_string()))?;
    let text_path = output.with_extension("txt");
    let mut text = fs::read_to_string(&text_path).unwrap_or_default();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&result.stdout).into_owned();
    }
    let _ = fs::remove_file(&wav);
    let _ = fs::remove_file(&text_path);
    if !result.status.success() {
        let detail = String::from_utf8_lossy(&result.stderr)
            .chars()
            .take(1_000)
            .collect::<String>();
        return Err(AudioError::Processing(format!(
            "Whisper transcription failed: {detail}"
        )));
    }
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Err(AudioError::NoSpeech);
    }
    Ok(Transcript {
        text,
        final_result: true,
        confidence: None,
        language: Some(language.to_owned()),
    })
}

/// Render speech to a private WAV file with Piper. Playback is a separate,
/// cancellable child process owned by the desktop host.
///
/// # Errors
///
/// Returns an error when the model is unavailable, synthesis fails, or the
/// private output file cannot be created.
pub async fn synthesize_piper(
    executable: &Path,
    model: &Path,
    config: Option<&Path>,
    working_directory: &Path,
    text: &str,
    speech_rate_percent: u16,
) -> Result<PathBuf, AudioError> {
    if text.trim().is_empty() {
        return Err(AudioError::Processing("speech text cannot be blank".into()));
    }
    if !executable.is_file() || !model.is_file() {
        return Err(AudioError::Unavailable(
            "offline Piper executable or voice model is missing".into(),
        ));
    }
    fs::create_dir_all(working_directory)
        .map_err(|error| AudioError::Processing(error.to_string()))?;
    let wav = working_directory.join(format!("{}.wav", request_stem("tts")));
    let mut command = Command::new(executable);
    command
        .arg("--model")
        .arg(model)
        .arg("--length_scale")
        .arg(format!(
            "{:.3}",
            100.0 / f64::from(speech_rate_percent.clamp(50, 200))
        ))
        .arg("--output_file")
        .arg(&wav);
    if let Some(config) = config.filter(|path| path.is_file()) {
        command.arg("--config").arg(config);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AudioError::Processing(error.to_string()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AudioError::Processing("Piper stdin is unavailable".into()))?;
    stdin
        .write_all(text.as_bytes())
        .await
        .map_err(|error| AudioError::Processing(error.to_string()))?;
    stdin
        .shutdown()
        .await
        .map_err(|error| AudioError::Processing(error.to_string()))?;
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| AudioError::Processing(error.to_string()))?;
    if !output.status.success() || !wav.is_file() {
        let detail = String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(1_000)
            .collect::<String>();
        return Err(AudioError::Processing(format!(
            "Piper synthesis failed: {detail}"
        )));
    }
    Ok(wav)
}

/// Start platform-local WAV playback and return the cancellable process.
///
/// # Errors
///
/// Returns an error when the configured native playback process cannot start.
pub fn play_wav(
    command: &Path,
    wav: &Path,
    output_device: &str,
    volume_percent: u16,
) -> Result<Child, AudioError> {
    let name = command
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let mut process = Command::new(command);
    match name {
        "pw-play" => {
            if !output_device.trim().is_empty() {
                process.args(["--target", output_device]);
            }
            process.args([
                "--volume",
                &format!("{:.2}", f64::from(volume_percent.min(150)) / 100.0),
            ]);
        }
        "paplay" => {
            if !output_device.trim().is_empty() {
                process.arg(format!("--device={output_device}"));
            }
            process.arg(format!(
                "--volume={}",
                u32::from(volume_percent.min(150)) * 65_536 / 100
            ));
        }
        "aplay" => {
            if !output_device.trim().is_empty() {
                process.args(["-D", output_device]);
            }
        }
        "ffplay" | "ffplay.exe" => {
            process.args([
                "-nodisp",
                "-autoexit",
                "-loglevel",
                "error",
                "-volume",
                &volume_percent.min(100).to_string(),
            ]);
        }
        _ => {}
    }
    process
        .arg(wav)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| AudioError::Processing(error.to_string()))
}
