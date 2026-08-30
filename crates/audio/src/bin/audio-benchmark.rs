//! Deterministic audio orchestration benchmark; physical-device runs use the same schema.

use async_trait::async_trait;
use personal_agent_audio::{
    AudioError, AudioFrame, EnrolledWakeDetector, MicrophoneState, NetworkPolicy,
    NeuralVoiceRuntime, PhraseCache, PhraseKey, SpeechRecognizer, SpeechSynthesizer, Transcript,
    VoicePipeline, WakeTemplate, summarize_latencies,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const STT_CORPUS_CASES: usize = 10;
const STT_CORPUS_MANIFEST_SHA256: &str =
    "19beb8c0f60c68df6661cd5d5f6975687d825ee9f419419751bee935442b3152";
const STT_REPLAY_CHUNK_SAMPLES: usize = 320;
const STT_SAMPLE_RATE_HZ: u32 = 16_000;
const ACK_PHRASE: &str = "On it.";
const ACK_PHRASE_ENGINE: &str = "qwen3-tts";
const ACK_PHRASE_VOICE: &str = "Ryan";
const ACK_PHRASE_SAMPLE_RATE_HZ: u32 = 24_000;
const ACK_PHRASE_FRAME_SAMPLES: usize = 480;

fn audio_duration(samples: u64) -> Duration {
    let sample_rate_hz = u64::from(STT_SAMPLE_RATE_HZ);
    Duration::from_secs(samples / sample_rate_hz)
        + Duration::from_nanos((samples % sample_rate_hz) * 1_000_000_000 / sample_rate_hz)
}

fn partial_emit_lag(
    emit_elapsed: Duration,
    partial_audio_samples: u64,
    samples_sent: usize,
) -> Result<Duration, String> {
    if partial_audio_samples == 0 {
        return Err("changed STT partial has no decoder audio boundary".to_owned());
    }
    let samples_sent = u64::try_from(samples_sent)
        .map_err(|_| "STT replay sample count does not fit in u64".to_owned())?;
    if partial_audio_samples > samples_sent {
        return Err(format!(
            "STT partial decoder boundary {partial_audio_samples} exceeds {samples_sent} sent samples"
        ));
    }
    let boundary_time = audio_duration(partial_audio_samples);
    emit_elapsed.checked_sub(boundary_time).ok_or_else(|| {
        format!(
            "STT partial was observed at {emit_elapsed:?} before its decoder audio boundary {boundary_time:?}"
        )
    })
}

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

/// Pre-synthesize the warmup acknowledgement into a private phrase cache, the
/// same way the desktop host warms "On it." and the persona lines before the
/// first spoken turn.
fn warm_ack_phrase_cache() -> Result<(PhraseCache, PhraseKey, PathBuf), Box<dyn std::error::Error>>
{
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!(
        "personal-agent-ack-replay-{}-{nanos}",
        std::process::id()
    ));
    let cache = PhraseCache::open(&root)?;
    let key = PhraseKey::new(
        ACK_PHRASE_ENGINE,
        ACK_PHRASE_VOICE,
        &format!("{ACK_PHRASE_SAMPLE_RATE_HZ}@100"),
        ACK_PHRASE,
    );
    let sample_count = usize::try_from(ACK_PHRASE_SAMPLE_RATE_HZ)?;
    let samples = (0..sample_count)
        .map(|index| i16::try_from(index % 4_096).unwrap_or(i16::MAX))
        .collect::<Vec<_>>();
    cache.put(&key, &samples, ACK_PHRASE_SAMPLE_RATE_HZ, 1)?;
    Ok((cache, key, root))
}

