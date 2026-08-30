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
| PERF-1 | DONE | `python scripts/verify-performance.py` → verified extended deterministic replay distributions; perf unit tests 2 passed, 0 failed; diagnostics now emits phase and turn first-delta data. SPEC Files-line omission: the required turn path also needed `api.rs`. | `e864cb8` |
| RUN-0 | BLOCKED-EXTERNAL / BLOCKED-CONTRADICTION | Two source patches and the PTY skip handoff are in `patches/opencode/`; fork selection is fail-closed. GitHub fork/release/CI require a valid credential. The requested before-hook patch contradicts pristine v1.18.23, which already short-circuits sync throws and async rejections (`exit=Failure`, executor count 0). | `1ac32c2` |
| PERF-2 | DONE | 10,000-event reopen test replays fewer than 100 rows; checkpoint/full goal recovery equivalence passes; storage 8, core 35, and desktop 61 tests pass. | `b0fc9e0` |
| PERF-3 | DONE | `app_paint_precedes_deferred_gdbus_probe_in_perf_report` passes; replay `startup_native_setup` maximum is 83 µs against the 800 ms limit; desktop 61 tests pass. | `0619645` |
| PERF-4 | DONE | Slim bootstrap releases the startup shield independently of lazy catalog loading; `App.test.tsx` 17/17, desktop frontend 73/73, and desktop Rust 61/61 pass. | `ee50bf1` |
| FIX-27 | DONE | Exhaustive 3,584-case policy matrix and 4,096-case nested redaction corpus pass; policy line coverage is 100.00%, combined policy/tools line coverage 95.65%. | `1aeaff9` |
| PERF-6 | DONE | The v4 database migration test creates a private encrypted backup before applying the v5 usage indexes and preserves all seeded data; storage tests and strict clippy pass. | `f77150e` |
| PERF-7 | DONE | The 5,000-event test writes no `usage-ledger-v1` snapshots and recovers exact SQL aggregates; frozen legacy ledgers merge without rewrite, and server-filtered 50-row UI pagination passes. | `7e2627e` |
| PERF-8 | DONE | Supervisor, scheduler, and MCP bursts persist once per 10 mutations; critical/shutdown flushes and bounded artifact formats pass. The wave gate's export-version race was reproduced, fixed, stress-tested 10 times, and folded into this task commit. | `0f5f26c` |
| FIX-7 | DONE | The pinned multilingual E5 int8 worker passes its real 118 MB model protocol and English/Hebrew top-3 fixtures; finite-width numeric validation, memory tests, and desktop clippy pass. | `0592003` |
| FIX-8 | DONE | `recall_index_handles_ten_thousand_memories_under_fifty_ms` materializes one FTS candidate and reports 569.228 µs; clone/deserialization, eligibility-before-limit, and approval regressions pass. | `1048c2e` |
| FIX-9 | DONE | Trigger-audited persistence adds exactly one `memories` row with zero updates/deletes or legacy blob rewrites; transactional v5→v6 migration preserves vectors, metadata, links, and pre-existing `legacy-imported` personal data. | `efa0826` |
| PERF-10 | DONE | FakeRuntime ran one delayed chat turn and 20 resource calls in 307 ms with `max_in_flight=21` (serialized minimum 700 ms); retained-handle replacement, held-lock shutdown, runtime/desktop suites, and strict clippy pass. | `00b312b` |
| PERF-11 | DONE | PTY echo uses `pty-output:{id}` events with one revision-deduplicated replay and no poll timer; playback awaits a cancellable child; resident mutation/deadline/cancellation and shutdown-join tests pass; desktop Rust 80/80 and frontend 76/76 pass at the task boundary. | `78c4197` |
| PERF-12 | DONE | Pinned Tauri 2.11.5 compiled raw `ipc::Request` bodies; exact PCM16LE decode/limit tests and ArrayBuffer/no-per-sample-JSON Vitest pass, with desktop Rust 81/81 and frontend 77/77 green. | `acaf82c` |
| PERF-9 | DONE | The 4.5 GiB typed registry admits CPU fallbacks for free, evicts only idle GPU models in vision → STT → TTS order, and rejects active/stale evictions; the real Python worker protocol test verifies idempotent `unload`. Audio tests pass 27/27 with one pre-existing hardware/model-assets test ignored. | `bb19691` |
| PERF-13 | DONE | Every non-chat destination is lazy-loaded behind one Suspense boundary; the deterministic bundle gate reports 79.67 KiB gzip at the task boundary, below the 300 KiB budget, and route/build tests pass. | `330d9bd` |
| PERF-14 | DONE | The profiler regression batches 500 deltas into five revisions, renders a stable completed row once, retains a 200-row window, and pins scrolling; desktop tests pass 90/90. | `7903dda` |
| STT-1 | DONE | Real pinned-model replay measured a 9.7×–9.99× openWakeWord CPU reduction versus Moonshine and sub-13 ms wake-event latency; protocol, asset-pin, and frontend tests pass. | `cd65019` |
| STT-2 | DONE | Combined real-model replay measured 9.194× wake CPU reduction and 120.279 ms Smart Turn endpoint p95/max with 5/5 decisions; Silero state/framing and missing-model fallback tests pass. | `389ccbc` |
| STT-4 | BLOCKED-HARDWARE | AudioWorklet capture emits exact 320-sample/16 kHz frames and raw 640-byte IPC; focused tests pass 15/15 and the task-boundary desktop suite passes 87/87. Native launch built and started, but physical-microphone partials could not be exercised and the user-data wake asset was absent. | `fde73c2` |
| TTS-1 | DONE | The fake worker streams three ordered clauses over a private length-prefixed PCM socket; one monotonic deadline covers connect/read/ack, and generation cancellation stops within one frame. Audio protocol, deadline, bounds, format, and strict-clippy gates pass. | `34ec158` |
| TTS-2 | DONE | Native rodio/cpal playback owns one streaming sink, stops immediately without polling, applies capture ducking, and falls back to `pw-play` only when device discovery/open fails. Audio tests pass 32/32 with one pre-existing asset test ignored; replay stop and first-audio gates pass. | `8163733` |
| TTS-3 | DONE | A bounded nonblocking Rust clause pump consumes FakeRuntime deltas, keeps exactly one completed-clause prebuffer, rejects stale playback generations, and starts sink audio before `runtime-turn-complete`; replay first-audio p95 is 216 µs against the 700 ms limit. | `765a034` |
| STT-5 | DONE | `python scripts/verify-performance.py` passes and gates `stt_partial_lag_ms` p95 < 700 ms; WER is computed over the 10-WAV CC0 corpus in `scripts/fixtures/stt-corpus/`. On this host the three STT metrics record `external-model-assets-required`, so the gate is satisfied but not yet numerically exercised. | `ca7c2d3` |
| STT-3 | DONE | Pinned wheel/model hashes are exact (`faster_whisper_wheel_dependencies_and_complete_model_are_exactly_pinned`), the status probe is metadata-bounded and fail-closed, a tampered pinned model is rejected before CUDA load, `voice_self_test_reports_the_selected_accurate_engine` passes, and the accurate stream never leaks its arbiter lease across restart. Workspace suite green. | `6db6bfd` |
| TTS-4 | DONE | `cargo test -p personal-agent-desktop kokoro` -> 3 passed, 0 failed: `voice_self_test_synthesizes_via_kokoro_when_kokoro_is_the_selected_backend`, `qwen_failure_falls_to_kokoro_and_emits_the_recovering_event`, `kokoro_failure_falls_to_piper_and_announces_every_remaining_tier`. The ladder tests assert the exact `recovering` event sequence, that Piper never runs once Kokoro recovers, that a missing pinned manifest fails closed to Piper, and that prior-tier errors chain forward. | `8180118` |
| TTS-5 | DONE | Frontend 17 files / 96 tests and desktop Rust 101/101 pass. `App.test.tsx > voice barge-in` renders the real app, asserts the wake track is not stopped during playback, that `applyConstraints({echoCancellation:true})` ran, and that 400 ms at `speech_prob 0.95` triggers `voice_stop` + Listening. Replay `internal_speaker_stop` p95 = 1 us (max 15 us) against the 100 ms barge-in bar. The `between-clauses` emission that made the half-duplex guard dead code was implemented in `api.rs` with a Rust state-machine test, and the neural wake path now reports `speech_prob`, verified against the real pinned openWakeWord + Silero models. | `c3584c3` |

