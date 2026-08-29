# SPEC-V2 implementation progress

## Baseline

- Branch preparation: `pre-spec-v2-baseline` and `spec-v2` both start at `be1f62d` (`chore: capture pre-SPEC-V2 baseline`). The working tree was clean when this run began.
- Toolchains: `rustc 1.98.0 (88d9e12ae 2026-08-18) (Arch Linux rust 1:1.98.0-1)` and Bun `1.4.0`.
- Windows target installation is unavailable in this environment: `rustup target add x86_64-pc-windows-msvc` exits 127 with `/bin/bash: rustup: command not found`.
- Baseline full gate passed: `bun install --frozen-lockfile`, `bun run sidecar:fetch`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `bun run check`, `bun run test`, `bun run --cwd apps/desktop build`, `python scripts/verify-registries.py`, `python scripts/verify-packs.py`, `python scripts/verify-performance.py`, `python scripts/verify-fuzz.py`, `python scripts/security-gate.py`, and `python scripts/generate-release-metadata.py --check`.
- Inherited baseline failures:
  - `cargo check -p personal-agent-desktop --target x86_64-pc-windows-msvc` exits 101: `error[E0463]: can't find crate for core` because the Windows target is not installed.
  - `bun scripts/verify-bundle-size.ts` exits 1: `error: Module not found "scripts/verify-bundle-size.ts"` (the verifier is introduced by PERF-13).
- Baseline desktop bundle: `dist/assets/index-CILpN_aC.js 761.88 kB | gzip: 207.29 kB`; Vite warns that a chunk exceeds 500 kB.

## Task ledger

| Task | Status | Evidence | Commit |
|---|---|---|---|
| A-1 | BLOCKED-NETWORK | Host `cargo check -p personal-agent-desktop` passes and non-Linux CI check is present; required Windows check exits 101 with `can't find crate for core/std` because `rustup` and the target are unavailable. Human: `rustup target add x86_64-pc-windows-msvc && cargo check -p personal-agent-desktop --target x86_64-pc-windows-msvc`. | `fbfdff1` |
| A-2 | DONE | `bun run test` → plugin 5 passed, desktop 70 passed, 0 failed. | `392d471` |
| A-3 | DONE | `cargo test -p personal-agent-runtime destructive_command -- --nocapture` → 2 passed, 0 failed. | `3c555c1` |
| A-4 | DONE | `cargo test -p personal-agent-tools` → 5 passed, 0 failed; mutation corpus includes 16 new secret-shape cases. | `1f7d2d6` |
| A-5 | DONE | `bun run test` → 14 files / 70 desktop tests passed; StrictMode delayed-listener regression passes. | `3d67094` |
| A-6 | DONE | `cargo test -p personal-agent-desktop shutdown_timeout_stops_sidecar_while_lifecycle_lock_is_held -- --nocapture` → 1 passed, 0 failed; desktop suite 57 passed. | `d7be93f` |
| A-7 | DONE | `cargo test -p personal-agent-desktop concurrent_connector_saves_remain_atomic -- --nocapture` → 1 passed, 0 failed; desktop suite 56 passed at task boundary. | `f487811` |
| A-8 | DONE | `cargo test -p personal-agent-desktop connector_oauth::tests::loopback_callback_accepts_request_in_one_byte_chunks -- --exact --nocapture` → 1 passed, 0 failed. | `a97c4f0` |

## Wave 1 gate

Wave 1 passed every full-gate command except the two inherited baseline failures. In particular,
workspace clippy finished with warnings denied, workspace Rust tests passed (desktop 57/57,
runtime 13 passed with one pre-existing ignored test), Bun tests passed (desktop 70/70), the
desktop production build completed, and all registry, pack, performance, fuzz, security, and
release-metadata verifiers passed. The inherited failures remain:

- Windows cross-check: target standard library unavailable (`E0463`).
- Bundle-size verifier: file is introduced by PERF-13 and does not exist yet.
