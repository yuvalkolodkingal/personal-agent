#!/usr/bin/env python3
"""Verify official pack manifests, connector safety, and capability coverage."""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load(path: Path):
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def main() -> None:
    capabilities = load(ROOT / "capabilities.yaml")["capabilities"]
    acceptance = load(ROOT / "acceptance-tests.yaml")["tests"]
    evaluation_rows = load(ROOT / "evals/official-packs.json")["evaluations"]
    evaluation_ids = {row["id"] for row in evaluation_rows}
    test_ids = {row["id"] for row in acceptance}
    known_capabilities = {row["id"] for row in capabilities}
    pack_paths = sorted((ROOT / "packs").glob("*/pack.json"))
    assert pack_paths, "no official packs found"
    covered_by_pack: set[str] = set()
    connector_ids: set[str] = set()
    for path in pack_paths:
        pack = load(path)
        assert pack["schema_version"] == 1, path
        assert pack["official"] and pack["publisher"] == "Personal Agent", path
        assert pack["install_disabled"], f"{path}: pack must install disabled"
        assert set(pack["capabilities"]) <= known_capabilities, path
        assert set(pack["evaluation_ids"]) <= evaluation_ids, path
        assert pack["evaluation_ids"], f"{path}: evaluation required"
        covered_by_pack.update(pack["capabilities"])
        for connector in pack["connectors"]:
            assert not connector["enabled_by_default"], connector["id"]
            assert connector["scopes"], connector["id"]
            assert all(alias.startswith("keychain://") for alias in connector["credential_aliases"])
            connector_ids.add(connector["id"])
    for capability in capabilities:
        assert set(capability["acceptance_test_ids"]) <= test_ids, capability["id"]
        if capability["route"] == "official-pack":
            assert capability["id"] in covered_by_pack, capability["id"]
        else:
            assert capability["route"] in {"core", "rejected"}, capability["id"]
    required_connectors = {"calendar","contacts","email","notes-tasks-reminders","cloud-drives","github","slack-discord-teams","telegram-signal-whatsapp","generic-webhooks","home-assistant","media-services","maps-travel-weather-news","shopping-booking-delivery","image-audio-video-generation"}
    assert required_connectors <= connector_ids, sorted(required_connectors - connector_ids)
    assert {row["acceptance_test"] for row in evaluation_rows} <= test_ids
    print(f"verified {len(pack_paths)} official packs, {len(evaluation_rows)} evaluations, and {len(known_capabilities)} capability routes")


if __name__ == "__main__":
    main()