## Wave 1 gate

Wave 1 passed every full-gate command except the two inherited baseline failures. In particular,
workspace clippy finished with warnings denied, workspace Rust tests passed (desktop 57/57,
runtime 13 passed with one pre-existing ignored test), Bun tests passed (desktop 70/70), the
desktop production build completed, and all registry, pack, performance, fuzz, security, and
release-metadata verifiers passed. The inherited failures remain:

- Windows cross-check: target standard library unavailable (`E0463`).
- Bundle-size verifier: file is introduced by PERF-13 and does not exist yet.

## Wave 2 gate

Wave 2 passes the full gate except the two inherited baseline failures. `bun run sidecar:fetch`
again verifies upstream 1.18.23, workspace clippy passes with warnings denied, all workspace Rust
tests pass (desktop 59/59; runtime 15 passed with one pre-existing ignored test), Bun tests pass
(desktop 70/70), the production build completes, and all performance/fuzz/security, registry,
pack, and release-metadata checks pass. The inherited exceptions remain the unavailable Windows
standard-library target and the PERF-13 bundle-size verifier that has not been introduced yet.

RUN-0 human handoff:

```sh
gh auth login -h github.com
gh repo fork anomalyco/opencode --clone=false --remote=false
# Apply patches/opencode/0001 and 0003 to v1.18.23, publish six fork artifacts,
# record their hashes, verify /doc, set fork.ready=true, then:
PERSONAL_AGENT_OPENCODE_SOURCE=fork bun run sidecar:fetch
PERSONAL_AGENT_OPENCODE_SOURCE=fork cargo test --workspace --locked
```

