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

See [SPEC.md](SPEC.md), [ROADMAP.md](ROADMAP.md), and
[docs/architecture/system.md](docs/architecture/system.md).
