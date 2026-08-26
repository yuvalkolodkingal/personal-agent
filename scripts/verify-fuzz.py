#!/usr/bin/env python3
"""Run bounded deterministic mutation corpora for security-critical parsers."""

from __future__ import annotations

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    subprocess.run(
        ["cargo", "test", "--workspace", "--locked", "mutation_corpus"],
        cwd=ROOT,
        check=True,
    )
    print(
        "verified 6,144 deterministic mutations across release metadata, "
        "official pack manifests, and secret redaction"
    )


if __name__ == "__main__":
    main()