## Wave 3 gate

Wave 3 passes the full §14 gate except the same two inherited baseline failures. Dependency
installation made no changes, the pinned OpenCode 1.18.23 sidecar verified, formatting and
workspace clippy pass with warnings denied, and all workspace Rust tests pass (desktop 61/61;
runtime 15 passed with one pre-existing ignored live-sidecar test). Bun checks and tests pass
(desktop 73/73), the production build completes, and the registry, pack, performance, fuzz,
security, SBOM, and release-metadata verifiers all pass. The inherited exceptions were rerun:

- Windows cross-check exits 101 with `E0463` because the MSVC target standard library is absent.
- Bundle-size verification exits 1 because `scripts/verify-bundle-size.ts` is introduced by
  PERF-13 and does not exist yet.

## Wave 4 gate

Wave 4 passes the full §14 gate except the same two inherited baseline failures. The first full
run exposed a real artifact-export race (`selectedVersion` could still be unset while the snapshot
was already interactive); the exact failure was made deterministic, fixed without weakening the
assertion, stress-tested in 10 focused runs, and folded into PERF-8 before the gate was restarted.

On the clean rerun, dependency installation made no changes, the pinned OpenCode 1.18.23 sidecar
verified, formatting and workspace clippy passed with warnings denied, and every workspace Rust
test passed (desktop 74/74; core 37/37; memory 14/14; storage 14/14; runtime 15 passed with one
pre-existing ignored live-sidecar test). Bun checks and tests passed (desktop 75/75), and the
production build completed at 764.08 kB / 208.00 kB gzip. Registry, pack, performance, fuzz,
security, SBOM, and release-metadata verification all passed. The inherited exceptions were rerun:

- Windows cross-check exits 101 with `E0463` because the MSVC target standard library is absent.
- Bundle-size verification exits 1 because `scripts/verify-bundle-size.ts` is introduced by
  PERF-13 and does not exist yet.

## Wave 5 gate

Wave 5 passes the full §14 gate except the same two inherited baseline failures. Formatting and
workspace clippy pass with warnings denied, and every locked workspace Rust test passes (desktop
81/81; runtime 17 passed with one pre-existing ignored live-sidecar test). Bun checks and tests
pass (desktop 77/77), and the production build completes at 764.50 kB / 208.24 kB gzip. Registry,
pack, performance, and fuzz verification pass.

The first security-gate pass correctly detected that the generated SBOM predated PERF-10's new
exact `dashmap` dependency. `python scripts/generate-release-metadata.py` deterministically added
`dashmap 6.1.0` and its `hashbrown 0.14.5` dependency to the SBOM/notices; the security gate and
release-metadata `--check` then pass. The inherited exceptions were rerun:

- Windows cross-check exits 101 with `E0463` because the MSVC target standard library is absent.
- Bundle-size verification exits 1 because `scripts/verify-bundle-size.ts` is introduced by
  PERF-13 and does not exist yet.

## Wave 6 gate

Wave 6 passes the full §14 gate. Formatting and workspace clippy pass with warnings denied, and
every locked workspace Rust test passes (audio 27/27 with one pre-existing hardware/model-assets
test ignored; desktop 81/81; runtime 17/17 with one pre-existing ignored live-sidecar test). Bun
checks and tests pass (desktop 77/77), and the production build completes at 764.50 kB / 208.24 kB
gzip. Registry, pack, performance, fuzz, security, SBOM, and release-metadata verification all
pass. The inherited exceptions remain unchanged: the Windows MSVC standard-library target is
unavailable, and the bundle-size verifier is introduced by PERF-13 in the next wave.