/// Time from requesting the acknowledgement to its first PCM frame reaching the
/// playback queue. A warmed phrase never touches the synthesis worker, so this
/// is a cache read plus one frame handoff; it excludes physical device startup.
fn replay_ack_first_audio(
    cache: &PhraseCache,
    key: &PhraseKey,
) -> Result<Duration, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut first_audio = Duration::ZERO;
    std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
        let (pcm_producer, pcm_sink) = mpsc::sync_channel::<Vec<i16>>(1);
        scope.spawn(move || {
            let Some(phrase) = cache.get(key) else {
                return;
            };
            for frame in phrase.samples.chunks(ACK_PHRASE_FRAME_SAMPLES) {
                if pcm_producer.send(frame.to_vec()).is_err() {
                    return;
                }
            }
        });
        let first_frame = pcm_sink.recv()?;
        first_audio = started.elapsed();
        black_box(first_frame.first().copied());
        for frame in pcm_sink {
            black_box(frame.first().copied());
        }
        Ok(())
    })?;
    if first_audio.is_zero() {
        return Err("the warmed acknowledgement produced no cached audio".into());
    }
    Ok(first_audio)
}

#[derive(Debug)]
struct SttCorpusCase {
    id: String,
    reference: String,
    samples: Vec<f32>,
}

#[derive(Debug)]
struct SttEngineReplay {
    wer: Value,
    partial_observations: Vec<SttPartialObservation>,
}

#[derive(Debug)]
struct SttPartialObservation {
    case_id: String,
    index: usize,
    text: String,
    decoder_audio_samples: u64,
    observed_audio_samples: u64,
    emit_elapsed: Duration,
    lag: Duration,
}

fn little_endian_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let value = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| "truncated WAV integer".to_owned())?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn little_endian_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let value = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| "truncated WAV integer".to_owned())?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn pcm16_mono_wav_samples(path: &Path) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err(format!("{} is not a RIFF/WAVE file", path.display()).into());
    }
    let mut cursor = 12_usize;
    let mut format = None;
    let mut data = None;
    while cursor.saturating_add(8) <= bytes.len() {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_size = usize::try_from(little_endian_u32(&bytes, cursor + 4)?)?;
        let payload_start = cursor + 8;
        let payload_end = payload_start
            .checked_add(chunk_size)
            .ok_or("WAV chunk length overflow")?;
        let payload = bytes
            .get(payload_start..payload_end)
            .ok_or("truncated WAV chunk")?;
        match chunk_id {
            b"fmt " => {
                if payload.len() < 16 {
                    return Err("truncated WAV format chunk".into());
                }
                format = Some((
                    little_endian_u16(payload, 0)?,
                    little_endian_u16(payload, 2)?,
                    little_endian_u32(payload, 4)?,
                    little_endian_u16(payload, 12)?,
                    little_endian_u16(payload, 14)?,
                ));
            }
            b"data" => data = Some(payload),
            _ => {}
        }
        cursor = payload_end
            .checked_add(chunk_size % 2)
            .ok_or("WAV padding overflow")?;
    }
    if format != Some((1, 1, STT_SAMPLE_RATE_HZ, 2, 16)) {
        return Err(format!(
            "{} must be mono signed PCM16 at {STT_SAMPLE_RATE_HZ} Hz",
            path.display()
        )
        .into());
    }
    let data = data.ok_or("WAV has no data chunk")?;
    if data.is_empty() || !data.len().is_multiple_of(2) {
        return Err("WAV PCM data must contain complete signed 16-bit samples".into());
    }
    Ok(data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|sample| f32::from(i16::from_le_bytes(*sample)) / 32_768.0)
        .collect())
}

fn stt_corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fixtures/stt-corpus")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn validate_sha256(
    bytes: &[u8],
    expected: &str,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let found = sha256_hex(bytes);
    if found != expected {
        return Err(format!("{label} drifted: expected {expected}, found {found}").into());
    }
    Ok(())
}

fn parse_stt_corpus_manifest(bytes: &[u8]) -> Result<Value, Box<dyn std::error::Error>> {
    validate_sha256(bytes, STT_CORPUS_MANIFEST_SHA256, "STT corpus manifest")?;
    let manifest: Value = serde_json::from_slice(bytes)?;
    for (field, expected) in [
        ("license", "CC0-1.0"),
        ("source_repository", "akahana/common-voice-11-eng-sample"),
        (
            "source_revision",
            "4354744379973dd44a1b2273d7beb893810912f5",
        ),
    ] {
        if manifest.get(field).and_then(Value::as_str) != Some(expected) {
            return Err(format!("STT corpus manifest has unexpected {field}").into());
        }
    }
    Ok(manifest)
}

