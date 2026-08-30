#!/usr/bin/env python3
"""Run deterministic latency probes and optionally write the evidence report."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LIMITS_US = {
    "hotkey_to_listening": 100_000,
    "wake_detection_to_listening": 250_000,
    "internal_speaker_stop": 50_000,
    "offline_deterministic_command": 500_000,
    "startup_native_setup": 800_000,
    "bootstrap_ipc": 250_000,
    "desktop_snapshot_warm": 150_000,
    "tts_first_audio_ms": 700_000,
    # Warmup pre-synthesizes the acknowledgement phrases, so a cached ack must
    # reach the playback queue well inside the SPEC-V2 TTS-6 250 ms budget.
    "tts_ack_first_audio_ms": 250_000,
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    result = subprocess.run(
        ["cargo", "run", "-p", "personal-agent-audio", "--bin", "audio-benchmark", "--quiet"],
        cwd=ROOT, check=False, capture_output=True, text=True,
    )
    if result.returncode != 0:
        sys.stdout.write(result.stdout)
        sys.stderr.write(result.stderr)
        raise SystemExit(result.returncode)
    report = json.loads(result.stdout)
    for name, limit in LIMITS_US.items():
        metric = report[name]
        assert metric["sample_count"] >= 100, name
        assert metric["p95_microseconds"] < limit, f"{name} p95 exceeded {limit}us"
        assert metric["maximum_microseconds"] < limit, f"{name} maximum exceeded {limit}us"
    endpoint = report["stt_endpoint_replay"]
    if endpoint["status"] == "measured":
        metric = endpoint["endpoint_decision"]
        assert metric["sample_count"] >= 5, "stt_endpoint_replay"
        assert metric["p95_microseconds"] < 250_000, "STT endpoint p95 exceeded 250ms"
        assert metric["maximum_microseconds"] < 250_000, "STT endpoint maximum exceeded 250ms"
        assert endpoint["smart_turn_consultations_per_silence"] == 1
        assert all(
            decision["decision"] == "smart-turn"
            for decision in endpoint["decisions"]
        ), "endpoint replay did not exercise Smart Turn"
    else:
        assert endpoint["status"] == "external-model-assets-required"
    moonshine_wer = report["stt_wer_moonshine"]
    accurate_wer = report["stt_wer_accurate"]
    if moonshine_wer["status"] == "measured":
        assert accurate_wer["status"] == "measured"
        assert moonshine_wer["sample_count"] == 10
        assert accurate_wer["sample_count"] == 10
        assert moonshine_wer["reference_words"] > 0
        assert accurate_wer["reference_words"] > 0
        assert moonshine_wer["wer"] >= 0
        assert accurate_wer["wer"] >= 0
        wer_evidence = (
            f"Moonshine WER {moonshine_wer['wer']:.2%}; "
            f"accurate WER {accurate_wer['wer']:.2%}"
        )
    else:
        assert moonshine_wer["status"] == "external-model-assets-required"
        assert accurate_wer["status"] == "external-model-assets-required"
        wer_evidence = "WER requires the pinned external STT model assets"
    # Print measured accuracy before the latency assertion so a hardware-bound
    # latency failure still preserves the real two-engine WER evidence.
    print(f"STT WER evidence: {wer_evidence}")
    partial_lag = report["stt_partial_lag_ms"]
    if partial_lag["status"] == "measured":
        assert partial_lag["sample_count"] >= 10, "stt_partial_lag_ms"
        observations = partial_lag["observations"]
        assert set(observations) == {"moonshine", "faster-whisper"}
        for engine, engine_observations in observations.items():
            assert len(engine_observations) == partial_lag["by_engine"][engine]["sample_count"]
            for observation in engine_observations:
                assert observation["decoder_audio_samples"] <= observation["observed_audio_samples"]
                decomposed_lag = (
                    observation["decoder_backlog_microseconds"]
                    + observation["post_latest_ingress_microseconds"]
                )
                assert abs(observation["lag_microseconds"] - decomposed_lag) <= 1
        slow_observations = {
            engine: [
                observation
                for observation in engine_observations
                if observation["lag_microseconds"] >= 700_000
            ]
            for engine, engine_observations in observations.items()
        }
        assert partial_lag["p95_microseconds"] < 700_000, (
            f"STT partial lag p95 {partial_lag['p95_microseconds']}us exceeded "
            f"700000us; by engine: {partial_lag['by_engine']}; "
            f"slow observations: {slow_observations}"
        )
    else:
        assert partial_lag["status"] == "external-model-assets-required"
    if args.write:
        output = ROOT / "docs/operations/performance-report.json"
        output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(
        f"verified deterministic replay performance distributions; {wer_evidence}; "
        "replay is not a physical microphone, speaker, network, screen-capture, "
        "or UI-startup measurement; physical-device metrics remain externally gated"
    )


if __name__ == "__main__":
    main()
