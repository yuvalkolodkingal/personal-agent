#!/usr/bin/env python3
"""Persistent, local neural speech worker for Personal Agent.

The desktop host owns this process and communicates with one JSON object per
line.  Protocol output is kept on the original stdout; noisy model-library
output is redirected to stderr so it can never corrupt an IPC response.
"""

from __future__ import annotations

import argparse
import contextlib
import gc
import hashlib
import json
import os
from pathlib import Path
import re
import sys
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


class VoiceRuntime:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.models = root / "models"
        self.moonshine = None
        self.moonshine_thread_mode = ""
        self.stream = None
        self.partial_text = ""
        self.qwen = None
        self.qwen_kind = ""
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
        qwen_custom = self.models / "qwen3-tts-0.6b-customvoice"
        qwen_base = self.models / "qwen3-tts-0.6b-base"
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
            "qwen_ready": custom_complete or base_complete,
            "qwen_custom_voice_ready": custom_complete,
            "qwen_clone_ready": base_complete,
            "smart_turn_ready": smart_turn.is_file(),
            "embed_ready": embed_model.is_file() and embed_tokenizer.is_file(),
            "moonshine_loaded": self.moonshine is not None,
            "moonshine_thread_mode": self.moonshine_thread_mode or "unloaded",
            "qwen_loaded": self.qwen is not None,
            "faster_whisper_loaded": self.faster_whisper is not None,
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
        self._load_moonshine(
            [str(item) for item in request.get("vocabulary", [])], streaming=True
        )
        self._load_silero_vad()
        if self.stream is not None:
            try:
                self.stream.stop()
            except Exception:
                pass
        self.partial_text = ""
        self.turn_audio = []
        self._reset_silero_vad()
        self.stream = self.moonshine.create_stream(update_interval=0.45)
        self.stream.start()
        return {
            "state": "listening",
            "text": "",
            "moonshine_thread_mode": self.moonshine_thread_mode,
        }

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
        if self.stream is None:
            raise RuntimeError("no Moonshine stream is active")
        samples = request.get("samples", [])
        if not isinstance(samples, list) or not samples or len(samples) > 32_000:
            raise RuntimeError("invalid speech chunk")
        sample_rate_hz = int(request.get("sample_rate_hz", 16_000))
        speech_prob, vad_frames = self._speech_probability(samples, sample_rate_hz)
        self.stream.add_audio(samples, sample_rate_hz)
        self.turn_audio.extend(float(sample) for sample in samples)
        if len(self.turn_audio) > 16_000 * 8:
            self.turn_audio = self.turn_audio[-16_000 * 8 :]
        result = self.stream.update_transcription()
        text = transcript_text(result)
        if text:
            self.partial_text = text
        return {
            "state": "listening",
            "text": self.partial_text,
            "final_result": False,
            "speech_prob": speech_prob,
            "vad_frames": vad_frames,
            "vad_model": f"silero-vad-{SILERO_VAD_VERSION}",
        }

    def stt_stop(self, _request: dict[str, Any]) -> dict[str, Any]:
        if self.stream is None:
            raise RuntimeError("no Moonshine stream is active")
        stream = self.stream
        self.stream = None
        result = stream.stop()
        text = transcript_text(result) or self.partial_text
        self.partial_text = ""
        self.turn_audio = []
        self._reset_silero_vad()
        if not text:
            raise RuntimeError("no speech was detected")
        return {
            "state": "idle",
            "text": text,
            "final_result": True,
            "language": "en",
        }

    def stt_cancel(self, _request: dict[str, Any]) -> dict[str, Any]:
        if self.stream is not None:
            try:
                self.stream.stop()
            finally:
                self.stream = None
        self.partial_text = ""
        self.turn_audio = []
        self._reset_silero_vad()
        return {"state": "idle", "cancelled": True}

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
        from moonshine_voice import load_wav_file

        self._load_moonshine(
            [str(item) for item in request.get("vocabulary", [])], streaming=False
        )
        wav = Path(str(request.get("wav", "")))
        if not wav.is_file() or not wav.is_relative_to(self.root.parent):
            raise RuntimeError("transcription input is outside the private voice directory")
        audio, sample_rate = load_wav_file(wav)
        result = self.moonshine.transcribe_without_streaming(audio, sample_rate)
        text = transcript_text(result)
        if not text:
            raise RuntimeError("no speech was detected")
        return {
            "text": text,
            "final_result": True,
            "language": "en",
            "moonshine_thread_mode": self.moonshine_thread_mode,
        }

    def tts_synthesize(self, request: dict[str, Any]) -> dict[str, Any]:
        import soundfile as sf

        text = str(request.get("text", "")).strip()
        output = Path(str(request.get("output", "")))
        if not text or len(text) > 65_536:
            raise RuntimeError("invalid speech text")
        if not output.is_relative_to(self.root.parent):
            raise RuntimeError("speech output is outside the private voice directory")
        output.parent.mkdir(parents=True, exist_ok=True)
        requested = str(request.get("model_kind", "custom"))
        kind = self._load_qwen(requested)
        started = time.monotonic()
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