## Wave 7 gate

Wave 7 passes the full §14 gate except the inherited unavailable Windows target. Dependency
installation made no changes, the pinned OpenCode 1.18.23 sidecar verified, formatting and
workspace clippy passed with warnings denied, and every locked workspace Rust test passed
(desktop 86/86; audio 27 passed with one pre-existing hardware/model-assets test ignored; runtime
17 passed with one pre-existing ignored live-sidecar test). Bun checks and tests passed (desktop
17 files / 90 tests), and the production build completed at 264.34 kB / 82.39 kB gzip. PERF-13's
new deterministic bundle verifier reports 80.46 KiB gzip, resolving the inherited missing-verifier
failure and remaining below the 300 KiB budget. Registry, pack, performance, fuzz (6,144
deterministic mutations), security, SBOM, and release-metadata verification all pass.

The Windows cross-check was rerun and exits 101 with `E0463`: this host has no
`x86_64-pc-windows-msvc` standard library and no `rustup` with which to install it.

STT-4 human handoff:

```sh
# Install the pinned wake assets into the configured user-data model directory, then:
bunx tauri dev
# Speak through a physical microphone and confirm partial transcripts arrive during speech.
```

## Wave 8 gate

Wave 8 passes the full §14 gate except the inherited unavailable Windows target. The frozen Bun
install made no changes and the pinned OpenCode 1.18.23 sidecar verified. Formatting and workspace
clippy pass with warnings denied; every locked workspace Rust test passes (desktop 90/90, audio
32 passed with one pre-existing hardware/model-assets test ignored, runtime 17 passed with one
pre-existing ignored live-sidecar test). Bun checks and tests pass (desktop 17 files / 90 tests),
the production build completes, and the deterministic initial-JavaScript gate reports 80.44 KiB
gzip against the 300 KiB limit. Registry, pack, deterministic performance, 6,144-case fuzz,
security, SBOM, notices, and release-metadata checks all pass.

The Windows cross-check was rerun and exits 101 with `E0463`: this host has no
`x86_64-pc-windows-msvc` standard library and no `rustup` with which to install it.

## Wave 9 gate

Wave 9 is STT-3, STT-5, TTS-4, TTS-5, TTS-6. The gate below was run at the TTS-5 boundary and
covers STT-3, STT-5, TTS-4, and TTS-5; **TTS-6 is still in flight and this gate will be re-run
before the wave is closed.** This wave was interrupted by a host crash that killed two in-flight
subagents; their work was recovered from disk, independently re-verified rather than trusted, and
finished.

`cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --locked -- -D warnings`
both exit 0, and `cargo test --workspace --locked` is fully green with the two pre-existing ignores
(audio hardware/model-assets, runtime live-sidecar); desktop is 101/101 and audio 34/34. `bun run
check`, `bun run test` (17 files / 96 tests), and the production build all pass. The deterministic
initial-JavaScript gate reports 81.32 KiB gzip against the 300 KiB limit. Registry, pack,
performance, 6,144-case fuzz, security, SBOM, and release-metadata checks all pass.

Three defects were found by verification rather than by the implementing agents, and fixed:

- TTS-4 was left with two `unused_assignments` warnings that would have failed the `-D warnings`
  wave gate, plus a `cargo fmt` violation.
- TTS-5's half-duplex guard consumed `voice-state: between-clauses`, but nothing in `api.rs` ever
  emitted it, so the AEC-unavailable listening window was dead code. The sink now emits it.
- `wake_chunk` returned `speech_prob` only on the `stt-match` fallback branch, so TTS-5's
  "400 ms above 0.9" barge-in trigger could not fire on the default openWakeWord path. The neural
  branch now reports it, requested by Rust only while playback is active so STT-1's ambient
  wake-CPU reduction is preserved, and fail-soft when Silero is unavailable.

The Windows cross-check was rerun and exits 101 with `E0463`: this host has no
`x86_64-pc-windows-msvc` standard library and `rustup` is not installed.

STT-5 follow-up available: the pinned Moonshine/faster-whisper replay assets are present on this
host under `target/stt3-live/`, so `stt_partial_lag_ms` and both WER figures can be converted from
`external-model-assets-required` into real measurements.
