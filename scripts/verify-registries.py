#!/usr/bin/env python3
"""Dependency-free registry integrity checks for CI."""
from __future__ import annotations
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
names = ["legacy-capabilities.yaml", "competitors.yaml", "capabilities.yaml", "acceptance-tests.yaml", "rejections.yaml"]
documents = {name: json.loads((ROOT / name).read_text()) for name in names}
capabilities = {record["id"] for record in documents["capabilities.yaml"]["capabilities"]}
tests = {record["id"] for record in documents["acceptance-tests.yaml"]["tests"]}
competitors = {record["id"] for record in documents["competitors.yaml"]["products"]}
assert len(capabilities) == len(documents["capabilities.yaml"]["capabilities"]), "duplicate capability ID"
assert len(tests) == len(documents["acceptance-tests.yaml"]["tests"]), "duplicate acceptance-test ID"
assert len(competitors) == len(documents["competitors.yaml"]["products"]), "duplicate competitor ID"
for record in documents["capabilities.yaml"]["capabilities"]:
    required = {"id","category","behavior","products","sources","verified_on","source_confidence","legacy_status","security_privacy","route","platforms","dependencies","milestone","acceptance_test_ids","status"}
    assert required <= record.keys(), f"incomplete capability {record.get('id')}"
    assert set(record["acceptance_test_ids"]) <= tests, f"unknown test on {record['id']}"
    assert len(record["products"]) == len(record["sources"]), f"source/product mismatch on {record['id']}"
    assert record["route"] in {"core", "official-pack", "third-party", "rejected"}, f"invalid route on {record['id']}"
    assert record["status"] in {"planned", "in_progress", "passed", "rejected"}, f"invalid status on {record['id']}"
    for source in record["sources"]:
        assert source.startswith(("https://", "legacy://")), f"non-primary source on {record['id']}: {source}"
        assert "example.invalid" not in source and "pending" not in source, f"placeholder source on {record['id']}"
for record in documents["competitors.yaml"]["products"]:
    required = {"id", "product", "category", "primary_source", "verified_on", "source_confidence", "capabilities", "status"}
    assert required <= record.keys(), f"incomplete competitor {record.get('id')}"
    assert record["primary_source"].startswith("https://"), f"invalid competitor source on {record['id']}"
    assert record["capabilities"], f"competitor has no capabilities: {record['id']}"
for item in documents["legacy-capabilities.yaml"]["inventory"]:
    assert item["capability_id"] in capabilities, f"unmapped capability for {item['source']}"
    assert set(item["acceptance_test_ids"]) <= tests, f"unknown test for {item['source']}"
assert documents["legacy-capabilities.yaml"]["inventory"], "legacy inventory is empty"
required_legacy_kinds = {"module", "cli-command", "config-field", "test", "documented-feature", "platform-limitation", "migration-input"}
actual_legacy_kinds = {item["kind"] for item in documents["legacy-capabilities.yaml"]["inventory"]}
assert required_legacy_kinds <= actual_legacy_kinds, f"missing legacy inventory kinds: {required_legacy_kinds - actual_legacy_kinds}"
assert documents["legacy-capabilities.yaml"]["read_only"] is True, "legacy inventory must be read-only"
print(f"verified {len(capabilities)} capabilities, {len(competitors)} competitors, {len(tests)} acceptance tests, and {len(documents['legacy-capabilities.yaml']['inventory'])} legacy mappings")
