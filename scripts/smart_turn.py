"""Minimal Smart Turn v3.2 CPU inference used by the local voice worker.

Feature extraction is adapted from Pipecat's BSD-2-Clause implementation:
https://github.com/pipecat-ai/pipecat/tree/main/src/pipecat/audio/turn/smart_turn
Copyright (c) 2024-2026, Daily.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import numpy as np
from numpy.lib.stride_tricks import sliding_window_view
import onnxruntime as ort


_N_FFT = 400
_HOP_LENGTH = 160
_N_MELS = 80
_SAMPLE_RATE = 16_000
_SAMPLES = _SAMPLE_RATE * 8


def _hertz_to_mel(freq: np.ndarray) -> np.ndarray:
    freq = np.atleast_1d(np.asarray(freq, dtype=np.float64))
    mels = 3.0 * freq / 200.0
    log_region = freq >= 1000.0
    mels[log_region] = 15.0 + np.log(freq[log_region] / 1000.0) * (27.0 / np.log(6.4))
    return mels


def _mel_to_hertz(mels: np.ndarray) -> np.ndarray:
    mels = np.atleast_1d(np.asarray(mels, dtype=np.float64))
    freq = 200.0 * mels / 3.0
    log_region = mels >= 15.0
    freq[log_region] = 1000.0 * np.exp((np.log(6.4) / 27.0) * (mels[log_region] - 15.0))
    return freq


def _mel_filterbank() -> np.ndarray:
    mel_freqs = np.linspace(
        float(_hertz_to_mel(np.array([0.0]))[0]),
        float(_hertz_to_mel(np.array([_SAMPLE_RATE / 2]))[0]),
        _N_MELS + 2,
    )
    filter_freqs = _mel_to_hertz(mel_freqs)
    fft_freqs = np.linspace(0, _SAMPLE_RATE // 2, _N_FFT // 2 + 1)
    diff = np.diff(filter_freqs)
    slopes = np.expand_dims(filter_freqs, 0) - np.expand_dims(fft_freqs, 1)
    filters = np.maximum(
        np.zeros(1),
        np.minimum(-slopes[:, :-2] / diff[:-1], slopes[:, 2:] / diff[1:]),
    )
    filters *= np.expand_dims(2.0 / (filter_freqs[2:] - filter_freqs[:-2]), 0)
    return filters


_HANN = np.hanning(_N_FFT + 1)[:-1]
_MEL_FILTERS = _mel_filterbank()


def _features(audio: np.ndarray) -> np.ndarray:
    x = np.asarray(audio, dtype=np.float32).reshape(-1)
    if x.size > _SAMPLES:
        x = x[-_SAMPLES:]
    elif x.size < _SAMPLES:
        # Smart Turn expects the most recent speech at the end of its window.
        x = np.pad(x, (_SAMPLES - x.size, 0), mode="constant")
    x = (x - x.mean()) / np.sqrt(x.var() + 1e-7)
    padded = np.pad(x.astype(np.float64), (_N_FFT // 2, _N_FFT // 2), mode="reflect")
    windows = sliding_window_view(padded, _N_FFT)[::_HOP_LENGTH]
    spectrum = np.fft.rfft(windows * _HANN.astype(np.float64), axis=-1)
    powers = (np.abs(spectrum) ** 2).T
    mel = np.maximum(1e-10, _MEL_FILTERS.T @ powers)
    logged = np.log10(mel)[:, :-1]
    logged = np.maximum(logged, logged.max() - 8.0)
    return ((logged + 4.0) / 4.0).astype(np.float32)


class SmartTurn:
    def __init__(self, model_path: str | Path, cpu_count: int = 2) -> None:
        model = Path(model_path)
        if not model.is_file():
            raise RuntimeError("Smart Turn v3.2 model is not installed")
        options = ort.SessionOptions()
        options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
        options.inter_op_num_threads = 1
        options.intra_op_num_threads = max(1, min(cpu_count, 4))
        options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        self.session = ort.InferenceSession(str(model), sess_options=options, providers=["CPUExecutionProvider"])

    def predict(self, samples: list[float], threshold: float = 0.5) -> dict[str, Any]:
        if len(samples) < 1_600:
            return {"complete": False, "probability": 0.0, "reason": "insufficient_audio"}
        input_features = np.expand_dims(_features(np.asarray(samples, dtype=np.float32)), axis=0)
        outputs = self.session.run(None, {"input_features": input_features})
        probability = float(np.asarray(outputs[0])[0].item())
        return {
            "complete": probability >= max(0.0, min(float(threshold), 1.0)),
            "probability": probability,
            "reason": "smart_turn_v3.2",
        }
