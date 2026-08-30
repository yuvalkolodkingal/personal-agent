#!/usr/bin/env python3
"""Persistent, local neural speech worker for Personal Agent.

The desktop host owns this process and communicates with one JSON object per
line.  Protocol output is kept on the original stdout; noisy model-library
output is redirected to stderr so it can never corrupt an IPC response.
"""

from __future__ import annotations

import argparse
from array import array
import contextlib
import gc
import hashlib
import json
import os
from pathlib import Path
import queue
import re
import socket
import stat
import struct
import sys
import threading
import time
import traceback
from typing import Any


PROTOCOL_STDOUT = sys.stdout
sys.stdout = sys.stderr

E5_MODEL_ID = "e5-small-int8"
E5_MODEL_REVISION = "614241f622f53c4eeff9890bdc4f31cfecc418b3"
E5_MODEL_SHA256 = "dd476dd0c2514e9b9be83aeb3853fac0763e0bdf4a71645407587d77c48a2d88"
E5_TOKENIZER_SHA256 = "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39"
OPENWAKEWORD_MODEL_SHA256 = "94a13cfe60075b132f6a472e7e462e8123ee70861bc3fb58434a73712ee0d2cb"
OPENWAKEWORD_MELSPEC_SHA256 = "ba2b0e0f8b7b875369a2c89cb13360ff53bac436f2895cced9f479fa65eb176f"
OPENWAKEWORD_EMBEDDING_SHA256 = "70d164290c1d095d1d4ee149bc5e00543250a7316b59f31d056cff7bd3075c1f"
BUILTIN_WAKE_PHRASES = frozenset({"hey jarvis", "jarvis"})
SILERO_VAD_VERSION = "v5.1.2"
SILERO_VAD_REVISION = "6478567951ae5c9979ad7b234185b5515f4be7a1"
SILERO_VAD_SHA256 = "2623a2953f6ff3d2c1e61740c6cdb7168133479b267dfef114a4a3cc5bdd788f"
SILERO_VAD_WINDOW_SAMPLES = 512
SILERO_VAD_CONTEXT_SAMPLES = 64
FASTER_WHISPER_PACKAGE_VERSION = "1.2.1"
FASTER_WHISPER_WHEEL_SHA256 = "79a66ad50688c0b794dd501dc340a736992a6342f7f95e5811be60b5224a26a7"
FASTER_WHISPER_MODEL_ID = "mobiuslabsgmbh/faster-whisper-large-v3-turbo"
FASTER_WHISPER_MODEL_REVISION = "0a363e9161cbc7ed1431c9597a8ceaf0c4f78fcf"
FASTER_WHISPER_COMPUTE_TYPE = "int8_float16"
FASTER_WHISPER_RUNTIME_DEPENDENCIES = [
    "av==16.0.1",
    "certifi==2026.7.22",
    "charset-normalizer==3.5.1",
    "ctranslate2==4.7.1",
    "filelock==3.32.4",
    "flatbuffers==25.12.19",
    "fsspec==2026.7.0",
    "hf-xet==1.6.0",
    "huggingface-hub==0.36.2",
    "idna==3.19",
    "numpy==2.5.2",
    "nvidia-cublas-cu12==12.9.1.4",
    "nvidia-cudnn-cu12==9.16.0.29",
    "onnxruntime==1.28.0",
    "packaging==26.3",
    "protobuf==7.36.0",
    "PyYAML==6.0.3",
    "requests==2.34.2",
    "setuptools==84.0.0",
    "tokenizers==0.22.2",
    "tqdm==4.70.0",
    "typing-extensions==4.16.0",
    "urllib3==2.7.0",
]
FASTER_WHISPER_MODEL_SHA256 = {
    "config.json": "b0253ea6c0d3bea6b1e19e91a02acfd3b53f4467362efcb5a3e6b16c9b3a9b7e",
    "model.bin": "e76620f83d5f5b69efd3d87e3dc180c1bd21df9fbebacfd4335e5e1efcc018da",
    "preprocessor_config.json": "7ccc62c6f2765af1f3b46c00c9b5894426835a05021c8b9c01eecb6dfb542711",
    "tokenizer.json": "297b13372ac43916285644fb9687add3cc62ee2a1adb60da3dc25cc94c1871fd",
    "vocabulary.json": "c69260f2ab26d659b7c398f9a2b2b48ed0df16c3b47d7326782fd9cba71690c1",
}
KOKORO_PACKAGE_VERSION = "0.6.1"
KOKORO_WHEEL_SHA256 = "50c8de4950d601df41428ee5462a48c8a78bef441bf671f2492e070ef44d8a32"
KOKORO_MODEL_RELEASE = "model-files-v1.1"
KOKORO_MODEL_SHA256 = "ae315a79b623f244700e4afb9246c46a26066782e049ba174bf3ba433970ee9c"
KOKORO_VOICES_SHA256 = "bca610b8308e8d99f32e6fe4197e7ec01679264efed0cac9140fe9c29f1fbf7d"
KOKORO_RUNTIME_DEPENDENCIES = [
    "attrs==26.1.0",
    "cffi==2.1.1",
    "dlinfo==2.0.0",
    "espeakng-loader==0.2.4",
    "joblib==1.5.3",
    "numpy==2.5.2",
    "onnxruntime==1.28.0",
    "phonemizer==3.4.0",
    "pycparser==3.0",
    "soundfile==0.14.0",
    "typing-extensions==4.16.0",
]
KOKORO_DEFAULT_VOICE = "af_heart"
KOKORO_SAMPLE_RATE_HZ = 24_000
FASTER_WHISPER_WINDOW_SAMPLES = 16_000 * 3
FASTER_WHISPER_FIRST_PARTIAL_SAMPLES = FASTER_WHISPER_WINDOW_SAMPLES
FASTER_WHISPER_OVERLAP_SAMPLES = 16_000 // 2
FASTER_WHISPER_HOP_SAMPLES = (
    FASTER_WHISPER_WINDOW_SAMPLES - FASTER_WHISPER_OVERLAP_SAMPLES
)
FASTER_WHISPER_PARTIAL_ENCODER_FRAMES = 600
FASTER_WHISPER_PARTIAL_MAX_TOKENS = 48
MOONSHINE_STREAM_QUEUE_MAX_SAMPLES = 16_000 * 5
STT_MAX_SAMPLES = 16_000 * 60 * 10
TTS_CLAUSE_MAX_CHARACTERS = 220
TTS_STREAM_MAX_FRAME_BYTES = 8 * 1024 * 1024
TTS_STREAM_DELIMITERS = frozenset(".!?;:")


def highest_frequency_cpu_tier(
    allowed: set[int], frequencies: dict[int, int]
) -> list[int]:
    """Select one complete, non-uniform highest-frequency logical CPU tier."""
    if not allowed or set(frequencies) != allowed:
        return []
    peak = max(frequencies.values())
    tier = sorted(cpu for cpu, frequency in frequencies.items() if frequency == peak)
    if not tier or len(tier) == len(allowed):
        return []
    return tier


def prefer_highest_frequency_cpu_tier() -> list[int]:
    """Let native model threads inherit the fastest CPU tier when detectable.

    Linux hybrid CPUs expose each logical CPU's hardware maximum in sysfs. If
    that complete topology is unavailable, uniform, or cannot be applied, the
    worker leaves scheduler policy untouched. Other platforms fail open.
    """
    get_affinity = getattr(os, "sched_getaffinity", None)
    set_affinity = getattr(os, "sched_setaffinity", None)
    if get_affinity is None or set_affinity is None:
        return []
    try:
        allowed = set(get_affinity(0))
        frequencies = {
            cpu: int(
                Path(
                    f"/sys/devices/system/cpu/cpu{cpu}/cpufreq/cpuinfo_max_freq"
                ).read_text(encoding="ascii")
            )
            for cpu in allowed
        }
        tier = highest_frequency_cpu_tier(allowed, frequencies)
        if not tier:
            return []
        set_affinity(0, tier)
        return tier
    except (OSError, ValueError):
        return []


def normalized_phrase(value: Any) -> str:
    return " ".join(re.findall(r"[a-z0-9']+", str(value).lower()))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def transcript_text(transcript: Any) -> str:
    if transcript is None:
        return ""
    return " ".join(
        str(line.text).strip()
        for line in getattr(transcript, "lines", [])
        if str(getattr(line, "text", "")).strip()
    ).strip()


