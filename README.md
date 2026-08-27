# Personal Agent

Personal Agent is an Apache-2.0 desktop assistant built around bounded
autonomy. It combines a cinematic voice HUD with a full workspace for durable
goals, task graphs, browser and desktop work, memory, automations, approvals,
artifacts, and diagnostics. The default configurable persona is **JARVIS**.

The project is greenfield. The legacy Jarvis repository is used only as a
read-only behavioral reference and migration source; its Git history and
personal state are never copied here.

## Status

This repository is private during its initial hardening period. It is under
milestone-driven construction. Registry status is the
source of truth: no milestone or capability is called complete until its
acceptance tests pass. Unsupported platform states are returned explicitly
with a reason and remediation.

## Development

Prerequisites are Bun 1.4.0, Rust 1.98.0, and the platform dependencies needed
by Tauri 2. All dependency versions are exact and lockfiles are committed.

```sh
bun install
bun run sidecar:fetch
cargo test --workspace
bun run check
bun run test
```

For a native desktop build, fetch the verified sidecar first and run
`bunx tauri build --debug --no-bundle`. The read-only CLI supports `status`,
`doctor`, `config print-default`, `config check PATH`, and metadata-only legacy
discovery with `migration dry-run CONFIG_ROOT DATA_ROOT [OPENCODE_AUTH]`.

### English neural voice

The Balanced voice profile uses a persistent local worker: Moonshine Medium
Streaming performs incremental English recognition on CPU and Qwen3-TTS 0.6B
CustomVoice synthesizes the built-in `Ryan` voice on CUDA. The worker protocol
is owned by the Tauri host; model output never enters the renderer as commands.
Whisper `base` and Piper Lessac remain private compatibility fallbacks when a
neural model cannot load. Voice settings can install the isolated Python 3.12
runtime and verified model profile without changing the system Python.

Capture exposes explicit `loading_model`, `listening`, `endpointing`, and
`transcribing` states, live partial text, adaptive silence endpointing, and
barge-in. Barge-in invalidates in-flight synthesis as well as stopping current
playback, so late audio cannot begin after the user interrupts it.

See [SPEC.md](SPEC.md), [ROADMAP.md](ROADMAP.md), and
[docs/architecture/system.md](docs/architecture/system.md).
