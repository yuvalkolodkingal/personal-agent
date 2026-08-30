//! Native offline STT/TTS process adapters used by the desktop host.

use crate::{AudioError, Transcript, native_output_device_name};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

/// Resolved local voice engines and models. Missing components remain explicit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // Readiness probes are independent hardware/runtime facts.
pub struct NativeVoiceStatus {
    pub stt_ready: bool,
    pub tts_ready: bool,
    pub playback_ready: bool,
    pub configured_stt_backend: String,
    pub configured_tts_backend: String,
    pub active_stt_backend: String,
    pub active_tts_backend: String,
    pub degraded: bool,
    pub neural_runtime_ready: bool,
    pub moonshine_ready: bool,
    pub smart_turn_ready: bool,
    pub qwen_ready: bool,
    pub kokoro_ready: bool,
    pub moonshine_model: Option<PathBuf>,
    pub qwen_model: Option<PathBuf>,
    pub kokoro_model: Option<PathBuf>,
    pub neural_python: Option<PathBuf>,
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
#[allow(clippy::too_many_arguments)] // Discovery mirrors independently configured engine/model/device fields.
#[allow(clippy::too_many_lines)] // A single probe preserves the explicit fallback order.
pub fn discover_native_voice(
    voice_root: &Path,
    stt_backend: &str,
    tts_backend: &str,
    whisper_override: &str,
    whisper_model_override: &str,
    piper_override: &str,
    piper_model_override: &str,
    output_device: &str,
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
    let native_output_device = native_output_device_name(output_device);
    let neural_python = first_file(&[Some(voice_root.join(if cfg!(windows) {
        "neural/venv/Scripts/python.exe"
    } else {
        "neural/venv/bin/python"
    }))]);
    let moonshine_marker = voice_root.join("neural/moonshine.json");
    let moonshine_model = fs::read_to_string(&moonshine_marker)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| {
            value
                .get("model_path")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
        })
        .filter(|path| path.is_dir());
    let qwen_custom = voice_root.join("neural/models/qwen3-tts-0.6b-customvoice");
    let qwen_base = voice_root.join("neural/models/qwen3-tts-0.6b-base");
    let qwen_model = [qwen_custom, qwen_base].into_iter().find(|path| {
        path.join("config.json").is_file()
            && path.join("model.safetensors").is_file()
            && path.join("speech_tokenizer/model.safetensors").is_file()
    });
    let kokoro_root = voice_root.join("neural/models/kokoro-v1.0-int8");
    let kokoro_model = (kokoro_root.join("kokoro-v1.0.int8.onnx").is_file()
        && kokoro_root.join("voices-v1.0.bin").is_file()
        && voice_root.join("neural/kokoro.json").is_file())
    .then_some(kokoro_root);
    let neural_runtime_ready = neural_python.is_some();
    let smart_turn_ready = neural_runtime_ready
        && voice_root
            .join("neural/models/smart-turn-v3.2-cpu.onnx")
            .is_file();
    let moonshine_ready = neural_runtime_ready && moonshine_model.is_some();
    let qwen_ready = neural_runtime_ready && qwen_model.is_some();
    let kokoro_ready = neural_runtime_ready && kokoro_model.is_some();
    let whisper_ready = whisper_executable.is_some() && whisper_model.is_some();
    let piper_ready = piper_executable.is_some() && piper_model.is_some();
    let wants_moonshine = stt_backend == "moonshine";
    let wants_qwen = tts_backend == "qwen3-tts";
    let wants_kokoro = tts_backend == "kokoro";
    let stt_ready = if wants_moonshine {
        moonshine_ready || whisper_ready
    } else {
        whisper_ready
    };
    let tts_ready = if wants_qwen {
        qwen_ready || kokoro_ready || piper_ready
    } else if wants_kokoro {
        kokoro_ready || piper_ready
    } else {
        piper_ready
    };
    let active_stt_backend = if wants_moonshine && moonshine_ready {
        "moonshine".to_owned()
    } else if whisper_ready {
        "whisper.cpp".to_owned()
    } else {
        stt_backend.to_owned()
    };
    let active_tts_backend = if wants_qwen && qwen_ready {
        "qwen3-tts".to_owned()
    } else if (wants_qwen || wants_kokoro) && kokoro_ready {
        "kokoro".to_owned()
    } else if piper_ready {
        "piper".to_owned()
    } else {
        tts_backend.to_owned()
    };
    let degraded = active_stt_backend != stt_backend
        || active_tts_backend != tts_backend
        || (wants_moonshine && !smart_turn_ready);
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
    match (&native_output_device, &playback_command) {
        (Ok(device), Some(command)) => {
            details.push(format!(
                "Native rodio playback is available through cpal device `{device}`; `{}` remains subprocess fallback only.",
                command
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("pw-play")
            ));
        }
        (Ok(device), None) => details.push(format!(
            "Native rodio playback is available through cpal device `{device}`."
        )),
        (Err(error), Some(command)) => details.push(format!(
            "Native cpal playback is unavailable ({error}); `{}` is available as the subprocess fallback.",
            command
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("pw-play")
        )),
        (Err(error), None) => details.push(format!(
            "No native audio output is available: {error}. Configure a cpal output device or install pw-play."
        )),
    }
    if wants_moonshine && !moonshine_ready {
        details
            .push("Moonshine Medium Streaming is unavailable; Whisper is the STT fallback.".into());
    }
    if wants_qwen && !qwen_ready {
        details.push("Qwen3-TTS 0.6B is unavailable; Kokoro CPU is the next TTS tier.".into());
    }
    if (wants_qwen || wants_kokoro) && !kokoro_ready {
        details.push("Kokoro CPU is unavailable; Piper is the final TTS fallback.".into());
    }
    if wants_moonshine && !smart_turn_ready {
        details.push(
            "Smart Turn v3.2 is unavailable; adaptive silence endpointing remains active.".into(),
        );
    }
    if moonshine_ready {
        details.push("Moonshine Medium Streaming is ready for English speech.".into());
    }
    if qwen_ready {
        details.push("Qwen3-TTS 0.6B is ready on the local GPU.".into());
    }
    if kokoro_ready {
        details.push("Kokoro int8 is ready on the local CPU.".into());
    }
    if smart_turn_ready {
        details.push("Smart Turn v3.2 semantic endpointing is ready on the local CPU.".into());
    }
    NativeVoiceStatus {
        stt_ready,
        tts_ready,
        playback_ready: native_output_device.is_ok() || playback_command.is_some(),
        configured_stt_backend: stt_backend.to_owned(),
        configured_tts_backend: tts_backend.to_owned(),
        active_stt_backend,
        active_tts_backend,
        degraded,
        neural_runtime_ready,
        moonshine_ready,
        smart_turn_ready,
        qwen_ready,
        kokoro_ready,
        moonshine_model,
        qwen_model,
        kokoro_model,
        neural_python,
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
/// Write normalized mono samples as a PCM WAV file for local inference engines.
///
/// # Errors
///
/// Returns an [`AudioError`] for invalid samples or a file-system write failure.
pub fn write_pcm_wav(path: &Path, samples: &[f32], sample_rate_hz: u32) -> Result<(), AudioError> {
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
    write_pcm_wav(&wav, samples, sample_rate_hz)?;
    let result = transcribe_wav(executable, model, working_directory, &wav, language).await;
    let _ = fs::remove_file(&wav);
    result
}

/// Transcribe an existing private WAV file using local `whisper.cpp`.
///
/// # Errors
///
/// Returns an error when the engine, model, or input is missing, the process
/// fails, or no usable speech is recognized.
pub async fn transcribe_wav(
    executable: &Path,
    model: &Path,
    working_directory: &Path,
    wav: &Path,
    language: &str,
) -> Result<Transcript, AudioError> {
    if !executable.is_file() || !model.is_file() || !wav.is_file() {
        return Err(AudioError::Unavailable(
            "offline Whisper executable, model, or WAV input is missing".into(),
        ));
    }
    fs::create_dir_all(working_directory)
        .map_err(|error| AudioError::Processing(error.to_string()))?;
    let output = working_directory.join(request_stem("stt-output"));
    let result = Command::new(executable)
        .args(["-m"])
        .arg(model)
        .args(["-f"])
        .arg(wav)
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
    command.kill_on_drop(true);
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
        .write_all(format!("{}\n", text.trim()).as_bytes())
        .await
        .map_err(|error| AudioError::Processing(error.to_string()))?;
    stdin
        .shutdown()
        .await
        .map_err(|error| AudioError::Processing(error.to_string()))?;
    // Async pipe shutdown flushes bytes but does not necessarily close the
    // descriptor. Piper reads until EOF, so explicitly drop our writer before
    // waiting or synthesis remains stuck forever.
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .map_err(|_| AudioError::Processing("Piper synthesis timed out".into()))?
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn piper_input_reaches_eof_before_waiting_for_output() {
        let directory = std::env::temp_dir().join(request_stem("piper-eof-test"));
        fs::create_dir_all(&directory).expect("fixture directory");
        let executable = directory.join("piper-fixture");
        let model = directory.join("voice.onnx");
        fs::write(&model, b"fixture").expect("fixture model");
        fs::write(
            &executable,
            br#"#!/bin/sh
output=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--output_file' ]; then output="$2"; shift 2; else shift; fi
done
payload=$(dd bs=1 2>/dev/null)
[ "$payload" = 'Hello from test' ] || exit 9
printf RIFF > "$output"
"#,
        )
        .expect("fixture executable");
        let mut permissions = fs::metadata(&executable)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).expect("fixture permissions");

        let wav = tokio::time::timeout(
            Duration::from_secs(5),
            synthesize_piper(
                &executable,
                &model,
                None,
                &directory,
                "Hello from test",
                100,
            ),
        )
        .await
        .expect("Piper fixture must receive EOF")
        .expect("Piper fixture synthesis");
        assert_eq!(fs::read(wav).expect("fixture WAV"), b"RIFF");
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[test]
    fn neural_voice_assets_are_selected_without_hiding_compatibility_fallbacks() {
        let root = std::env::temp_dir().join(request_stem("neural-status-test"));
        let python = root.join("neural/venv/bin/python");
        let moonshine = root.join("neural/models/moonshine/medium-streaming-en");
        let qwen = root.join("neural/models/qwen3-tts-0.6b-customvoice");
        let smart_turn = root.join("neural/models/smart-turn-v3.2-cpu.onnx");
        fs::create_dir_all(python.parent().expect("python parent")).expect("python directory");
        fs::create_dir_all(&moonshine).expect("Moonshine directory");
        fs::create_dir_all(&qwen).expect("Qwen directory");
        fs::write(&python, b"fixture").expect("python fixture");
        fs::write(qwen.join("config.json"), b"{}").expect("Qwen config");
        fs::write(qwen.join("model.safetensors"), b"fixture").expect("Qwen model");
        fs::create_dir_all(qwen.join("speech_tokenizer")).expect("speech tokenizer directory");
        fs::write(qwen.join("speech_tokenizer/model.safetensors"), b"fixture")
            .expect("speech tokenizer model");
        fs::write(smart_turn, b"fixture").expect("Smart Turn model");
        fs::write(
            root.join("neural/moonshine.json"),
            serde_json::to_vec(&serde_json::json!({"model_path": moonshine, "model_arch": 5}))
                .expect("Moonshine marker"),
        )
        .expect("write Moonshine marker");

        let status = discover_native_voice(&root, "moonshine", "qwen3-tts", "", "", "", "", "");
        assert!(status.moonshine_ready);
        assert!(status.smart_turn_ready);
        assert!(status.qwen_ready);
        assert_eq!(status.active_stt_backend, "moonshine");
        assert_eq!(status.active_tts_backend, "qwen3-tts");
        assert!(!status.degraded);
        fs::remove_dir_all(root).expect("remove neural fixture");
    }

    #[test]
    fn configured_output_status_uses_cpal_first_and_command_only_as_fallback() {
        let root = std::env::temp_dir().join(request_stem("output-status-test"));
        fs::create_dir_all(&root).expect("status fixture directory");
        let missing_device = "personal-agent-test-device-that-does-not-exist";
        let status = discover_native_voice(
            &root,
            "whisper.cpp",
            "piper",
            "",
            "",
            "",
            "",
            missing_device,
        );
        assert_eq!(status.playback_ready, status.playback_command.is_some());
        assert!(status.details.iter().any(|detail| {
            detail.contains("Native cpal playback is unavailable")
                || detail.contains("No native audio output is available")
        }));
        assert!(!status.details.iter().any(|detail| {
            detail.contains("Native rodio playback is available") && detail.contains(missing_device)
        }));
        fs::remove_dir_all(root).expect("remove status fixture");
    }

    #[tokio::test]
    #[ignore = "requires installed Whisper and Piper assets"]
    async fn installed_voice_assets_round_trip_speech() {
        let root = PathBuf::from(
            std::env::var("PERSONAL_AGENT_VOICE_SMOKE_ROOT")
                .expect("set PERSONAL_AGENT_VOICE_SMOKE_ROOT"),
        );
        let status = discover_native_voice(&root, "whisper.cpp", "piper", "", "", "", "", "");
        let working = root.join("runtime-smoke");
        let wav = synthesize_piper(
            status
                .piper_executable
                .as_deref()
                .expect("Piper executable"),
            status.piper_model.as_deref().expect("Piper model"),
            status
                .piper_model
                .as_ref()
                .map(|model| model.with_extension("onnx.json"))
                .as_deref(),
            &working,
            "Personal Agent voice test",
            100,
        )
        .await
        .expect("Piper synthesis");
        let transcript = transcribe_wav(
            status
                .whisper_executable
                .as_deref()
                .expect("Whisper executable"),
            status.whisper_model.as_deref().expect("Whisper model"),
            &working,
            &wav,
            "en",
        )
        .await
        .expect("Whisper transcription");
        assert!(
            transcript
                .text
                .to_ascii_lowercase()
                .contains("personal agent"),
            "unexpected transcript: {}",
            transcript.text
        );
        fs::remove_dir_all(working).expect("remove smoke output");
    }
}
