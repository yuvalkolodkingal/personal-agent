#!/usr/bin/env python3
"""Repository-local release security invariants; external scanners run in CI."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXCLUDED = {".git", "target", "node_modules", "dist"}
SECRET_PATTERNS = [
    re.compile(rb"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"),
    re.compile(rb"(?:ghp|github_pat)_[A-Za-z0-9_]{20,}"),
    re.compile(rb"AKIA[0-9A-Z]{16}"),
    re.compile(rb"sk-(?:proj|or-v1)-[A-Za-z0-9_-]{12,}"),
]


def files():
    for path in ROOT.rglob("*"):
        if not path.is_file() or any(part in EXCLUDED for part in path.parts):
            continue
        if "apps/desktop/src-tauri/binaries" in path.as_posix():
            continue  # pinned upstream executable is checksum-verified separately
        if path.suffix.lower() in {".png", ".ico", ".icns", ".deb", ".exe", ".dmg"}:
            continue
        yield path


def main() -> None:
    findings = []
    for path in files():
        body = path.read_bytes()
        for pattern in SECRET_PATTERNS:
            if pattern.search(body):
                findings.append(str(path.relative_to(ROOT)))
    assert not findings, f"credential-like material found: {sorted(set(findings))}"
    subprocess.run(["python", "scripts/verify-registries.py"], cwd=ROOT, check=True)
    subprocess.run(["python", "scripts/verify-packs.py"], cwd=ROOT, check=True)
    subprocess.run(["python", "scripts/generate-release-metadata.py", "--check"], cwd=ROOT, check=True)
    csp = (ROOT / "apps/desktop/src-tauri/tauri.conf.json").read_text(encoding="utf-8")
    assert "default-src 'self'" in csp and "script-src http" not in csp
    print("verified local security gate: credential scan, registry/pack safety, SBOM drift, and CSP")


if __name__ == "__main__":
    main()
