#!/usr/bin/env python3
"""Run deterministic latency probes and optionally write the evidence report."""

from __future__ import annotations

import argparse
import json
import subprocess
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
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    result = subprocess.run(
        ["cargo", "run", "-p", "personal-agent-audio", "--bin", "audio-benchmark", "--quiet"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    )
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
    if args.write:
        output = ROOT / "docs/operations/performance-report.json"
        output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(
        "verified deterministic replay performance distributions; "
        "replay is not a physical microphone, speaker, network, screen-capture, "
        "or UI-startup measurement; physical-device metrics remain externally gated"
    )


if __name__ == "__main__":
    main()
