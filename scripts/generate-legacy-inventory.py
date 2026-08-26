#!/usr/bin/env python3
"""Generate the exhaustive, reviewable legacy-to-capability map.

The source tree is read only. The output is JSON, which is valid YAML 1.2 and
keeps CI validation dependency-free.
"""
from __future__ import annotations

import ast
from dataclasses import fields, is_dataclass
from datetime import date
import json
from pathlib import Path
import re
import subprocess
import sys

LEGACY = Path(sys.argv[1] if len(sys.argv) > 1 else "/home/yuval/Documents/GitHub/Jarvis").resolve()
OUTPUT = Path(__file__).resolve().parents[1] / "legacy-capabilities.yaml"

ROUTES = {
    "voice": ("CAP-VOICE", "AT-M3-OFFLINE-VOICE"),
    "runtime": ("CAP-RUNTIME", "AT-M2-STREAMED-TURN"),
    "browser": ("CAP-BROWSER", "AT-M5-BROWSER-SAFE"),
    "remote": ("CAP-REMOTE", "AT-M7-REMOTE-PROTOCOL"),
    "memory": ("CAP-MEMORY", "AT-M6-MEMORY-PROVENANCE"),
    "automation": ("CAP-AUTOMATION", "AT-M6-AUTOMATION-RECOVERY"),
    "safety": ("CAP-SAFETY", "AT-M5-POLICY-GATE"),
    "workspace": ("CAP-WORKSPACE", "AT-M4-ACCESSIBLE-WORKSPACE"),
    "extensions": ("CAP-EXTENSIONS", "AT-M7-PLUGIN-SAFETY"),
    "projects": ("CAP-PROJECTS", "AT-M4-PROJECT-CONTEXT"),
    "desktop": ("CAP-DESKTOP", "AT-M5-DESKTOP-TOOLS"),
    "operations": ("CAP-OPERATIONS", "AT-M9-LIFECYCLE"),
    "configuration": ("CAP-CONFIG", "AT-M1-CONFIG-ROUNDTRIP"),
    "conversation": ("CAP-CONVERSATION", "AT-M3-CONVERSATION-STATES"),
}

KEYWORDS = {
    "voice": ("audio", "wake", "vad", "stt", "tts", "speaker", "mic", "voice", "aec", "barge", "duck", "gain", "capture", "playback", "spoken", "self_echo"),
    "runtime": ("brain", "opencode", "model", "provider", "streaming", "auth", "login", "budget"),
    "browser": ("browser", "webfetch", "websearch", "chromium", "page"),
    "remote": ("remote", "pair", "device", "noise", "ristretto", "relay"),
    "memory": ("memory", "history", "facts", "scrollback", "trace"),
    "automation": ("cron", "scheduler", "schedule", "reminder", "timer", "sentinel", "monitor"),
    "safety": ("permission", "autonomy", "checkpoint", "egress", "guest", "consent", "undo", "security", "interrupt"),
    "workspace": ("ui", "hud", "canvas", "render", "plot", "diagram", "whiteboard", "overlay", "activity", "board", "theme", "htmlview", "levels"),
    "extensions": ("skill", "expert", "mcp"),
    "projects": ("project", "projstat", "gitid"),
    "desktop": ("desktop", "pointer", "screengrab", "screenshot", "apps", "portal", "window", "sysinfo"),
    "operations": ("update", "watchdog", "version", "setup", "unit", "doctor", "gripe", "packaging", "install", "resource"),
    "configuration": ("config", "confschema", "confedit", "envfile", "setting"),
}

def route(text: str) -> tuple[str, str, str]:
    value = text.lower()
    for category, words in KEYWORDS.items():
        if any(word in value for word in words):
            capability, acceptance = ROUTES[category]
            return category, capability, acceptance
    capability, acceptance = ROUTES["conversation"]
    return "conversation", capability, acceptance

def tracked(pattern: str) -> list[str]:
    result = subprocess.run(["git", "-C", str(LEGACY), "ls-files", pattern], check=True, text=True, capture_output=True)
    return [line for line in result.stdout.splitlines() if line]

def mapped(kind: str, name: str, detail: str = "") -> dict:
    category, capability, acceptance = route(f"{name} {detail}")
    return {"kind": kind, "source": name, "detail": detail, "category": category, "capability_id": capability, "acceptance_test_ids": [acceptance]}

def commands() -> list[str]:
    tree = ast.parse((LEGACY / "src/jarvis/control.py").read_text())
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id == "COMMANDS" for target in node.targets):
            return list(ast.literal_eval(node.value))
    raise RuntimeError("COMMANDS tuple not found")

def config_fields() -> list[str]:
    sys.path.insert(0, str(LEGACY / "src"))
    from jarvis.config import Config  # type: ignore
    config = Config()
    found: list[str] = []
    for section in fields(config):
        value = getattr(config, section.name)
        if is_dataclass(value):
            found.extend(f"{section.name}.{field.name}" for field in fields(value))
    return found

def documented_features() -> list[dict]:
    records: list[dict] = []
    for relative in tracked("*.md") + tracked("docs/*.md"):
        path = LEGACY / relative
        for line_number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
            if re.match(r"^#{2,4} ", line):
                title = re.sub(r"^#+\s*", "", line).strip()
                records.append(mapped("documented-feature", f"{relative}:{line_number}", title))
    return records

def limitations() -> list[dict]:
    records: list[dict] = []
    sources = [LEGACY / "docs/cross-platform.md", LEGACY / "docs/threat-model.md", LEGACY / "README.md"]
    marker = re.compile(r"\b(unsupported|cannot|does not|fallback|degrad|limitation|Wayland|Windows|macOS|GNOME|KDE|wlroots)\b", re.I)
    for path in sources:
        if not path.exists(): continue
        for line_number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
            text = line.strip()
            if text and marker.search(text): records.append(mapped("platform-limitation", f"{path.relative_to(LEGACY)}:{line_number}", text[:500]))
    return records

def main() -> None:
    if not (LEGACY / ".git").exists(): raise SystemExit(f"legacy repository not found: {LEGACY}")
    inventory: list[dict] = []
    inventory.extend(mapped("module", path) for path in tracked("src/jarvis/*.py") + tracked("src/jarvis/**/*.py"))
    inventory.extend(mapped("test", path) for path in tracked("tests/test_*.py"))
    inventory.extend(mapped("cli-command", command) for command in commands())
    inventory.extend(mapped("config-field", name) for name in config_fields())
    inventory.extend(documented_features())
    inventory.extend(limitations())
    for item in ["config.toml", "state files", "history JSONL", "memory Markdown", "traces", "skills", "experts", "projects", "themes", "schedules and reminders", "MCP configuration", "OpenCode authentication", "remote device metadata"]:
        inventory.append(mapped("migration-input", item))
    # Deduplicate git patterns that overlap while preserving deterministic order.
    unique = {(item["kind"], item["source"]): item for item in inventory}
    inventory = [unique[key] for key in sorted(unique)]
    document = {
        "schema_version": 1,
        "generated_from": str(LEGACY),
        "legacy_commit": subprocess.run(["git", "-C", str(LEGACY), "rev-parse", "HEAD"], check=True, text=True, capture_output=True).stdout.strip(),
        "verified_on": str(date.today()),
        "source_confidence": "high-code-and-tests",
        "read_only": True,
        "counts": {kind: sum(item["kind"] == kind for item in inventory) for kind in sorted({item["kind"] for item in inventory})},
        "inventory": inventory,
    }
    OUTPUT.write_text(json.dumps(document, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps(document["counts"], indent=2))

if __name__ == "__main__": main()
