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
