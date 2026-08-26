# Personal Agent roadmap

Registry status is authoritative. “In progress” means interfaces or vertical
slices exist but the milestone exit gate has not passed on all target systems.

## M0 — Specification and registries (passed)

Deliver specification, threat model, capability/competitor/acceptance/rejection
registries, architecture decisions, repository policies, and CI. The legacy
inventory currently maps code, commands, 177 config fields, tests, documented
features, limitations, and migration inputs at legacy commit
`89a87df90614adf3422f0f1fb2ba41ac4dafb522`.

Exit gate passed: `python scripts/verify-registries.py` proves that all 1,149
legacy items map to a capability and existing acceptance test, all 36 active
competitor records have a non-placeholder primary source, and all capability
records satisfy the required provenance and implementation fields.

## M1 — Foundation (in progress)

Tauri/React/Rust shell, generated contracts, SQLCipher event store and core
tables, rebuildable projection, CLI/shim, strict TOML settings and keychain
aliases, IPC, tray, autostart, logs, and crash recovery.

Exit gate: installable development builds run on Windows, macOS, and Linux and
recover UI state entirely from events.

## M2 — OpenCode runtime (in progress)

Pinned 1.18.23 authenticated sidecar, generated OpenAPI client, normalized
SSE, provider/model onboarding, sessions, safety plugin, gated MCP tools, and
compatibility suite. Effectful built-ins remain disabled.

Exit gate: fake and configured real providers complete streamed tool turns on
all platforms.

## M3 — Voice and conversation (in progress)

Audio contracts, offline/hosted pipeline, wake/VAD/STT/TTS/AEC, device state,
conversation/persona/project states, interruption, and settings.

Exit gate: networking-disabled wake-to-spoken response and measured barge-in.

## M4 — HUD and workspace (in progress)

The coordinated workspace/HUD shell, visible privacy, exact activity,
controls, navigation, reduced motion, keyboard labels, artifacts, whiteboard,
history, and settings.

Exit gate: all event types render, keyboard/accessibility and visual regressions
pass.

## M5 — Native tools, browser, and safety (in progress)

Policy/gateway vertical slice, tool schemas, zones, consent, audit, checkpoint
interfaces, platform capability reporting, browser handles/invalidation, CDP,
desktop backends, quarantine, egress, and rollback.

Exit gate: representative platform/browser tasks and injection red-team gates.

## M6 — Memory, automation, and agent runtime (in progress)

Typed acyclic goals/tasks, completion verification, memory trust/provenance,
automation failure policy, durable scheduler, retrieval, delegates, recovery,
monitoring, and control UI.

Exit gate: a multi-hour goal survives restart/provider failure without duplicate
consequential action.

## M7 — Competitive packs (planned)

Official productivity, development, communications, smart-home, media,
research, dictation, and creative packs plus plugin registry.

Exit gate: every adopted capability is core/pack and evaluated.

## M8 — Migration (in progress)

Read-only discovery and initial secret-aware dry-run types exist. Complete
synthetic fixtures, mappings, confirmed import, reports, and compatibility CLI.

Exit gate: anonymized/synthetic profiles import idempotently without source
changes.

## M9 — Hardening and release (planned)

Performance/security/fuzz/chaos/localization/docs/installers/signing/updater/
rollback/export/uninstall and final reports.

Exit gate: all product completion criteria pass. Signing credentials, store
access, unavailable OS/hardware, and commercial accounts may block only their
external verification; deterministic substitutes and unsigned artifacts remain
required.