def tts_clauses(value: Any) -> list[str]:
    """Split text at spoken clause boundaries without slicing encoded UTF-8."""
    text = " ".join(str(value).split())
    clauses: list[str] = []
    pending: list[str] = []
    for character in text:
        pending.append(character)
        if (
            character in TTS_STREAM_DELIMITERS
            or len(pending) >= TTS_CLAUSE_MAX_CHARACTERS
        ):
            clause = "".join(pending).strip()
            if clause:
                clauses.append(clause)
            pending = []
    clause = "".join(pending).strip()
    if clause:
        clauses.append(clause)
    return clauses


class VoiceRuntime:
    def __init__(self, root: Path) -> None:
        # Run before constructing any ONNX/CTranslate2/Torch model so its
        # native threads inherit the detected performance-core affinity.
        self.performance_cpu_tier = prefer_highest_frequency_cpu_tier()
        self.root = root
        self.models = root / "models"
        self.moonshine = None
        self.moonshine_thread_mode = ""
        self.moonshine_partial_lines: dict[int, str] = {}
        self.moonshine_state_lock = threading.Lock()
        self.moonshine_jobs: queue.Queue[tuple[str, Any]] | None = None
        self.moonshine_done: queue.Queue[tuple[bool, Any]] | None = None
        self.moonshine_thread: threading.Thread | None = None
        self.moonshine_cancel: threading.Event | None = None
        self.moonshine_queued_samples = 0
        self.moonshine_processed_samples = 0
        self.moonshine_decode_boundary_samples = 0
        self.stream = None
        self.stt_engine = ""
        self.stt_language = "en"
        self.stt_vocabulary: list[str] = []
        self.stt_audio = array("f")
        self.next_partial_samples = FASTER_WHISPER_FIRST_PARTIAL_SAMPLES
        self.partial_text = ""
        self.partial_audio_samples = 0
        self.qwen = None
        self.qwen_kind = ""
        self.kokoro = None
        self.smart_turn = None
        self.turn_audio: list[float] = []
        self.silero_vad = None
        self.vad_state = None
        self.vad_context = None
        self.vad_pending_samples: list[float] = []
        self.vad_last_probability = 0.0
        self.embed_session = None
        self.embed_tokenizer = None
        self.wake_model = None
        self.wake_active = False
        self.wake_fallback = False
        self.wake_phrases: list[str] = []
        self.wake_threshold = 0.5
        self.wake_pending_samples: list[float] = []
        # STT-3 and DESK-5 populate these slots. Defining them now keeps the
        # unload protocol stable before those optional GPU engines are installed.
        self.faster_whisper = None
        self.vision_grounding = None

    def _faster_whisper_marker(self) -> dict[str, Any]:
        marker = self.root / "faster-whisper.json"
        if not marker.is_file():
            raise RuntimeError(
                "faster-whisper large-v3-turbo is not installed; install the Accurate voice profile"
            )
        manifest = json.loads(marker.read_text(encoding="utf-8"))
        expected = {
            "package": f"faster-whisper=={FASTER_WHISPER_PACKAGE_VERSION}",
            "wheel_sha256": FASTER_WHISPER_WHEEL_SHA256,
            "model_id": FASTER_WHISPER_MODEL_ID,
            "revision": FASTER_WHISPER_MODEL_REVISION,
            "compute_type": FASTER_WHISPER_COMPUTE_TYPE,
            "dependencies": FASTER_WHISPER_RUNTIME_DEPENDENCIES,
        }
        for key, value in expected.items():
            if manifest.get(key) != value:
                raise RuntimeError(
                    f"faster-whisper install manifest has an unexpected {key}"
                )
        model_path = Path(str(manifest.get("model_path", "")))
        if not model_path.is_dir() or not model_path.is_relative_to(self.root):
            raise RuntimeError("faster-whisper model files are missing or outside the voice root")
        required = manifest.get("files")
        if required != FASTER_WHISPER_MODEL_SHA256:
            raise RuntimeError("faster-whisper install manifest is incomplete")
        for name, expected_digest in required.items():
            asset = model_path / name
            if not asset.is_file():
                raise RuntimeError(f"faster-whisper model file is missing: {name}")
            actual_digest = sha256_file(asset)
            if actual_digest != expected_digest:
                raise RuntimeError(
                    f"faster-whisper model digest mismatch for {name}: "
                    f"expected {expected_digest}, found {actual_digest}"
                )
        manifest["model_path"] = str(model_path)
        return manifest

    def _load_faster_whisper(self) -> None:
        if self.faster_whisper is not None:
            return
        manifest = self._faster_whisper_marker()
        from importlib.metadata import PackageNotFoundError, version

        for specification in [
            f"faster-whisper=={FASTER_WHISPER_PACKAGE_VERSION}",
            *FASTER_WHISPER_RUNTIME_DEPENDENCIES,
        ]:
            package, expected_version = specification.split("==", maxsplit=1)
            try:
                installed_version = version(package)
            except PackageNotFoundError as error:
                raise RuntimeError(
                    f"Accurate STT runtime dependency is missing: {package}"
                ) from error
            if installed_version != expected_version:
                raise RuntimeError(
                    f"Accurate STT runtime dependency mismatch for {package}: "
                    f"expected {expected_version}, found {installed_version}"
                )
        import ctranslate2
        from faster_whisper import WhisperModel

        if ctranslate2.get_cuda_device_count() < 1:
            raise RuntimeError("faster-whisper Accurate profile requires a CUDA GPU")
        self.faster_whisper = WhisperModel(
            manifest["model_path"],
            device="cuda",
            device_index=0,
            compute_type=FASTER_WHISPER_COMPUTE_TYPE,
            local_files_only=True,
        )
        # CTranslate2 initializes several CUDA kernels lazily. Paying that
        # one-time cost on the first live three-second window turns an
        # otherwise warm ~150 ms partial into a near-one-second cold outlier.
        # Warm the exact low-level partial path once while model loading is
        # already allowed to block; the zero samples are never exposed as a
        # transcript or added to a user session.
        import numpy as np

        try:
            self._faster_whisper_partial_decode(
                np.zeros(FASTER_WHISPER_WINDOW_SAMPLES, dtype=np.float32), None
            )
        except BaseException:
            # A partially initialized CUDA model must not make a retry look
            # loaded after the start request correctly reports failure.
            self.faster_whisper = None
            gc.collect()
            raise

    def _reset_stt_session(self) -> None:
        self.stt_engine = ""
        self.stt_language = "en"
        self.stt_vocabulary = []
        self.stt_audio = array("f")
        self.next_partial_samples = FASTER_WHISPER_FIRST_PARTIAL_SAMPLES
        with self.moonshine_state_lock:
            self.partial_text = ""
            self.partial_audio_samples = 0
            self.moonshine_partial_lines = {}
            self.moonshine_queued_samples = 0
            self.moonshine_processed_samples = 0
            self.moonshine_decode_boundary_samples = 0

    def _capture_moonshine_event(self, event: Any) -> None:
        """Keep the latest lines emitted by Moonshine's adaptive update gate."""
        line = getattr(event, "line", None)
        line_id = getattr(line, "line_id", None)
        if line is None or not isinstance(line_id, int):
            return
        text = str(getattr(line, "text", "")).strip()
        with self.moonshine_state_lock:
            if text:
                self.moonshine_partial_lines[line_id] = text
            else:
                self.moonshine_partial_lines.pop(line_id, None)
            self.partial_text = " ".join(
                self.moonshine_partial_lines[key]
                for key in sorted(self.moonshine_partial_lines)
            ).strip()
            # This is the amount of source audio the background stream had
            # accepted when it produced this text, not the newer amount the
            # protocol thread may already have queued.
            self.partial_audio_samples = self.moonshine_decode_boundary_samples

    def _run_moonshine_stream(
        self,
        stream: Any,
        jobs: queue.Queue[tuple[str, Any]],
        done: queue.Queue[tuple[bool, Any]],
        cancelled: threading.Event,
    ) -> None:
        """Own all calls into one Moonshine stream on a single worker thread."""
        try:
            while True:
                action, payload = jobs.get()
                if action == "audio":
                    samples, sample_rate_hz = payload
                    try:
                        if not cancelled.is_set():
                            with self.moonshine_state_lock:
                                boundary = self.moonshine_processed_samples + len(samples)
                                self.moonshine_decode_boundary_samples = boundary
                            stream.add_audio(samples, sample_rate_hz)
                            with self.moonshine_state_lock:
                                self.moonshine_processed_samples = boundary
                    finally:
                        with self.moonshine_state_lock:
                            self.moonshine_queued_samples = max(
                                0, self.moonshine_queued_samples - len(samples)
                            )
                    continue
                if action == "stop":
                    with self.moonshine_state_lock:
                        self.moonshine_decode_boundary_samples = (
                            self.moonshine_processed_samples
                        )
                    done.put((True, stream.stop()))
                    return
                if action == "cancel":
                    done.put((True, None))
                    return
                raise RuntimeError(f"unknown Moonshine stream job: {action}")
        except BaseException as error:
            done.put((False, error))
        finally:
            stream.close()

    def _start_moonshine_stream(self) -> None:
        if self.moonshine is None:
            raise RuntimeError("Moonshine Medium Streaming is not loaded")
        stream = self.moonshine.create_stream(update_interval=0.45)
        stream.add_listener(self._capture_moonshine_event)
        stream.start()
        jobs: queue.Queue[tuple[str, Any]] = queue.Queue(maxsize=256)
        done: queue.Queue[tuple[bool, Any]] = queue.Queue(maxsize=1)
        cancelled = threading.Event()
        worker = threading.Thread(
            target=self._run_moonshine_stream,
            args=(stream, jobs, done, cancelled),
            name="moonshine-stream",
            daemon=True,
        )
        self.stream = stream
        self.moonshine_jobs = jobs
        self.moonshine_done = done
        self.moonshine_cancel = cancelled
        self.moonshine_thread = worker
        try:
            worker.start()
        except BaseException:
            self.stream = None
            self.moonshine_jobs = None
            self.moonshine_done = None
            self.moonshine_cancel = None
            self.moonshine_thread = None
            stream.close()
            raise

    def _enqueue_moonshine_audio(
        self, samples: list[Any], sample_rate_hz: int
    ) -> tuple[str, int]:
        jobs = self.moonshine_jobs
        worker = self.moonshine_thread
        if jobs is None or worker is None or self.stream is None:
            raise RuntimeError("no Moonshine stream is active")
        if not worker.is_alive():
            raise RuntimeError("Moonshine stream worker exited unexpectedly")
        with self.moonshine_state_lock:
            queued = self.moonshine_queued_samples + len(samples)
            if queued > MOONSHINE_STREAM_QUEUE_MAX_SAMPLES:
                raise RuntimeError("Moonshine stream queue exceeds five seconds of audio")
            self.moonshine_queued_samples = queued
        try:
            jobs.put_nowait(("audio", (samples, sample_rate_hz)))
        except queue.Full as error:
            with self.moonshine_state_lock:
                self.moonshine_queued_samples = max(
                    0, self.moonshine_queued_samples - len(samples)
                )
            raise RuntimeError("Moonshine stream queue is full") from error
        with self.moonshine_state_lock:
            return self.partial_text, self.partial_audio_samples

    def _finish_moonshine_stream(self, *, cancel: bool) -> Any:
        jobs = self.moonshine_jobs
        done = self.moonshine_done
        worker = self.moonshine_thread
        cancelled = self.moonshine_cancel
        if (
            jobs is None
            or done is None
            or worker is None
            or cancelled is None
            or self.stream is None
        ):
            return None
        try:
            if cancel:
                cancelled.set()
                while True:
                    try:
                        action, payload = jobs.get_nowait()
                    except queue.Empty:
                        break
                    if action == "audio":
                        samples, _sample_rate_hz = payload
                        with self.moonshine_state_lock:
                            self.moonshine_queued_samples = max(
                                0, self.moonshine_queued_samples - len(samples)
                            )
                jobs.put(("cancel", None), timeout=5)
            else:
                # FIFO placement means every accepted audio frame is processed
                # before the one final endpoint decode.
                jobs.put(("stop", None), timeout=5)
            try:
                succeeded, value = done.get(timeout=120 if not cancel else 15)
            except queue.Empty as error:
                raise RuntimeError("Moonshine stream worker did not stop") from error
            worker.join(timeout=1)
            if worker.is_alive():
                raise RuntimeError("Moonshine stream worker did not exit")
            if not succeeded:
                raise RuntimeError(f"Moonshine stream failed: {value}") from value
            return value
        finally:
            self.stream = None
            self.moonshine_jobs = None
            self.moonshine_done = None
            self.moonshine_cancel = None
            self.moonshine_thread = None
            with self.moonshine_state_lock:
                self.moonshine_queued_samples = 0

    def _faster_whisper_decode(self, audio: Any, *, final: bool) -> str:
        if self.faster_whisper is None:
            raise RuntimeError("faster-whisper is not loaded")
        import numpy as np

        if isinstance(audio, (str, Path)):
            source: Any = str(audio)
        else:
            samples = np.asarray(audio, dtype=np.float32)
            if samples.size == 0:
                return ""
            source = samples
        initial_prompt = ", ".join(self.stt_vocabulary[:128]).strip() or None
        if not final:
            return self._faster_whisper_partial_decode(source, initial_prompt)
        segments, _info = self.faster_whisper.transcribe(
            source,
            language=self.stt_language,
            beam_size=5,
            best_of=5,
            temperature=[0.0, 0.2, 0.4, 0.6, 0.8, 1.0],
            condition_on_previous_text=True,
            vad_filter=False,
            word_timestamps=False,
            initial_prompt=initial_prompt,
        )
        return " ".join(
            str(segment.text).strip()
            for segment in segments
            if str(getattr(segment, "text", "")).strip()
        ).strip()

    def _faster_whisper_partial_decode(
        self, samples: Any, initial_prompt: str | None
    ) -> str:
        """Run one bounded greedy decode over a real three-second window.

        faster-whisper's public streaming path pads every short input to the
        Whisper 30-second encoder width. The pinned CTranslate2 model accepts a
        shorter feature sequence, so partials use the same model/tokenizer with
        six seconds of encoder features (three seconds of captured audio plus
        zero padding). This is not a wider audio window. The endpoint still
        uses the public full-width, beam-search transcription path above.
        """
        import numpy as np
        from faster_whisper.audio import pad_or_trim
        from faster_whisper.tokenizer import Tokenizer

        model = self.faster_whisper
        if model is None:
            raise RuntimeError("faster-whisper is not loaded")
        unused_segments, info = model.transcribe(
            samples,
            language=self.stt_language,
            beam_size=1,
            best_of=1,
            temperature=0.0,
            condition_on_previous_text=False,
            vad_filter=False,
            word_timestamps=False,
            without_timestamps=True,
            max_new_tokens=FASTER_WHISPER_PARTIAL_MAX_TOKENS,
            no_repeat_ngram_size=3,
            repetition_penalty=1.1,
            initial_prompt=initial_prompt,
        )
        del unused_segments
        tokenizer = Tokenizer(
            model.hf_tokenizer,
            model.model.is_multilingual,
            task="transcribe",
            language=self.stt_language,
        )
        features = model.feature_extractor(
            np.asarray(samples, dtype=np.float32)
        )[..., :-1]
        features = pad_or_trim(
            features, length=FASTER_WHISPER_PARTIAL_ENCODER_FRAMES
        )
        encoder_output = model.encode(features)
        previous_tokens = (
            tokenizer.encode(" " + initial_prompt.strip()) if initial_prompt else []
        )
        prompt = model.get_prompt(
            tokenizer,
            previous_tokens,
            without_timestamps=True,
        )
        result, average_log_probability, _temperature, _compression_ratio = (
            model.generate_with_fallback(
                encoder_output,
                prompt,
                tokenizer,
                info.transcription_options,
            )
        )
        options = info.transcription_options
        if (
            options.no_speech_threshold is not None
            and result.no_speech_prob > options.no_speech_threshold
            and options.log_prob_threshold is not None
            and average_log_probability < options.log_prob_threshold
        ):
            return ""
        return tokenizer.decode(result.sequences_ids[0]).strip()

    def _moonshine_marker(self) -> dict[str, Any]:
        marker = self.root / "moonshine.json"
        if not marker.is_file():
            raise RuntimeError("Moonshine Medium Streaming is not installed")
        return json.loads(marker.read_text(encoding="utf-8"))

    def _load_moonshine(
        self, vocabulary: list[str] | None = None, *, streaming: bool
    ) -> None:
        wanted_thread_mode = "single" if streaming else "default"
        if self.moonshine is not None and self.moonshine_thread_mode != wanted_thread_mode:
            if self.stream is not None:
                raise RuntimeError("cannot change Moonshine mode while a stream is active")
            self.moonshine.close()
            self.moonshine = None
            self.moonshine_thread_mode = ""
            gc.collect()
        # Moonshine exposes this native-runtime switch so its many resident
        # ONNX sessions do not leave spinning thread pools that starve the
        # latency-bound Silero/Smart Turn endpoint path. Batch transcription
        # retains the normal scheduler because it is throughput-bound.
        if streaming:
            os.environ["MOONSHINE_ORT_SINGLE_THREAD"] = "1"
        else:
            os.environ.pop("MOONSHINE_ORT_SINGLE_THREAD", None)
        from moonshine_voice import ModelArch
        from moonshine_voice.transcriber import Transcriber

        marker = self._moonshine_marker()
        model_path = Path(str(marker["model_path"]))
        if not model_path.is_dir():
            raise RuntimeError("Moonshine model files are missing")
        wanted_arch = ModelArch(int(marker.get("model_arch", 5)))
        if self.moonshine is None:
            self.moonshine = Transcriber(
                model_path,
                model_arch=wanted_arch,
                update_interval=0.45,
                options={
                    # Partial text is a product requirement. The native
                    # speculative path is intentionally absent: repeated
                    # pinned-corpus measurements increased its p95 tail.
                    "decode_incomplete_lines": "true",
                },
            )
            self.moonshine_thread_mode = wanted_thread_mode
        if vocabulary and hasattr(self.moonshine, "set_keyterms"):
            self.moonshine.set_keyterms(vocabulary[:128])

    def _load_smart_turn(self) -> bool:
        smart_turn_path = self.models / "smart-turn-v3.2-cpu.onnx"
        if not smart_turn_path.is_file():
            return False
        if self.smart_turn is None:
            from smart_turn import SmartTurn

            # Create the small endpoint model before Moonshine claims its own
            # ONNX execution resources. This avoids starving the latency-bound
            # endpoint consultation after a streaming decode.
            self.smart_turn = SmartTurn(smart_turn_path, cpu_count=4)
        return True

    def _qwen_model_path(self, requested: str) -> tuple[Path, str]:
        custom = self.models / "qwen3-tts-0.6b-customvoice"
        base = self.models / "qwen3-tts-0.6b-base"
        if requested == "base" and base.is_dir():
            return base, "base"
        if custom.is_dir():
            return custom, "custom"
        if base.is_dir():
            return base, "base"
        raise RuntimeError("Qwen3-TTS 0.6B is not installed")

    def _load_qwen(self, requested: str) -> str:
        model_path, kind = self._qwen_model_path(requested)
        if self.qwen is not None and self.qwen_kind == kind:
            return kind
        import torch
        from qwen_tts import Qwen3TTSModel

        if not torch.cuda.is_available():
            raise RuntimeError("Qwen3-TTS requires the configured CUDA GPU")
        if self.qwen is not None:
            del self.qwen
            self.qwen = None
            torch.cuda.empty_cache()
        self.qwen = Qwen3TTSModel.from_pretrained(
            str(model_path),
            device_map="cuda:0",
            # Qwen's code sampler is numerically unstable in FP16 on Ada laptop
            # GPUs; BF16 preserves its exponent range and is native on RTX 40xx.
            dtype=torch.bfloat16,
            attn_implementation="sdpa",
        )
        self.qwen_kind = kind
        return kind

    def _kokoro_marker(self) -> dict[str, Any]:
        marker = self.root / "kokoro.json"
        if not marker.is_file():
            raise RuntimeError(
                "Kokoro CPU voice is not installed; install the Kokoro voice profile"
            )
        manifest = json.loads(marker.read_text(encoding="utf-8"))
        expected = {
            "package": f"kokoro-onnx=={KOKORO_PACKAGE_VERSION}",
            "wheel_sha256": KOKORO_WHEEL_SHA256,
            "model_release": KOKORO_MODEL_RELEASE,
            "quantization": "int8",
            "dependencies": KOKORO_RUNTIME_DEPENDENCIES,
        }
        for key, value in expected.items():
            if manifest.get(key) != value:
                raise RuntimeError(f"Kokoro install manifest has an unexpected {key}")
        model_path = Path(str(manifest.get("model_path", "")))
        expected_path = self.models / "kokoro-v1.0-int8"
        if model_path != expected_path or not model_path.is_dir():
            raise RuntimeError("Kokoro model files are missing or outside the voice root")
        required = manifest.get("files")
        expected_files = {
            "kokoro-v1.0.int8.onnx": KOKORO_MODEL_SHA256,
            "voices-v1.0.bin": KOKORO_VOICES_SHA256,
        }
        if required != expected_files:
            raise RuntimeError("Kokoro install manifest is incomplete")
        for name, expected_digest in expected_files.items():
            asset = model_path / name
            if not asset.is_file():
                raise RuntimeError(f"Kokoro model file is missing: {name}")
            actual_digest = sha256_file(asset)
            if actual_digest != expected_digest:
                raise RuntimeError(
                    f"Kokoro model digest mismatch for {name}: "
                    f"expected {expected_digest}, found {actual_digest}"
                )
        manifest["model_path"] = str(model_path)
        return manifest

    def _load_kokoro(self) -> None:
        if self.kokoro is not None:
            return
        manifest = self._kokoro_marker()
        from importlib.metadata import PackageNotFoundError, version

        for specification in [
            f"kokoro-onnx=={KOKORO_PACKAGE_VERSION}",
            *KOKORO_RUNTIME_DEPENDENCIES,
        ]:
            package, expected_version = specification.split("==", maxsplit=1)
            try:
                installed_version = version(package)
            except PackageNotFoundError as error:
                raise RuntimeError(
                    f"Kokoro runtime dependency is missing: {package}"
                ) from error
            if installed_version != expected_version:
                raise RuntimeError(
                    f"Kokoro runtime dependency mismatch for {package}: "
                    f"expected {expected_version}, found {installed_version}"
                )
        from kokoro_onnx import Kokoro

        model_path = Path(manifest["model_path"])
        self.kokoro = Kokoro(
            str(model_path / "kokoro-v1.0.int8.onnx"),
            str(model_path / "voices-v1.0.bin"),
        )

    @staticmethod
    def _kokoro_voice(request: dict[str, Any]) -> str:
        voice = str(request.get("voice", "")).strip()
        # Existing Qwen profiles store `Ryan`; crossing into the CPU fallback
        # must never pass that incompatible speaker name into Kokoro.
        return KOKORO_DEFAULT_VOICE if not voice or voice == "Ryan" else voice

    @staticmethod
    def _kokoro_speed(request: dict[str, Any]) -> float:
        raw = request.get("speech_rate_percent", 100)
        if not isinstance(raw, (int, float)) or isinstance(raw, bool):
            raise RuntimeError("Kokoro speech rate must be numeric")
        return min(200.0, max(50.0, float(raw))) / 100.0

    def _kokoro_create(
        self, text: str, request: dict[str, Any]
    ) -> tuple[Any, int, str]:
        self._load_kokoro()
        voice = self._kokoro_voice(request)
        waveform, sample_rate = self.kokoro.create(
            text,
            voice=voice,
            speed=self._kokoro_speed(request),
            lang="en-us",
        )
        sample_rate = int(sample_rate)
        if sample_rate != KOKORO_SAMPLE_RATE_HZ:
            raise RuntimeError(
                f"Kokoro returned {sample_rate} Hz; expected {KOKORO_SAMPLE_RATE_HZ} Hz"
            )
        return waveform, sample_rate, voice

    def _embed_paths(self) -> tuple[Path, Path]:
        root = self.models / "multilingual-e5-small-int8"
        return root / "model.onnx", root / "tokenizer.json"

    def _wake_paths(self) -> tuple[Path, Path, Path]:
        root = self.models / "openwakeword"
        return (
            root / "hey_jarvis_v0.1.onnx",
            root / "melspectrogram.onnx",
            root / "embedding_model.onnx",
        )

    def _silero_vad_path(self) -> Path:
        return self.models / "silero-vad-v5.1.2.onnx"

    def _reset_silero_vad(self) -> None:
        self.vad_pending_samples = []
        self.vad_last_probability = 0.0
        if self.silero_vad is None:
            self.vad_state = None
            self.vad_context = None
            return
        import numpy as np

        # Silero VAD v5 uses one combined recurrent state, unlike the older
        # openWakeWord-bundled model's separate h/c tensors.
        self.vad_state = np.zeros((2, 1, 128), dtype=np.float32)
        self.vad_context = np.zeros(
            (1, SILERO_VAD_CONTEXT_SAMPLES), dtype=np.float32
        )

    def _load_silero_vad(self) -> None:
        if self.silero_vad is not None:
            return
        model = self._silero_vad_path()
        if not model.is_file():
            raise RuntimeError(f"Silero VAD {SILERO_VAD_VERSION} is not installed")
        found = sha256_file(model)
        if found != SILERO_VAD_SHA256:
            raise RuntimeError(
                "Silero VAD v5 asset digest mismatch: "
                f"expected {SILERO_VAD_SHA256}, found {found}"
            )

        import onnxruntime as ort

        options = ort.SessionOptions()
        options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
        options.inter_op_num_threads = 1
        options.intra_op_num_threads = 1
        options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        session = ort.InferenceSession(
            str(model), sess_options=options, providers=["CPUExecutionProvider"]
        )
        inputs = {item.name: item for item in session.get_inputs()}
        outputs = {item.name: item for item in session.get_outputs()}
        if set(inputs) != {"input", "state", "sr"} or set(outputs) != {
            "output",
            "stateN",
        }:
            raise RuntimeError(
                "Silero VAD model does not expose the pinned v5 input/state contract"
            )
        state_shape = inputs["state"].shape
        if (
            len(state_shape) != 3
            or state_shape[0] != 2
            or state_shape[2] != 128
            or inputs["sr"].type != "tensor(int64)"
        ):
            raise RuntimeError(
                "Silero VAD model has an unexpected recurrent-state contract"
            )
        self.silero_vad = session
        self._reset_silero_vad()

    def _speech_probability(
        self, samples: list[Any], sample_rate_hz: int
    ) -> tuple[float, int]:
        if sample_rate_hz != 16_000:
            raise RuntimeError("Silero VAD chunks must be 16 kHz mono audio")
        self._load_silero_vad()
        import numpy as np

        audio = np.asarray(self.vad_pending_samples + samples, dtype=np.float32)
        audio = np.nan_to_num(audio, nan=0.0, posinf=1.0, neginf=-1.0)
        audio = np.clip(audio, -1.0, 1.0)
        complete_samples = (
            audio.size // SILERO_VAD_WINDOW_SAMPLES
        ) * SILERO_VAD_WINDOW_SAMPLES
        self.vad_pending_samples = audio[complete_samples:].tolist()
        probabilities: list[float] = []
        for offset in range(0, complete_samples, SILERO_VAD_WINDOW_SAMPLES):
            frame = audio[
                offset : offset + SILERO_VAD_WINDOW_SAMPLES
            ].reshape(1, -1)
            model_input = np.concatenate((self.vad_context, frame), axis=1)
            output, state = self.silero_vad.run(
                ["output", "stateN"],
                {
                    "input": model_input,
                    "state": self.vad_state,
                    "sr": np.asarray(16_000, dtype=np.int64),
                },
            )
            self.vad_state = np.asarray(state, dtype=np.float32)
            self.vad_context = model_input[:, -SILERO_VAD_CONTEXT_SAMPLES:]
            probabilities.append(
                max(0.0, min(1.0, float(np.asarray(output).reshape(-1)[0])))
            )
        if probabilities:
            # The maximum makes a mixed speech/tail-silence transport chunk a
            # speech chunk. A following all-silence chunk then trips the stop
            # threshold without cutting off the final phoneme.
            self.vad_last_probability = max(probabilities)
        return self.vad_last_probability, len(probabilities)

    def _load_wake_model(self) -> None:
        if self.wake_model is not None:
            self.wake_model.reset()
            return
        model, melspec, embedding = self._wake_paths()
        expected = (
            (model, OPENWAKEWORD_MODEL_SHA256),
            (melspec, OPENWAKEWORD_MELSPEC_SHA256),
            (embedding, OPENWAKEWORD_EMBEDDING_SHA256),
        )
        for path, wanted in expected:
            if not path.is_file():
                raise RuntimeError(
                    "the pinned hey-jarvis openWakeWord model is not installed"
                )
            found = sha256_file(path)
            if found != wanted:
                raise RuntimeError(
                    f"wake asset digest mismatch for {path.name}: "
                    f"expected {wanted}, found {found}"
                )

        from openwakeword.model import Model

        self.wake_model = Model(
            wakeword_models=[str(model)],
            inference_framework="onnx",
            melspec_model_path=str(melspec),
            embedding_model_path=str(embedding),
        )

    def _load_embedder(self) -> None:
        if self.embed_session is not None and self.embed_tokenizer is not None:
            return
        model, tokenizer_file = self._embed_paths()
        if not model.is_file() or not tokenizer_file.is_file():
            raise RuntimeError(
                "the pinned multilingual E5 embedder is not installed; "
                "feature-hash fallback remains active"
            )
        expected = (
            (model, E5_MODEL_SHA256),
            (tokenizer_file, E5_TOKENIZER_SHA256),
        )
        for path, wanted in expected:
            found = sha256_file(path)
            if found != wanted:
                raise RuntimeError(
                    f"embedding asset digest mismatch for {path.name}: "
                    f"expected {wanted}, found {found}"
                )

        import onnxruntime as ort
        from tokenizers import Tokenizer

        tokenizer = Tokenizer.from_file(str(tokenizer_file))
        tokenizer.enable_truncation(max_length=512)
        tokenizer.enable_padding(pad_id=1, pad_token="<pad>")
        options = ort.SessionOptions()
        options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
        self.embed_session = ort.InferenceSession(
            str(model),
            sess_options=options,
            providers=["CPUExecutionProvider"],
        )
        self.embed_tokenizer = tokenizer

    def embed(self, request: dict[str, Any]) -> dict[str, Any]:
        texts = request.get("texts")
        if not isinstance(texts, list) or not texts or len(texts) > 64:
            raise RuntimeError("embed expects between one and 64 texts")
        normalized = [str(text).strip() for text in texts]
        if any(not text or len(text) > 8_192 for text in normalized):
            raise RuntimeError("embedding texts must be non-empty and at most 8192 characters")
        if sum(len(text) for text in normalized) > 65_536:
            raise RuntimeError("embedding batch exceeds 65536 characters")
        input_type = str(request.get("input_type", "passage"))
        if input_type not in {"passage", "query"}:
            raise RuntimeError("embedding input_type must be passage or query")

        self._load_embedder()
        import numpy as np

        encoded = self.embed_tokenizer.encode_batch(
            [f"{input_type}: {text}" for text in normalized]
        )
        input_ids = np.asarray([item.ids for item in encoded], dtype=np.int64)
        attention_mask = np.asarray(
            [item.attention_mask for item in encoded], dtype=np.int64
        )
        token_type_ids = np.asarray(
            [item.type_ids for item in encoded], dtype=np.int64
        )
        available_inputs = {item.name for item in self.embed_session.get_inputs()}
        feed = {
            "input_ids": input_ids,
            "attention_mask": attention_mask,
        }
        if "token_type_ids" in available_inputs:
            feed["token_type_ids"] = token_type_ids
        hidden = self.embed_session.run(None, feed)[0]
        weights = attention_mask[:, :, None].astype(np.float32)
        vectors = (hidden * weights).sum(axis=1) / np.clip(
            weights.sum(axis=1), 1.0, None
        )
        vectors /= np.clip(
            np.linalg.norm(vectors, axis=1, keepdims=True), 1e-12, None
        )
        if vectors.shape != (len(normalized), 384) or not np.isfinite(vectors).all():
            raise RuntimeError("embedding worker returned invalid vectors")
        return {
            "model": E5_MODEL_ID,
            "revision": E5_MODEL_REVISION,
            "dimensions": 384,
            "vectors": vectors.tolist(),
        }

    def verify_embedder(self) -> dict[str, Any]:
        corpus = [
            "Project Atlas is implemented in Rust and uses Cargo.",
            "The dentist appointment is Tuesday morning at nine.",
            "The preferred afternoon drink is green tea without sugar.",
            "The passport is stored in the locked bedroom drawer.",
            "The family dog is named Miso.",
            "Production deployments require a reviewed release tag.",
            "The next flight departs from terminal three.",
            "הקוד של פרויקט נובה כתוב בפייתון.",
        ]
        cases = [
            ("What programming language does Project Atlas use?", 0),
            ("When is my dentist appointment?", 1),
            ("באיזו שפה כתוב פרויקט נובה?", 7),
        ]
        passages = self.embed({"texts": corpus, "input_type": "passage"})["vectors"]
        queries = self.embed(
            {"texts": [query for query, _ in cases], "input_type": "query"}
        )["vectors"]

        import numpy as np

        passage_matrix = np.asarray(passages, dtype=np.float32)
        ranks: list[dict[str, Any]] = []
        for query_vector, (query, expected) in zip(queries, cases, strict=True):
            scores = passage_matrix @ np.asarray(query_vector, dtype=np.float32)
            top_three = np.argsort(-scores)[:3].tolist()
            if expected not in top_three:
                raise RuntimeError(
                    f"embedding recall fixture failed for {query!r}: "
                    f"expected {expected}, got {top_three}"
                )
            ranks.append(
                {"query": query, "expected": expected, "top_three": top_three}
            )
        return {"ok": True, "model": E5_MODEL_ID, "cases": ranks}

    def status(self) -> dict[str, Any]:
        moonshine_marker = self.root / "moonshine.json"
        faster_whisper_marker = self.root / "faster-whisper.json"
        qwen_custom = self.models / "qwen3-tts-0.6b-customvoice"
        qwen_base = self.models / "qwen3-tts-0.6b-base"
        kokoro_root = self.models / "kokoro-v1.0-int8"
        smart_turn = self.models / "smart-turn-v3.2-cpu.onnx"
        embed_model, embed_tokenizer = self._embed_paths()
        custom_complete = (
            (qwen_custom / "config.json").is_file()
            and (qwen_custom / "model.safetensors").is_file()
            and (qwen_custom / "speech_tokenizer/model.safetensors").is_file()
        )
        base_complete = (
            (qwen_base / "config.json").is_file()
            and (qwen_base / "model.safetensors").is_file()
            and (qwen_base / "speech_tokenizer/model.safetensors").is_file()
        )
        return {
            "moonshine_ready": moonshine_marker.is_file(),
            "faster_whisper_ready": faster_whisper_marker.is_file(),
            "qwen_ready": custom_complete or base_complete,
            "qwen_custom_voice_ready": custom_complete,
            "qwen_clone_ready": base_complete,
            "kokoro_ready": (
                (self.root / "kokoro.json").is_file()
                and (kokoro_root / "kokoro-v1.0.int8.onnx").is_file()
                and (kokoro_root / "voices-v1.0.bin").is_file()
            ),
            "smart_turn_ready": smart_turn.is_file(),
            "embed_ready": embed_model.is_file() and embed_tokenizer.is_file(),
            "moonshine_loaded": self.moonshine is not None,
            "moonshine_thread_mode": self.moonshine_thread_mode or "unloaded",
            "performance_cpu_tier": self.performance_cpu_tier,
            "qwen_loaded": self.qwen is not None,
            "kokoro_loaded": self.kokoro is not None,
            "faster_whisper_loaded": self.faster_whisper is not None,
            "active_stt_engine": self.stt_engine or "idle",
            "vision_grounding_loaded": self.vision_grounding is not None,
            "embed_loaded": self.embed_session is not None,
            "wake_ready": all(path.is_file() for path in self._wake_paths()),
            "wake_loaded": self.wake_model is not None,
            "wake_active": self.wake_active,
            "silero_vad_ready": self._silero_vad_path().is_file(),
            "silero_vad_loaded": self.silero_vad is not None,
            "silero_vad_version": SILERO_VAD_VERSION,
            "silero_vad_revision": SILERO_VAD_REVISION,
        }

    def unload(self, request: dict[str, Any]) -> dict[str, Any]:
        model = str(request.get("model", "")).strip()
        attributes = {
            "qwen3-tts": "qwen",
            "faster-whisper-large-v3-turbo-int8": "faster_whisper",
            "vision-grounding": "vision_grounding",
        }
        attribute = attributes.get(model)
        if attribute is None:
            raise RuntimeError(f"unknown or CPU-only model cannot be unloaded: {model}")

        resident = getattr(self, attribute)
        setattr(self, attribute, None)
        if model == "qwen3-tts":
            self.qwen_kind = ""
        was_loaded = resident is not None
        del resident
        gc.collect()
        # A GPU-backed model necessarily imported torch while loading. Avoid
        # importing it merely to acknowledge an idempotent unload on a fresh
        # worker, which keeps this protocol command asset-independent.
        torch = sys.modules.get("torch")
        if torch is not None and torch.cuda.is_available():
            torch.cuda.empty_cache()
        return {"model": model, "unloaded": was_loaded, "loaded": False}

    def stt_start(self, request: dict[str, Any]) -> dict[str, Any]:
        self._load_smart_turn()
        engine = str(request.get("stt_engine", "moonshine")).strip()
        if engine not in {"moonshine", "faster-whisper"}:
            raise RuntimeError(f"unsupported neural STT engine: {engine}")
        vocabulary = [str(item) for item in request.get("vocabulary", [])]
        language = str(request.get("language", "en")).strip() or "en"
        if len(vocabulary) > 128 or any(len(item) > 256 for item in vocabulary):
            raise RuntimeError("STT vocabulary exceeds the worker limits")
        if len(language) > 16:
            raise RuntimeError("STT language identifier is too long")
        if self.stream is not None:
            self._finish_moonshine_stream(cancel=True)
        if engine == "moonshine":
            self._load_moonshine(vocabulary, streaming=True)
        else:
            self._load_faster_whisper()
        self._load_silero_vad()
        self._reset_stt_session()
        self.stt_engine = engine
        self.stt_language = language
        self.stt_vocabulary = vocabulary
        self.turn_audio = []
        self._reset_silero_vad()
        if engine == "moonshine":
            # `Stream.add_audio` runs inference on a load-adaptive cadence (at
            # least 450 ms of audio, widened by the previous pass cost). A
            # bounded single-owner queue keeps its synchronous decode off the
            # 20 ms protocol-ingest path without reordering or dropping audio.
            self._start_moonshine_stream()
        result = {
            "state": "listening",
            "text": "",
            "engine": engine,
        }
        if engine == "moonshine":
            result["model"] = "medium-streaming"
            result["moonshine_thread_mode"] = self.moonshine_thread_mode
        else:
            result.update(
                {
                    "model": "large-v3-turbo",
                    "compute_type": FASTER_WHISPER_COMPUTE_TYPE,
                    "window_seconds": 3.0,
                    "overlap_seconds": 0.5,
                }
            )
        return result

    def wake_start(self, request: dict[str, Any]) -> dict[str, Any]:
        phrases = request.get("phrases")
        if not isinstance(phrases, list) or not phrases or len(phrases) > 16:
            raise RuntimeError("wake_start expects between one and 16 phrases")
        normalized = [normalized_phrase(item) for item in phrases]
        if any(not phrase or len(phrase) > 128 for phrase in normalized):
            raise RuntimeError("wake phrases must be non-empty and at most 128 characters")
        threshold_milli = request.get("threshold_milli", 500)
        if not isinstance(threshold_milli, int) or not 0 <= threshold_milli <= 1_000:
            raise RuntimeError("wake threshold must be between 0 and 1000")

        self.wake_phrases = normalized
        self.wake_threshold = threshold_milli / 1_000
        self.wake_pending_samples = []
        self.wake_fallback = any(
            phrase not in BUILTIN_WAKE_PHRASES for phrase in normalized
        )
        self.wake_active = True
        if self.wake_fallback:
            # The frontend's digital-silence pre-gate means the VAD session can
            # stay unloaded until the first non-trivial custom-phrase frame.
            self._reset_silero_vad()
            return {
                "state": "listening",
                "fallback": "stt-match",
                "phrases": self.wake_phrases,
            }
        try:
            self._load_wake_model()
        except Exception:
            self.wake_active = False
            raise
        return {
            "state": "listening",
            "engine": "openwakeword-onnx",
            "model": "hey_jarvis_v0.1",
            "phrases": self.wake_phrases,
        }

    def wake_chunk(self, request: dict[str, Any]) -> dict[str, Any]:
        if not self.wake_active:
            raise RuntimeError("no wake stream is active")
        samples = request.get("samples", [])
        if not isinstance(samples, list) or not samples or len(samples) > 32_000:
            raise RuntimeError("invalid wake chunk")
        if int(request.get("sample_rate_hz", 16_000)) != 16_000:
            raise RuntimeError("wake chunks must be 16 kHz mono audio")
        if self.wake_fallback:
            speech_prob, vad_frames = self._speech_probability(
                samples, int(request.get("sample_rate_hz", 16_000))
            )
            return {
                "wake": False,
                "score": 0.0,
                "fallback": "stt-match",
                "speech_prob": speech_prob,
                "vad_frames": vad_frames,
                "vad_model": f"silero-vad-{SILERO_VAD_VERSION}",
            }
        if self.wake_model is None:
            raise RuntimeError("the openWakeWord model is not loaded")

        import numpy as np

        audio = np.asarray(self.wake_pending_samples + samples, dtype=np.float32)
        audio = np.nan_to_num(audio, nan=0.0, posinf=1.0, neginf=-1.0)
        audio = np.clip(audio, -1.0, 1.0)
        complete_samples = (audio.size // 1_280) * 1_280
        self.wake_pending_samples = audio[complete_samples:].tolist()
        pcm = (audio[:complete_samples] * 32_767).astype(np.int16)
        score = 0.0
        for offset in range(0, complete_samples, 1_280):
            predictions = self.wake_model.predict(pcm[offset : offset + 1_280])
            score = max(
                score,
                max((float(value) for value in predictions.values()), default=0.0),
            )
        return {"wake": score >= self.wake_threshold, "score": score}

    def wake_stop(self, _request: dict[str, Any]) -> dict[str, Any]:
        was_active = self.wake_active
        self.wake_active = False
        self.wake_fallback = False
        self.wake_phrases = []
        self.wake_pending_samples = []
        self._reset_silero_vad()
        if self.wake_model is not None:
            self.wake_model.reset()
        return {"state": "idle", "stopped": was_active}

    def stt_chunk(self, request: dict[str, Any]) -> dict[str, Any]:
        if self.stt_engine not in {"moonshine", "faster-whisper"}:
            raise RuntimeError("no neural STT stream is active")
        if self.stt_engine == "moonshine" and self.stream is None:
            raise RuntimeError("no Moonshine stream is active")
        samples = request.get("samples", [])
        if not isinstance(samples, list) or not samples or len(samples) > 32_000:
            raise RuntimeError("invalid speech chunk")
        sample_rate_hz = int(request.get("sample_rate_hz", 16_000))
        if sample_rate_hz != 16_000:
            raise RuntimeError("neural STT chunks must be 16 kHz mono audio")
        speech_prob, vad_frames = self._speech_probability(samples, sample_rate_hz)
        self.turn_audio.extend(float(sample) for sample in samples)
        if len(self.turn_audio) > 16_000 * 8:
            self.turn_audio = self.turn_audio[-16_000 * 8 :]
        if self.stt_engine == "moonshine":
            partial_text, partial_audio_samples = self._enqueue_moonshine_audio(
                samples, sample_rate_hz
            )
        else:
            if len(self.stt_audio) + len(samples) > STT_MAX_SAMPLES:
                raise RuntimeError("accurate STT capture exceeds the ten-minute limit")
            self.stt_audio.extend(float(sample) for sample in samples)
            if len(self.stt_audio) >= self.next_partial_samples:
                start = max(0, len(self.stt_audio) - FASTER_WHISPER_WINDOW_SAMPLES)
                text = self._faster_whisper_decode(self.stt_audio[start:], final=False)
                if text:
                    self.partial_text = text
                    self.partial_audio_samples = len(self.stt_audio)
                self.next_partial_samples += FASTER_WHISPER_HOP_SAMPLES
            partial_text = self.partial_text
            partial_audio_samples = self.partial_audio_samples
        result = {
            "state": "listening",
            "text": partial_text,
            "final_result": False,
            "engine": self.stt_engine,
            "partial_audio_samples": partial_audio_samples,
            "speech_prob": speech_prob,
            "vad_frames": vad_frames,
            "vad_model": f"silero-vad-{SILERO_VAD_VERSION}",
        }
        if self.stt_engine == "faster-whisper":
            result.update(
                {
                    "model": "large-v3-turbo",
                    "compute_type": FASTER_WHISPER_COMPUTE_TYPE,
                    "window_seconds": 3.0,
                    "overlap_seconds": 0.5,
                }
            )
        return result

    def stt_stop(self, _request: dict[str, Any]) -> dict[str, Any]:
        engine = self.stt_engine
        language = self.stt_language
        if engine == "moonshine" and self.stream is not None:
            result = self._finish_moonshine_stream(cancel=False)
            with self.moonshine_state_lock:
                partial_text = self.partial_text
            text = transcript_text(result) or partial_text
        elif engine == "faster-whisper" and self.faster_whisper is not None:
            text = self._faster_whisper_decode(self.stt_audio, final=True)
        else:
            raise RuntimeError("no neural STT stream is active")
        self._reset_stt_session()
        self.turn_audio = []
        self._reset_silero_vad()
        if not text:
            raise RuntimeError("no speech was detected")
        response = {
            "state": "idle",
            "text": text,
            "final_result": True,
            "language": language,
            "engine": engine,
            "model": "large-v3-turbo" if engine == "faster-whisper" else "medium-streaming",
        }
        if engine == "faster-whisper":
            response["compute_type"] = FASTER_WHISPER_COMPUTE_TYPE
        else:
            response["moonshine_thread_mode"] = self.moonshine_thread_mode
        return response

    def stt_cancel(self, _request: dict[str, Any]) -> dict[str, Any]:
        if self.stream is not None:
            self._finish_moonshine_stream(cancel=True)
        engine = self.stt_engine
        self._reset_stt_session()
        self.turn_audio = []
        self._reset_silero_vad()
        return {"state": "idle", "cancelled": True, "engine": engine or "none"}

    def turn_complete(self, request: dict[str, Any]) -> dict[str, Any]:
        if not self._load_smart_turn():
            return {"complete": True, "decision": "silence-fallback"}
        result = self.smart_turn.predict(
            self.turn_audio,
            threshold=float(request.get("threshold", 0.5)),
        )
        result["decision"] = "smart-turn"
        return result

    def stt_transcribe(self, request: dict[str, Any]) -> dict[str, Any]:
        engine = str(request.get("stt_engine", "moonshine")).strip()
        if engine not in {"moonshine", "faster-whisper"}:
            raise RuntimeError(f"unsupported neural STT engine: {engine}")
        vocabulary = [str(item) for item in request.get("vocabulary", [])]
        language = str(request.get("language", "en")).strip() or "en"
        wav = Path(str(request.get("wav", "")))
        if not wav.is_file() or not wav.is_relative_to(self.root.parent):
            raise RuntimeError("transcription input is outside the private voice directory")
        if engine == "moonshine":
            from moonshine_voice import load_wav_file

            self._load_moonshine(vocabulary, streaming=False)
            audio, sample_rate = load_wav_file(wav)
            result = self.moonshine.transcribe_without_streaming(audio, sample_rate)
            text = transcript_text(result)
        else:
            self._load_faster_whisper()
            previous_language = self.stt_language
            previous_vocabulary = self.stt_vocabulary
            self.stt_language = language
            self.stt_vocabulary = vocabulary
            try:
                text = self._faster_whisper_decode(str(wav), final=True)
            finally:
                self.stt_language = previous_language
                self.stt_vocabulary = previous_vocabulary
        if not text:
            raise RuntimeError("no speech was detected")
        response = {
            "text": text,
            "final_result": True,
            "language": language,
            "engine": engine,
            "model": "large-v3-turbo" if engine == "faster-whisper" else "medium-streaming",
        }
        if engine == "moonshine":
            response["moonshine_thread_mode"] = self.moonshine_thread_mode
        else:
            response["compute_type"] = FASTER_WHISPER_COMPUTE_TYPE
        return response

    def tts_synthesize(self, request: dict[str, Any]) -> dict[str, Any]:
        import soundfile as sf

        text = str(request.get("text", "")).strip()
        output = Path(str(request.get("output", "")))
        if not text or len(text) > 65_536:
            raise RuntimeError("invalid speech text")
        if not output.is_relative_to(self.root.parent):
            raise RuntimeError("speech output is outside the private voice directory")
        output.parent.mkdir(parents=True, exist_ok=True)
        started = time.monotonic()
        engine = str(request.get("tts_engine", "qwen3-tts")).strip()
        if engine == "kokoro":
            waveform, sample_rate, voice = self._kokoro_create(text, request)
            sf.write(output, waveform, sample_rate)
            return {
                "wav": str(output),
                "sample_rate_hz": sample_rate,
                "synthesis_ms": int((time.monotonic() - started) * 1000),
                "engine": "kokoro",
                "model_kind": "int8",
                "voice": voice,
            }
        if engine != "qwen3-tts":
            raise RuntimeError(f"unsupported neural TTS engine: {engine}")
        requested = str(request.get("model_kind", "custom"))
        kind = self._load_qwen(requested)
        if kind == "base":
            reference = Path(str(request.get("reference_audio", "")))
            reference_text = str(request.get("reference_text", "")).strip()
            if not reference.is_file():
                raise RuntimeError("Qwen voice cloning requires a local reference recording")
            wavs, sample_rate = self.qwen.generate_voice_clone(
                text=text,
                language="English",
                ref_audio=str(reference),
                ref_text=reference_text or None,
                x_vector_only_mode=not bool(reference_text),
            )
        else:
            wavs, sample_rate = self.qwen.generate_custom_voice(
                text=text,
                language="English",
                speaker=str(request.get("voice", "Ryan")) or "Ryan",
            )
        sf.write(output, wavs[0], sample_rate)
        return {
            "wav": str(output),
            "sample_rate_hz": int(sample_rate),
            "synthesis_ms": int((time.monotonic() - started) * 1000),
            "engine": "qwen3-tts-0.6b",
            "model_kind": kind,
        }

    def _private_tts_socket(self, request: dict[str, Any]) -> Path:
        raw_path = str(request.get("socket_path", ""))
        socket_path = Path(raw_path)
        if not raw_path or not socket_path.is_absolute() or not socket_path.name:
            raise RuntimeError("tts_stream requires an absolute private socket path")
        private_root = self.root.parent.resolve(strict=True)
        socket_parent = socket_path.parent.resolve(strict=True)
        if not socket_parent.is_relative_to(private_root):
            raise RuntimeError("tts_stream socket is outside the private voice directory")
        try:
            mode = socket_path.lstat().st_mode
        except FileNotFoundError as error:
            raise RuntimeError("tts_stream socket is not ready") from error
        if not stat.S_ISSOCK(mode):
            raise RuntimeError("tts_stream path is not a Unix domain socket")
        return socket_path

    @staticmethod
    def _pcm16le(waveform: Any) -> bytes:
        import numpy as np

        audio = np.asarray(waveform, dtype=np.float32).reshape(-1)
        if audio.size == 0:
            raise RuntimeError("Qwen3-TTS returned an empty clause")
        audio = np.nan_to_num(audio, nan=0.0, posinf=1.0, neginf=-1.0)
        pcm = (np.clip(audio, -1.0, 1.0) * 32_767.0).round().astype("<i2")
        encoded = pcm.tobytes()
        if len(encoded) > TTS_STREAM_MAX_FRAME_BYTES:
            raise RuntimeError("Qwen3-TTS clause exceeds the stream frame limit")
        return encoded

    def tts_stream(self, request: dict[str, Any]) -> dict[str, Any]:
        text = str(request.get("text", "")).strip()
        if not text or len(text) > 65_536:
            raise RuntimeError("invalid speech text")
        generation = request.get("generation")
        if not isinstance(generation, int) or not 0 <= generation <= (2**64 - 1):
            raise RuntimeError("tts_stream requires a valid generation")
        clauses = tts_clauses(text)
        if not clauses:
            raise RuntimeError("tts_stream found no speakable clauses")

        socket_path = self._private_tts_socket(request)
        engine = str(request.get("tts_engine", "qwen3-tts")).strip()
        if engine not in {"qwen3-tts", "kokoro"}:
            raise RuntimeError(f"unsupported neural TTS engine: {engine}")
        requested = str(request.get("model_kind", "custom"))
        voice = str(request.get("voice", "Ryan")) or "Ryan"
        started = time.monotonic()
        sample_rate_hz: int | None = None
        frames = 0
        samples = 0
        cancelled = False
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
            stream.connect(str(socket_path))
            # Connect before loading so model/config failures close the audio
            # channel and let Rust consume the JSON error without waiting for
            # the socket-accept deadline.
            kind = "int8"
            if engine == "qwen3-tts":
                kind = self._load_qwen(requested)
            else:
                self._load_kokoro()
            for clause in clauses:
                if engine == "kokoro":
                    waveform, clause_sample_rate, voice = self._kokoro_create(
                        clause, request
                    )
                    wavs = [waveform]
                elif kind == "base":
                    reference = Path(str(request.get("reference_audio", "")))
                    reference_text = str(request.get("reference_text", "")).strip()
                    if not reference.is_file():
                        raise RuntimeError(
                            "Qwen voice cloning requires a local reference recording"
                        )
                    wavs, clause_sample_rate = self.qwen.generate_voice_clone(
                        text=clause,
                        language="English",
                        ref_audio=str(reference),
                        ref_text=reference_text or None,
                        x_vector_only_mode=not bool(reference_text),
                    )
                else:
                    wavs, clause_sample_rate = self.qwen.generate_custom_voice(
                        text=clause,
                        language="English",
                        speaker=voice,
                    )
                clause_sample_rate = int(clause_sample_rate)
                if sample_rate_hz is None:
                    sample_rate_hz = clause_sample_rate
                elif sample_rate_hz != clause_sample_rate:
                    raise RuntimeError(f"{engine} changed sample rate between clauses")
                encoded = self._pcm16le(wavs[0])
                try:
                    stream.sendall(struct.pack("<I", len(encoded)) + encoded)
                except (BrokenPipeError, ConnectionResetError):
                    cancelled = True
                    break
                frames += 1
                samples += len(encoded) // 2
        return {
            "generation": generation,
            "sample_rate_hz": sample_rate_hz or 24_000,
            "clause_count": len(clauses),
            "frames": frames,
            "samples": samples,
            "cancelled": cancelled,
            "synthesis_ms": int((time.monotonic() - started) * 1000),
            "engine": "kokoro" if engine == "kokoro" else "qwen3-tts-0.6b",
            "model_kind": kind,
            "voice": voice,
        }

    def dispatch(self, request: dict[str, Any]) -> dict[str, Any]:
        command = str(request.get("command", ""))
        handlers = {
            "status": self.status,
            "unload": lambda: self.unload(request),
            "stt_start": lambda: self.stt_start(request),
            "stt_chunk": lambda: self.stt_chunk(request),
            "stt_stop": lambda: self.stt_stop(request),
            "stt_cancel": lambda: self.stt_cancel(request),
            "wake_start": lambda: self.wake_start(request),
            "wake_chunk": lambda: self.wake_chunk(request),
            "wake_stop": lambda: self.wake_stop(request),
            "turn_complete": lambda: self.turn_complete(request),
            "stt_transcribe": lambda: self.stt_transcribe(request),
            "tts_synthesize": lambda: self.tts_synthesize(request),
            "tts_stream": lambda: self.tts_stream(request),
            "embed": lambda: self.embed(request),
        }
        handler = handlers.get(command)
        if handler is None:
            raise RuntimeError(f"unknown voice command: {command}")
        return handler()


def respond(payload: dict[str, Any]) -> None:
    PROTOCOL_STDOUT.write(json.dumps(payload, separators=(",", ":")) + "\n")
    PROTOCOL_STDOUT.flush()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    parser.add_argument("--verify-embedder", action="store_true")
    args = parser.parse_args()
    runtime = VoiceRuntime(Path(args.root).resolve())
    if args.verify_embedder:
        with contextlib.redirect_stdout(sys.stderr):
            result = runtime.verify_embedder()
        respond(result)
        return 0
    respond({"ready": True, "protocol": 1})
    for raw in sys.stdin:
        request_id: Any = None
        try:
            request = json.loads(raw)
            request_id = request.get("id")
            with contextlib.redirect_stdout(sys.stderr):
                result = runtime.dispatch(request)
            respond({"id": request_id, "ok": True, "result": result})
        except Exception as error:
            traceback.print_exc(file=sys.stderr)
            respond({"id": request_id, "ok": False, "error": str(error)})
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
