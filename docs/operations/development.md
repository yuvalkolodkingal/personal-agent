# Development and verification

## Pinned toolchain

- Rust 1.98.0, edition 2024
- Bun 1.4.0
- React 19.2.8, TypeScript 7.0.2, Vite 8.2.2
- Tauri crate 2.11.5 and CLI 2.11.4
- OpenCode sidecar 1.18.23

Exact dependency versions and both lockfiles are committed. Updates arrive as
tested pull requests; production never silently changes dependencies or the
sidecar.

## Local checks

```sh
bun install --frozen-lockfile
bun run sidecar:fetch
bun run check
bun run test
bun run --cwd apps/desktop build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python scripts/verify-registries.py
```

Tauri Linux development needs the distribution packages for WebKitGTK,
AppIndicator, GTK, and desktop integration. Platform packaging documentation
must list exact packages rather than failing later with a linker message.

## Generated contracts

Edit protobuf or schemas first, run `bun run contracts:generate`, and commit
generated output. CI runs drift verification. Rust uses vendored `protoc` so a
global compiler is not required.

The sidecar fetch selects the host target (or accepts
`--target=<rust-triple>`), verifies the pinned release archive against
`opencode-1.18.23.json`, extracts the executable, checks its reported version,
and places it in Tauri's ignored `externalBin` directory. Neither the archive
nor binary is committed.

The exact authenticated 1.18.23 `/doc` response is stored as the reviewed
source contract. `python scripts/generate-opencode-contract.py` projects its
12 required stable/runtime-health routes and transitive schemas into the
generated-client surface.
`bun run opencode:contract:check` verifies both the source SHA-256 and byte-for-
byte projection drift; it never contacts a running profile or reads user
provider configuration.

The runtime compatibility test starts every dependency with isolated XDG and
application-data directories. Its local OpenAI-compatible fixture records only
request shape metadata, advertises one harmless native-gateway status call, and
proves that OpenCode crosses the authenticated, session-scoped Rust tool bridge
before streaming a terminal answer. It does not inherit provider credentials,
global plugins, or project-local OpenCode configuration.

Production uses the same isolation: the child receives an application-owned
private home/config/data/cache/state/temp tree and a cleared, allowlisted
environment. Do not add provider keys, proxy variables, package-manager config,
or arbitrary `OPENCODE_*` overrides to that allowlist. Explicit provider
onboarding belongs behind keychain aliases and a reviewed native flow.

## Desktop lifecycle

The host enforces one running instance, exposes show and quit through the
system tray, and offers explicit start-at-login control. Closing the main
window hides it; tray **Quit** is the normal full shutdown path. The OpenCode
child is killed on drop even if an orderly stop cannot obtain its runtime lock.

Canonical `config.toml`, encrypted profile databases, daily JSONL logs, and a
presence-only run marker live below Tauri's per-user application-data
directory. The marker stores only product version, PID, and start time. A
surviving marker is projected as `lifecycle.started` with
`previous_unclean_run=true`; normal exit removes it. Invalid existing config is
reported and never overwritten. Fresh config and lifecycle files use mode 0600
on Unix.

The native CLI does not open the database or start the sidecar:

```sh
cargo run -p personal-agent-core --bin personal-agent -- status
cargo run -p personal-agent-core --bin personal-agent -- config print-default
cargo run -p personal-agent-core --bin personal-agent -- config check /path/to/config.toml
cargo run -p personal-agent-core --bin personal-agent -- migration dry-run /path/to/jarvis-config /path/to/jarvis-data
```

`jarvisctl` is a compatibility name for this deliberately narrow, read-only
surface. Its migration command produces the same metadata-only JSON dry run as
`personal-agent`; confirmed copying is available only through the native
Settings review flow and encrypted profile store. See
[legacy-migration.md](legacy-migration.md).

The CI installer-smoke jobs build and inspect a Debian package, Windows NSIS
installer, and macOS application bundle. Each extracted payload must contain
the native host, an OpenCode executable that reports the pinned version, and a
byte-identical copy of the reviewed safety plugin. The runtime compatibility
test additionally proves that this plugin removes upstream filesystem and
effectful tools from the model request before allowing the native status turn.
The native architecture matrix remains separate so x64 and ARM64 compilation
failures are visible independently from packaging failures.

## Secrets and personal data

Do not put provider keys, OAuth tokens, keychain exports, transcripts, memory,
browser profiles, signing material, live profile fixtures, or sanitized-looking
copies of real user data in Git. Fixtures are synthetic. Logs and support
bundles are secret-filtered and tests include canary values.

## Milestone discipline

Contracts and acceptance tests precede implementation. A milestone changes to
complete only when its exit gate passes, registries are updated, platform
differences are documented, and the full applicable suite is green. A skipped
test records an inapplicable platform state and reason.
