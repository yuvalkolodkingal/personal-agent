#!/usr/bin/env python3
"""Project pinned OpenCode 3.1 into the reviewed M2 client surface."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "contracts/openapi/opencode-1.18.23.json"
OUTPUT = ROOT / "contracts/openapi/opencode-1.18.23.client.json"
SOURCE_SHA256 = "dfb7d42a555389f0c662fa2b4a8af1d61633c96710cf54bce3ff2404e2e7d896"

ROUTES = {
    "/api/health",
    "/event",
    "/permission/{requestID}/reply",
    "/provider",
    "/question/{requestID}/reject",
    "/question/{requestID}/reply",
    "/session",
    "/session/{sessionID}",
    "/session/{sessionID}/abort",
    "/session/{sessionID}/fork",
    "/session/{sessionID}/prompt_async",
    "/session/{sessionID}/summarize",
}

REQUIRED_OPERATIONS = {
    "event.subscribe",
    "permission.reply",
    "provider.list",
    "question.reject",
    "question.reply",
    "session.abort",
    "session.create",
    "session.fork",
    "session.get",
    "session.prompt_async",
    "session.summarize",
    "v2.health.get",
}


def references(value: Any) -> set[str]:
    found: set[str] = set()
    if isinstance(value, dict):
        reference = value.get("$ref")
        if isinstance(reference, str) and reference.startswith("#/components/schemas/"):
            found.add(reference.rsplit("/", 1)[-1])
        for child in value.values():
            found.update(references(child))
    elif isinstance(value, list):
        for child in value:
            found.update(references(child))
    return found


def convert_schema(value: Any) -> Any:
    if isinstance(value, list):
        return [convert_schema(child) for child in value]
    if not isinstance(value, dict):
        return value
    converted = {key: convert_schema(child) for key, child in value.items()}
    for name, bound in (("exclusiveMinimum", "minimum"), ("exclusiveMaximum", "maximum")):
        exclusive = converted.get(name)
        if isinstance(exclusive, (int, float)) and not isinstance(exclusive, bool):
            converted[bound] = exclusive
            converted[name] = True
    for unsupported in (
        "$schema",
        "unevaluatedProperties",
        "contentEncoding",
        "contentMediaType",
        "minContains",
        "maxContains",
    ):
        converted.pop(unsupported, None)
    return converted


def expand_deep_object_parameters(paths: dict[str, Any]) -> None:
    for path in paths.values():
        for method, operation in path.items():
            if method.lower() not in {"get", "post", "put", "patch", "delete"}:
                continue
            expanded: list[dict[str, Any]] = []
            for parameter in operation.get("parameters", []):
                if parameter.get("in") != "query" or parameter.get("style") != "deepObject":
                    expanded.append(parameter)
                    continue
                name = parameter["name"]
                properties = parameter.get("schema", {}).get("properties", {})
                for property_name, schema in properties.items():
                    expanded.append(
                        {
                            "name": f"{name}[{property_name}]",
                            "in": "query",
                            "schema": schema,
                            "required": False,
                        }
                    )
            operation["parameters"] = expanded
            success = {
                status: response
                for status, response in operation.get("responses", {}).items()
                if status.startswith("2")
            }
            operation["responses"] = success


def project(document: dict[str, Any]) -> dict[str, Any]:
    missing_routes = sorted(ROUTES.difference(document["paths"]))
    if missing_routes:
        raise ValueError(f"OpenCode contract is missing required routes: {missing_routes}")

    paths = {
        route: json.loads(json.dumps(document["paths"][route])) for route in sorted(ROUTES)
    }
    expand_deep_object_parameters(paths)
    operations = {
        operation["operationId"]
        for path in paths.values()
        for method, operation in path.items()
        if method.lower() in {"get", "post", "put", "patch", "delete"}
    }
    missing_operations = sorted(REQUIRED_OPERATIONS.difference(operations))
    if missing_operations:
        raise ValueError(f"OpenCode contract is missing operations: {missing_operations}")

    available = document.get("components", {}).get("schemas", {})
    wanted = references(paths)
    schemas: dict[str, Any] = {}
    while wanted:
        name = wanted.pop()
        if name in schemas:
            continue
        if name not in available:
            raise ValueError(f"OpenCode contract references missing schema {name}")
        schema = available[name]
        schemas[name] = schema
        wanted.update(references(schema).difference(schemas))

    projected = {
        "openapi": "3.0.3",
        "info": {
            "title": "Personal Agent pinned OpenCode client surface",
            "version": "1.18.23",
            "description": "Generated from the exact authenticated OpenCode /doc response.",
        },
        "paths": paths,
        "components": {"schemas": {name: schemas[name] for name in sorted(schemas)}},
        "security": [],
    }
    return convert_schema(projected)


def render() -> str:
    source = SOURCE.read_bytes()
    actual_hash = hashlib.sha256(source).hexdigest()
    if actual_hash != SOURCE_SHA256:
        raise ValueError(
            "pinned OpenCode source contract fingerprint differs from the reviewed release: "
            f"expected {SOURCE_SHA256}, got {actual_hash}"
        )
    document = json.loads(source)
    return json.dumps(project(document), indent=2, ensure_ascii=False) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()
    rendered = render()
    if arguments.check:
        if not OUTPUT.exists() or OUTPUT.read_text(encoding="utf-8") != rendered:
            raise SystemExit("generated OpenCode client contract drifted")
        print("OpenCode client contract is current")
    else:
        OUTPUT.write_text(rendered, encoding="utf-8")
        print(f"generated {OUTPUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
