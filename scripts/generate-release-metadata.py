#!/usr/bin/env python3
"""Generate deterministic CycloneDX SBOM and third-party license inventory."""

from __future__ import annotations

import argparse
import json
import subprocess
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SBOM = ROOT / "release/sbom.cdx.json"
NOTICES = ROOT / "THIRD_PARTY_NOTICES.md"


def cargo_components() -> list[dict]:
    raw = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout
    metadata = json.loads(raw)
    workspace = set(metadata["workspace_members"])
    components = []
    for package in metadata["packages"]:
        if package["id"] in workspace:
            continue
        component = {
            "type": "library", "group": "crates.io", "name": package["name"],
            "version": package["version"],
            "purl": f"pkg:cargo/{package['name']}@{package['version']}",
            "licenses": [{"expression": package["license"] or "NOASSERTION"}],
        }
        components.append(component)
    return components


def npm_components() -> list[dict]:
    components: dict[tuple[str, str], dict] = {}
    for manifest in [ROOT / "package.json", *sorted(ROOT.glob("apps/*/package.json")), *sorted(ROOT.glob("packages/*/package.json"))]:
        data = json.loads(manifest.read_text(encoding="utf-8"))
        for section in ("dependencies", "devDependencies"):
            for name, version in data.get(section, {}).items():
                if version.startswith("workspace:"):
                    continue
                exact = version.removeprefix("=")
                components[(name, exact)] = {
                    "type": "library", "group": "npm", "name": name, "version": exact,
                    "purl": f"pkg:npm/{name.replace('@', '%40')}@{exact}",
                    "licenses": [{"expression": "SEE-PACKAGE"}],
                }
    return list(components.values())


def render() -> tuple[str, str]:
    components = sorted(cargo_components() + npm_components(), key=lambda item: item["purl"])
    identity = "\n".join(component["purl"] for component in components)
    serial = uuid.uuid5(uuid.NAMESPACE_URL, f"https://github.com/yuvalkolodkingal/personal-agent\n{identity}")
    sbom = {
        "bomFormat": "CycloneDX", "specVersion": "1.6", "serialNumber": f"urn:uuid:{serial}",
        "version": 1,
        "metadata": {"component": {"type": "application", "name": "Personal Agent", "version": "0.1.0", "licenses": [{"expression": "Apache-2.0"}]}},
        "components": components,
    }
    lines = ["# Third-party notices", "", "Generated from locked Rust and direct JavaScript dependencies. Transitive npm license texts are resolved by the release security job.", ""]
    for component in components:
        license_value = component["licenses"][0]["expression"]
        lines.append(f"- `{component['name']} {component['version']}` — {license_value} — `{component['purl']}`")
    return json.dumps(sbom, indent=2, sort_keys=True) + "\n", "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    sbom, notices = render()
    if args.check:
        assert SBOM.read_text(encoding="utf-8") == sbom, "release/sbom.cdx.json drifted"
        assert NOTICES.read_text(encoding="utf-8") == notices, "THIRD_PARTY_NOTICES.md drifted"
    else:
        SBOM.parent.mkdir(parents=True, exist_ok=True)
        SBOM.write_text(sbom, encoding="utf-8")
        NOTICES.write_text(notices, encoding="utf-8")
    print("verified deterministic SBOM and third-party notices")


if __name__ == "__main__":
    main()