fn load_stt_corpus() -> Result<Vec<SttCorpusCase>, Box<dyn std::error::Error>> {
    let root = stt_corpus_root();
    let paths = std::fs::read_dir(&root)?
        .map(|entry| entry.map(|value| value.path()))
        .collect::<Result<Vec<_>, _>>()?;
    let wav_count = paths
        .iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("wav"))
        .count();
    let transcript_count = paths
        .iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("txt"))
        .count();
    if wav_count != STT_CORPUS_CASES || transcript_count != STT_CORPUS_CASES {
        return Err(format!(
            "STT corpus must contain exactly {STT_CORPUS_CASES} WAV/TXT pairs; found {wav_count} WAVs and {transcript_count} transcripts"
        )
        .into());
    }

    let manifest_bytes = std::fs::read(root.join("manifest.json"))?;
    let manifest = parse_stt_corpus_manifest(&manifest_bytes)?;
    let entries = manifest
        .get("cases")
        .and_then(Value::as_array)
        .ok_or("STT corpus manifest has no cases")?;
    if entries.len() != STT_CORPUS_CASES {
        return Err(format!("STT corpus manifest must contain {STT_CORPUS_CASES} cases").into());
    }
    let mut ids = BTreeSet::new();
    entries
        .iter()
        .map(|entry| {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .ok_or("STT corpus case has no id")?;
            if !id.starts_with("common_voice_en_")
                || !id
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
                || !ids.insert(id.to_owned())
            {
                return Err(format!("invalid or duplicate STT corpus id: {id}").into());
            }
            let wav = root.join(id).with_extension("wav");
            let reference_path = root.join(id).with_extension("txt");
            for (path, hash_field) in [(&wav, "wav_sha256"), (&reference_path, "transcript_sha256")]
            {
                let expected = entry
                    .get(hash_field)
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{id} has no {hash_field}"))?;
                validate_sha256(&std::fs::read(path)?, expected, &path.display().to_string())?;
            }
            let reference = std::fs::read_to_string(&reference_path)?.trim().to_owned();
            if reference.is_empty() {
                return Err(format!("{} is empty", reference_path.display()).into());
            }
            Ok(SttCorpusCase {
                id: id.to_owned(),
                reference,
                samples: pcm16_mono_wav_samples(&wav)?,
            })
        })
        .collect()
}

fn normalized_words(text: &str) -> Vec<String> {
    let normalized = text
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '\'' | '\u{2019}') {
                if character == '\u{2019}' {
                    '\''
                } else {
                    character
                }
            } else {
                ' '
            }
        })
        .collect::<String>();
    normalized
        .split_whitespace()
        .map(|word| word.trim_matches('\'').to_owned())
        .filter(|word| !word.is_empty())
        .collect()
}

