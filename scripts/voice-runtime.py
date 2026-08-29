#!/usr/bin/env python3
"""Persistent, local neural speech worker for Personal Agent.

The desktop host owns this process and communicates with one JSON object per
line.  Protocol output is kept on the original stdout; noisy model-library
output is redirected to stderr so it can never corrupt an IPC response.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
from pathlib import Path
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
        self.stream = None
        self.partial_text = ""
        self.qwen = None
        self.qwen_kind = ""
        self.smart_turn = None
        self.turn_audio: list[float] = []
        self.embed_session = None
        self.embed_tokenizer = None

    def _moonshine_marker(self) -> dict[str, Any]:
        marker = self.root / "moonshine.json"
        if not marker.is_file():
            raise RuntimeError("Moonshine Medium Streaming is not installed")
        return json.loads(marker.read_text(encoding="utf-8"))

    def _load_moonshine(self, vocabulary: list[str] | None = None) -> None:
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
        if vocabulary and hasattr(self.moonshine, "set_keyterms"):
            self.moonshine.set_keyterms(vocabulary[:128])

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
            "qwen_loaded": self.qwen is not None,
            "embed_loaded": self.embed_session is not None,
        }

    def stt_start(self, request: dict[str, Any]) -> dict[str, Any]:
        self._load_moonshine([str(item) for item in request.get("vocabulary", [])])
        if self.stream is not None:
            try:
                self.stream.stop()
            except Exception:
                pass
        self.partial_text = ""
        self.turn_audio = []
        self.stream = self.moonshine.create_stream(update_interval=0.45)
        self.stream.start()
        return {"state": "listening", "text": ""}

    def stt_chunk(self, request: dict[str, Any]) -> dict[str, Any]:
        if self.stream is None:
            raise RuntimeError("no Moonshine stream is active")
        samples = request.get("samples", [])
        if not isinstance(samples, list) or len(samples) > 32_000:
            raise RuntimeError("invalid speech chunk")
        self.stream.add_audio(samples, int(request.get("sample_rate_hz", 16_000)))
        self.turn_audio.extend(float(sample) for sample in samples)
        if len(self.turn_audio) > 16_000 * 8:
            self.turn_audio = self.turn_audio[-16_000 * 8 :]
        result = self.stream.update_transcription()
        text = transcript_text(result)
        if text:
            self.partial_text = text
        return {"state": "listening", "text": self.partial_text, "final_result": False}

    def stt_stop(self, _request: dict[str, Any]) -> dict[str, Any]:
        if self.stream is None:
            raise RuntimeError("no Moonshine stream is active")
        stream = self.stream
        self.stream = None
        result = stream.stop()
        text = transcript_text(result) or self.partial_text
        self.partial_text = ""
        self.turn_audio = []
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
        return {"state": "idle", "cancelled": True}

    def turn_complete(self, request: dict[str, Any]) -> dict[str, Any]:
        if self.smart_turn is None:
            from smart_turn import SmartTurn

            self.smart_turn = SmartTurn(self.models / "smart-turn-v3.2-cpu.onnx")
        return self.smart_turn.predict(
            self.turn_audio,
            threshold=float(request.get("threshold", 0.5)),
        )

    def stt_transcribe(self, request: dict[str, Any]) -> dict[str, Any]:
        from moonshine_voice import load_wav_file

        self._load_moonshine([str(item) for item in request.get("vocabulary", [])])
        wav = Path(str(request.get("wav", "")))
        if not wav.is_file() or not wav.is_relative_to(self.root.parent):
            raise RuntimeError("transcription input is outside the private voice directory")
        audio, sample_rate = load_wav_file(wav)
        result = self.moonshine.transcribe_without_streaming(audio, sample_rate)
        text = transcript_text(result)
        if not text:
            raise RuntimeError("no speech was detected")
        return {"text": text, "final_result": True, "language": "en"}

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
            "stt_start": lambda: self.stt_start(request),
            "stt_chunk": lambda: self.stt_chunk(request),
            "stt_stop": lambda: self.stt_stop(request),
            "stt_cancel": lambda: self.stt_cancel(request),
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