fn word_edit_distance(reference: &[String], hypothesis: &[String]) -> usize {
    let mut previous = (0..=hypothesis.len()).collect::<Vec<_>>();
    let mut current = vec![0; hypothesis.len() + 1];
    for (reference_index, reference_word) in reference.iter().enumerate() {
        current[0] = reference_index + 1;
        for (hypothesis_index, hypothesis_word) in hypothesis.iter().enumerate() {
            current[hypothesis_index + 1] = if reference_word == hypothesis_word {
                previous[hypothesis_index]
            } else {
                previous[hypothesis_index]
                    .min(previous[hypothesis_index + 1])
                    .min(current[hypothesis_index])
                    + 1
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[hypothesis.len()]
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

async fn replay_stt_corpus_case(
    worker: &mut NeuralVoiceRuntime,
    case: &SttCorpusCase,
    engine: &str,
    timeout: Duration,
) -> Result<(String, Vec<SttPartialObservation>), Box<dyn std::error::Error>> {
    let started = worker
        .request(
            "stt_start",
            json!({
                "stt_engine": engine,
                "language": "en",
                "vocabulary": [],
            }),
            timeout,
        )
        .await?;
    if started.get("engine").and_then(Value::as_str) != Some(engine) {
        return Err(format!(
            "{} started unexpected STT engine {:?}",
            case.id,
            started.get("engine")
        )
        .into());
    }

    let replay_started = Instant::now();
    let mut samples_sent = 0_usize;
    let mut last_partial = String::new();
    let mut partial_observations = Vec::new();
    for chunk in case.samples.chunks(STT_REPLAY_CHUNK_SAMPLES) {
        samples_sent = samples_sent.saturating_add(chunk.len());
        let samples_sent_seconds =
            samples_sent.to_string().parse::<f64>()? / f64::from(STT_SAMPLE_RATE_HZ);
        let audio_time = Duration::from_secs_f64(samples_sent_seconds);
        tokio::time::sleep_until(tokio::time::Instant::from_std(replay_started + audio_time)).await;
        let result = worker
            .request(
                "stt_chunk",
                json!({"samples": chunk, "sample_rate_hz": STT_SAMPLE_RATE_HZ}),
                timeout,
            )
            .await?;
        if result.get("engine").and_then(Value::as_str) != Some(engine)
            || result.get("final_result").and_then(Value::as_bool) != Some(false)
        {
            return Err(format!("{} returned invalid {engine} partial metadata", case.id).into());
        }
        let partial = result
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if !partial.is_empty() && partial != last_partial {
            let partial_audio_samples = result
                .get("partial_audio_samples")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    format!(
                        "{} returned a changed {engine} partial without an integer decoder audio boundary",
                        case.id
                    )
                })?;
            let emit_elapsed = replay_started.elapsed();
            let lag = partial_emit_lag(emit_elapsed, partial_audio_samples, samples_sent)?;
            let observed_audio_samples = u64::try_from(samples_sent)?;
            partial_observations.push(SttPartialObservation {
                case_id: case.id.clone(),
                index: partial_observations.len(),
                text: partial.to_owned(),
                decoder_audio_samples: partial_audio_samples,
                observed_audio_samples,
                emit_elapsed,
                lag,
            });
            last_partial.clear();
            last_partial.push_str(partial);
        }
    }
    let final_result = match worker.request("stt_stop", json!({}), timeout).await {
        Ok(result) => result,
        Err(AudioError::Processing(message)) if message == "no speech was detected" => {
            // An empty recognition result is valid WER evidence: every
            // reference word is a deletion. Other worker errors stay fatal.
            return Ok((String::new(), partial_observations));
        }
        Err(error) => return Err(Box::new(error)),
    };
    if final_result.get("engine").and_then(Value::as_str) != Some(engine)
        || final_result.get("final_result").and_then(Value::as_bool) != Some(true)
    {
        return Err(format!("{} returned invalid {engine} final metadata", case.id).into());
    }
    let transcript = final_result
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{} returned no {engine} transcript", case.id))?
        .trim()
        .to_owned();
    Ok((transcript, partial_observations))
}

async fn measure_stt_engine_replay(
    worker: &mut NeuralVoiceRuntime,
    corpus: &[SttCorpusCase],
    engine: &str,
    timeout: Duration,
) -> Result<SttEngineReplay, Box<dyn std::error::Error>> {
    let mut total_errors = 0_usize;
    let mut total_reference_words = 0_usize;
    let mut utterances = Vec::with_capacity(corpus.len());
    let mut partial_observations = Vec::new();
    for case in corpus {
        let replay = replay_stt_corpus_case(worker, case, engine, timeout).await;
        let (hypothesis, case_partial_observations) = match replay {
            Ok(replay) => replay,
            Err(error) => {
                let _ = worker.request("stt_cancel", json!({}), timeout).await;
                return Err(format!("{} {engine} replay failed: {error}", case.id).into());
            }
        };
        partial_observations.extend(case_partial_observations);
        let reference_words = normalized_words(&case.reference);
        let hypothesis_words = normalized_words(&hypothesis);
        let errors = word_edit_distance(&reference_words, &hypothesis_words);
        total_errors = total_errors.saturating_add(errors);
        total_reference_words = total_reference_words.saturating_add(reference_words.len());
        utterances.push(json!({
            "id": case.id,
            "reference": case.reference,
            "hypothesis": hypothesis,
            "word_errors": errors,
            "reference_words": reference_words.len(),
        }));
    }
    if total_reference_words == 0 {
        return Err("STT corpus contained no reference words".into());
    }
    let wer = total_errors.to_string().parse::<f64>()?
        / total_reference_words.to_string().parse::<f64>()?;
    Ok(SttEngineReplay {
        wer: json!({
            "status": "measured",
            "engine": engine,
            "corpus": "mozilla-common-voice-11-cc0",
            "sample_count": corpus.len(),
            "word_errors": total_errors,
            "reference_words": total_reference_words,
            "wer": wer,
            "utterances": utterances,
        }),
        partial_observations,
    })
}

fn partial_observation_json(
    observation: &SttPartialObservation,
) -> Result<Value, Box<dyn std::error::Error>> {
    let decoder_backlog_samples = observation
        .observed_audio_samples
        .checked_sub(observation.decoder_audio_samples)
        .ok_or("STT partial decoder boundary exceeds observed audio")?;
    let post_latest_ingress = observation
        .emit_elapsed
        .checked_sub(audio_duration(observation.observed_audio_samples))
        .ok_or("STT partial emit precedes latest observed audio")?;
    Ok(json!({
        "case": observation.case_id,
        "partial_index": observation.index,
        "text": observation.text,
        "decoder_audio_samples": observation.decoder_audio_samples,
        "observed_audio_samples": observation.observed_audio_samples,
        "decoder_backlog_samples": decoder_backlog_samples,
        "decoder_backlog_microseconds": u64::try_from(audio_duration(decoder_backlog_samples).as_micros())?,
        "post_latest_ingress_microseconds": u64::try_from(post_latest_ingress.as_micros())?,
        "emit_elapsed_microseconds": u64::try_from(observation.emit_elapsed.as_micros())?,
        "lag_microseconds": u64::try_from(observation.lag.as_micros())?,
    }))
}

async fn measure_stt_corpus_replay(
    worker: &mut NeuralVoiceRuntime,
    timeout: Duration,
) -> Result<(Value, Value, Value), Box<dyn std::error::Error>> {
    let corpus = load_stt_corpus()?;
    let moonshine = measure_stt_engine_replay(worker, &corpus, "moonshine", timeout).await?;
    let accurate = measure_stt_engine_replay(worker, &corpus, "faster-whisper", timeout).await?;
    if moonshine.partial_observations.is_empty() || accurate.partial_observations.is_empty() {
        return Err("each STT engine must emit at least one changed streaming partial".into());
    }
    let moonshine_latencies = moonshine
        .partial_observations
        .iter()
        .map(|observation| observation.lag)
        .collect::<Vec<_>>();
    let accurate_latencies = accurate
        .partial_observations
        .iter()
        .map(|observation| observation.lag)
        .collect::<Vec<_>>();
    let moonshine_partial = summarize_latencies(&moonshine_latencies)?;
    let accurate_partial = summarize_latencies(&accurate_latencies)?;
    let moonshine_observations = moonshine
        .partial_observations
        .iter()
        .map(partial_observation_json)
        .collect::<Result<Vec<_>, _>>()?;
    let accurate_observations = accurate
        .partial_observations
        .iter()
        .map(partial_observation_json)
        .collect::<Result<Vec<_>, _>>()?;
    let mut partial_latencies = moonshine_latencies;
    partial_latencies.extend(accurate_latencies);
    if partial_latencies.len() < STT_CORPUS_CASES {
        return Err(format!(
            "streaming STT replay emitted only {} changed partials across two engines",
            partial_latencies.len()
        )
        .into());
    }
    let partial_summary = summarize_latencies(&partial_latencies)?;
    Ok((
        moonshine.wer,
        accurate.wer,
        json!({
            "status": "measured",
            "measurement": "paced 20 ms corpus frames; observed emit wall-time minus the decoder-associated cumulative audio boundary for each changed partial",
            "engines": ["moonshine", "faster-whisper"],
            "by_engine": {
                "moonshine": moonshine_partial,
                "faster-whisper": accurate_partial,
            },
            "observations": {
                "moonshine": moonshine_observations,
                "faster-whisper": accurate_observations,
            },
            "p50_microseconds": partial_summary.p50_microseconds,
            "p95_microseconds": partial_summary.p95_microseconds,
            "maximum_microseconds": partial_summary.maximum_microseconds,
            "sample_count": partial_summary.sample_count,
        }),
    ))
}

async fn actual_worker_voice_replays()
-> Result<(Value, Value, Value, Value, Value), Box<dyn std::error::Error>> {
    let Some(root) = std::env::var_os("PERSONAL_AGENT_VOICE_REPLAY_ROOT").map(PathBuf::from) else {
        let external = json!({
            "status": "external-model-assets-required",
            "command": "PERSONAL_AGENT_VOICE_REPLAY_ROOT=<neural-root> PERSONAL_AGENT_VOICE_REPLAY_PYTHON=<venv-python> PERSONAL_AGENT_VOICE_REPLAY_PCM=<signed-pcm16le> PERSONAL_AGENT_VOICE_REPLAY_WAV=<pcm16-wav> cargo run -p personal-agent-audio --bin audio-benchmark --quiet"
        });
        return Ok((
            external.clone(),
            external.clone(),
            external.clone(),
            external.clone(),
            external,
        ));
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
    let (stt_wer_moonshine, stt_wer_accurate, stt_partial_lag_ms) =
        measure_stt_corpus_replay(&mut worker, timeout).await?;
    worker.terminate();
    Ok((
        ambient_armed_cpu_replay,
        stt_endpoint_replay,
        stt_wer_moonshine,
        stt_wer_accurate,
        stt_partial_lag_ms,
    ))
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
    let mut tts_ack_first_audio = Vec::with_capacity(SAMPLES);
    let (ack_cache, ack_key, ack_root) = warm_ack_phrase_cache()?;
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
        tts_ack_first_audio.push(replay_ack_first_audio(&ack_cache, &ack_key)?);
    }
    drop(ack_cache);
    std::fs::remove_dir_all(&ack_root)?;
    let (
        ambient_armed_cpu_replay,
        stt_endpoint_replay,
        stt_wer_moonshine,
        stt_wer_accurate,
        stt_partial_lag_ms,
    ) = actual_worker_voice_replays().await?;
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
        "tts_ack_first_audio_ms": summarize_latencies(&tts_ack_first_audio)?,
        "ambient_armed_cpu_replay": ambient_armed_cpu_replay,
        "stt_endpoint_replay": stt_endpoint_replay,
        "stt_wer_moonshine": stt_wer_moonshine,
        "stt_wer_accurate": stt_wer_accurate,
        "stt_partial_lag_ms": stt_partial_lag_ms,
        "replay_scope": {
            "startup_native_setup": "serialized native-state replay; excludes window-system and physical device probes",
            "bootstrap_ipc": "JSON encode/decode replay; excludes WebView transport and paint",
            "desktop_snapshot_warm": "serialized accessibility-tree replay; excludes physical screen capture and input",
            "tts_first_audio_ms": "three-clause fake-engine turn through a one-clause prebuffer and bounded in-memory PCM sink; excludes physical device startup",
            "tts_ack_first_audio_ms": "warmup-cached acknowledgement phrase read from the 64 MiB LRU disk cache and handed to a bounded in-memory PCM sink; excludes synthesis and physical device startup",
            "ambient_armed_cpu": "real pinned worker/model paths when replay env vars are set; otherwise reported as external-model-assets-required",
            "stt_endpoint": "real pinned Silero v5 recurrent-state inference plus one Smart Turn v3.2 consultation when replay env vars are set",
            "stt_accuracy": "ten CC0 Common Voice 11 utterances transcribed by each real configured engine when replay env vars are set; no hard WER threshold",
            "stt_partial_lag_ms": "paced 20 ms CC0 corpus replay comparing observed partial emit wall-time with the worker-reported decoder audio boundary when replay env vars are set"
        },
        "replay_disclaimer": "Replay numbers are not physical microphone, speaker, network, screen-capture, or UI-startup measurements.",
        "external_hardware_required": ["end_to_end_barge_in", "cloud_first_audio", "idle_cpu", "idle_resident_memory", "warm_ui_startup", "physical_desktop_snapshot"]
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    black_box(Duration::ZERO);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_lag_uses_decoder_boundary_without_saturating_invalid_timelines() {
        assert_eq!(
            partial_emit_lag(Duration::from_millis(850), 12_800, 13_120)
                .expect("valid decoder boundary"),
            Duration::from_millis(50)
        );
        assert!(partial_emit_lag(Duration::from_millis(850), 0, 13_120).is_err());
        assert!(partial_emit_lag(Duration::from_millis(850), 13_440, 13_120).is_err());
        assert!(partial_emit_lag(Duration::from_millis(750), 12_800, 13_120).is_err());
    }

    #[test]
    fn word_error_rate_normalizes_case_and_punctuation() {
        assert_eq!(
            normalized_words("She\u{2019}ll be ALL right!"),
            ["she'll", "be", "all", "right"]
        );
        let reference = normalized_words("the quick brown fox");
        assert_eq!(word_edit_distance(&reference, &reference), 0);
        assert_eq!(
            word_edit_distance(&reference, &normalized_words("the slow brown fox")),
            1
        );
        assert_eq!(
            word_edit_distance(&reference, &normalized_words("the brown fox")),
            1
        );
        assert_eq!(
            word_edit_distance(&reference, &normalized_words("the very quick brown fox")),
            1
        );
        assert_eq!(word_edit_distance(&reference, &[]), reference.len());
    }

    #[test]
    fn bundled_stt_corpus_has_ten_pcm16_reference_pairs() {
        let corpus = load_stt_corpus().expect("load CC0 STT corpus");
        assert_eq!(corpus.len(), STT_CORPUS_CASES);
        assert!(corpus.iter().all(|case| !case.reference.is_empty()));
        assert!(corpus.iter().all(|case| !case.samples.is_empty()));
        assert!(
            corpus
                .iter()
                .flat_map(|case| &case.samples)
                .all(|sample| sample.is_finite() && (-1.0..=1.0).contains(sample))
        );
    }

    #[test]
    fn bundled_stt_corpus_rejects_manifest_audio_or_transcript_drift() {
        let root = stt_corpus_root();
        let manifest_bytes = std::fs::read(root.join("manifest.json")).expect("read manifest");
        let manifest = parse_stt_corpus_manifest(&manifest_bytes).expect("verified manifest");
        let mut changed_manifest = manifest_bytes;
        changed_manifest.push(b'\n');
        assert!(parse_stt_corpus_manifest(&changed_manifest).is_err());

        let first = manifest["cases"][0].as_object().expect("first case");
        let id = first["id"].as_str().expect("case id");
        for (extension, field) in [("wav", "wav_sha256"), ("txt", "transcript_sha256")] {
            let mut bytes =
                std::fs::read(root.join(id).with_extension(extension)).expect("fixture bytes");
            let expected = first[field].as_str().expect("fixture digest");
            validate_sha256(&bytes, expected, id).expect("fixture digest matches");
            bytes[0] ^= 1;
            assert!(validate_sha256(&bytes, expected, id).is_err());
        }
    }
}
