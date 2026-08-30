# Personal Agent — Specification V2 (implementation-ready)

Status: proposed · Date: 2026-08-29 · Grounded in a file-level audit of the working tree on
2026-08-29 (RTX 4050 Laptop 6 GB VRAM / i5-14450HX / 16 GB RAM reference machine).

This document is written to be executed by a coding agent in ordered workstreams. Every task has
**Files**, **Do**, and **Done when**. Decisions are locked; do not re-open them mid-implementation.
Where a task cannot be verified on Linux (Windows/macOS helpers), it says `compile-gated` — the
deliverable is code + CI compile coverage, not a live-run claim.

---

## 0. How to use this document

1. Execute workstreams in the order of §12 (A → B → C/D → E → G → H → F; F/R0 starts immediately
   in parallel). Tasks inside a workstream are ordered by ID.
2. After each workstream, run the full gate: see §14 "Verification commands".
3. Never mark a task complete if its **Done when** command fails. Registry rule from SPEC.md §1
   still governs: a claim of completion is invalid when it disagrees with the registries.
4. Do not violate an invariant in §1. If a task appears to require it, stop and flag instead.

## 1. Invariants (do not break)

These come from SPEC.md, the threat model, and `rejections.yaml`. They survive V2 unchanged:

- The renderer talks only to Rust over Tauri IPC. It never gets provider keys, sidecar
  credentials, keychain values, or direct network access. CSP stays `default-src 'self'` with no
  `script-src http`.
- Every effect (filesystem, shell, browser, desktop, connector, MCP) passes the native
  `ToolGateway` policy pipeline: validate → policy → consent → checkpoint → execute → redact →
  audit → egress → postcondition. Model text is never authority (REJ-001).
- Always-confirm effects (SPEC §6): communication, commerce, unrecoverable deletion,
  credentials/auth/security, power, external-directory writes, real-world browser commitments,
  continuous mic/camera, scope widening.
- No silent permission widening (REJ-004); no auto-install of agent-authored skills (REJ-005);
  no ToS-prohibited scraping (REJ-006); biometrics are never authorization (REJ-007);
  OpenCode hook mutation is never the safety boundary (REJ-010).
- Secrets live only in the OS keychain; files that persist definitions are mode 0600 with atomic
  unique-temp-name replace. Unsupported platform states return an explicit reason + remediation,
  never a silent fallback.
- All dependency versions pinned exactly; `unsafe_code = "forbid"` stays workspace-wide (native
  helpers that require FFI go in dedicated crates with `unsafe` locally allowed and documented,
  never in existing crates).

**Amendments to SPEC.md made by this document** (apply them as text edits in task FIX-26):

- SPEC.md §3 "A pinned stable OpenCode sidecar is behind `AgentRuntime`" → amended to "A pinned
  runtime (forked OpenCode sidecar during transition, native engine at completion) is behind
  `AgentRuntime`".
- SPEC.md §9 "upstream OpenCode not a fork" → removed; replaced by ADR-0002
  (`docs/architecture/adr-0002-standalone-runtime.md`).
- CAP-BROWSER "Chromium/CDP" becomes true (workstream H); until then the registry keeps
  `in_progress`.

## 2. Current-state map (audit anchors)

What already works and must not regress: pinned sidecar isolation + OpenAPI fingerprint
(`crates/runtime/src/lib.rs:44-48,1346-1375`), event-sourced SQLCipher store, policy/consent/audit
gateway (`crates/tools`), Linux AT-SPI tree + actions (`apps/desktop/src-tauri/src/atspi_linux.rs`),
portal consent lifecycle (`portal_linux.rs`), Moonshine streaming STT + Qwen3-TTS worker
(`scripts/voice-runtime.py`), MCP manager lifecycle + digests (`crates/mcp-manager`), durable
goal/automation supervisors, artifacts, migration, connector PKCE OAuth (GitHub/Google).

Systemic problems V2 fixes:

1. **Sidecar owns everything.** All model calls, tool loops, sessions, MCP processes, PTYs live in
   OpenCode 1.18.23. Native side has zero LLM client (verified: no provider HTTP calls anywhere).
   PTYs die on sidecar restart; structured plans and per-session env are rejected
   (`crates/runtime/src/lib.rs:1819-1823,1672-1677`).
2. **Non-Linux builds are broken.** `capabilities.rs:147` calls `portal.status()`;
   `portal_stub.rs` doesn't define it.
3. **Voice pipeline blocks.** TTS synthesizes a full WAV (up to 180 s) before any audio
   (`api.rs:1890`); playback is a subprocess polled at 100 ms (`api.rs:1977`); wake word runs full
   STT on every ambient utterance (`useVoiceCapture.ts:326`); voice barge-in is impossible because
   wake capture is suspended during playback (`App.tsx:1244-1251`); audio crosses IPC as JSON
   `Vec<f32>` (`api.rs:1708,1780`).
4. **Storage write-amplifies.** Whole usage-ledger JSON rewrite per streamed event
   (`crates/core/src/lib.rs:515-531`); full event replay on every open (`crates/core/src/lib.rs:721-735`);
   `SCHEMA_VERSION=4` with no migration logic (`crates/storage/src/lib.rs:15,91-147`); 20 of 23
   tables dead.
5. **One global runtime lock** serializes chat, PTY, MCP, goals, automations
   (`api.rs:329-345,495-559`); polling loops at 100 ms/180 ms/1 s/2 s/5 s.
6. **Desktop control is Linux-semantic-only.** No PipeWire frames (`portal_linux.rs:279,293,396`),
   no drag anywhere (`native_desktop.rs:591-594`), no coordinate/vision fallback, no window-management
   verbs, Windows/macOS helpers referenced but nonexistent; AT-SPI reconnects the bus on every call
   (`atspi_linux.rs:83-99`). Screen context is never fed to the agent.
7. **Browser is WebDriver-only**, port 4444 hardcoded (`capabilities.rs:706`), profile isolation
   asserted but not passed to the driver (`crates/browser/src/webdriver.rs:306-346`), takeover is a
   bool (`crates/browser/src/webdriver.rs:428-432`), no downloads/BiDi/embedded surface. CAP-BROWSER promises CDP.
8. **Frontend gaps**: single eager bundle (xterm in initial chunk), plain-text chat (no markdown,
   no tool cards — `App.tsx:2031,1347-1350`), duplicated-delta listener leak (`App.tsx:1341-1397`),
   dead MCP catalog UI, unstyled connector modals, static UI presented as live.
9. **Safety papercuts**: plugin `reviewedPermissions` misses `write/apply_patch/patch/todoread`
   (`packages/opencode-plugin/src/index.ts`); destructive-command denylist is substring-based
   (`crates/runtime/src/lib.rs:570-587`); secret redaction misses AWS/JWT shapes
   (`crates/tools/src/lib.rs:337-369`); `crates/policy` has 2 tests.
10. **Dead code that should be live**: `crates/plugins` (packs/skills/pairing, 1,346 lines, zero
    consumers), `crates/core/src/release.rs` (signed updates, unwired), `research.rs` (unwired),
    migration commands with no UI (`main.rs:266,303`).

---

## Workstream A — Hotfixes (do first, all small)

**A-1. Fix the non-Linux compile break.**
Files: `apps/desktop/src-tauri/src/portal_stub.rs`.
Do: add a `status()` method returning the same JSON shape as `portal_linux.rs`'s (state
`"unsupported"`, reason "XDG Desktop Portal is available only on Linux", remediation string);
remove `#![allow(dead_code)]`; add `cargo check --target x86_64-pc-windows-msvc` (or `--target
x86_64-apple-darwin` via `cargo zigbuild`/cross-check in CI) to the native matrix.
Done when: `cargo check -p personal-agent-desktop` passes with the portal stub compiled in a
`#[cfg(not(target_os = "linux"))]` unit test, and CI gains a non-Linux `cargo check` job.

**A-2. Close the plugin permission-name gap.**
Files: `packages/opencode-plugin/src/index.ts`, `src/index.test.ts`.
Do: add `write`, `apply_patch`, `patch`, `todoread` to `reviewedPermissions` (keeps parity with the
17 `codingTools`); add a test asserting `codingTools ⊆ reviewedPermissions ∪ {tools without
permissions}`.
Done when: `bun run test` passes including the new assertion.

**A-3. Harden the destructive-command check.**
Files: `crates/runtime/src/lib.rs:522-589`.
Do: normalize the command before matching (collapse whitespace runs, strip quotes around argv0,
lowercase), tokenize with a shell-words split, and match token sequences (`rm` + `-rf|-fr|-r -f`,
`git` `reset` `--hard`, `git` `clean`, `sudo|doas` as argv0, `mkfs*` argv0, `shutdown|reboot`
argv0, redirection to `/dev/sd*|/dev/nvme*`). Keep the substring pass as a second net. Add cases:
`rm  -rf /`, `"rm" -rf`, `command rm -rf`, `$(rm -rf x)` (deny on `$(`/backtick when args contain
them for consequential verbs).
Done when: new unit tests in `crates/runtime` cover the bypass corpus and pass.

**A-4. Broaden secret redaction.**
Files: `crates/tools/src/lib.rs:337-369`.
Do: add pattern-based redaction for: `AKIA[0-9A-Z]{16}`, `ghp_|github_pat_`, `sk-[A-Za-z0-9-_]{20,}`,
`xox[baprs]-`, JWТ shape (`eyJ` + two dots), PEM blocks, and 32+ char hex/base64 runs adjacent to
key-ish names (`token|secret|key|password\s*[:=]`). Keep existing exact-value redaction first.
Done when: mutation corpus extended with ≥12 new cases passes (`cargo test -p personal-agent-tools`).

**A-5. Fix ChatView listener leak + effect without deps.**
Files: `apps/desktop/src/App.tsx:1341-1397,1617-1629`.
Do: apply the `disposed` guard pattern already used in `McpManagerHost.tsx:24-42` to the
`runtime-event`/`runtime-turn-complete`/`voice-state` listeners; give the window-listener effect a
dependency array.
Done when: `bun run test` passes; a new test mounts ChatView twice under StrictMode and asserts a
single delta append per event (mock `listen` returning controllable unlisteners).

**A-6. Fix shutdown that skips sidecar stop.**
Files: `apps/desktop/src-tauri/src/main.rs:368-392`.
Do: replace `try_lock` with `tokio::time::timeout(5s, runtime.lock())`; on timeout, call
`abort_all_sessions()` (add to `OpenCodeSidecar`) then proceed to kill; log which path ran.
Done when: unit test drives shutdown with a held lock and asserts the timeout path stops the child.

**A-7. Unique temp names for connector persistence.**
Files: `apps/desktop/src-tauri/src/capabilities.rs:97-103`.
Do: mirror `api.rs:359` (`atomic_save_config`): UUID temp name + `create_new` + 0600 + fsync +
rename.
Done when: existing tests pass; add one asserting two concurrent saves don't corrupt.

**A-8. OAuth callback robust read.**
Files: `apps/desktop/src-tauri/src/connector_oauth.rs:492-496`.
Do: read until `\r\n\r\n` or 16 KiB cap or 5 s deadline instead of a single `read()`.
Done when: unit test feeds the request in 1-byte chunks and the flow still succeeds.

---

## Workstream B — Performance & loading (PERF)

### B1. Measure first

**PERF-1. Startup + turn tracing.**
Files: `apps/desktop/src-tauri/src/main.rs`, new `apps/desktop/src-tauri/src/perf.rs`,
`scripts/verify-performance.py`, `crates/audio/src/bin/audio-benchmark.rs`.
Do: add `tracing` spans with a `startup.` prefix around every step of `setup()` (db open, goals
replay, automation load, capability probe, mcp load, tray) and `turn.` spans around
chat_send→first-delta→turn-complete. Emit a `perf-report` JSON on demand (`diagnostics` gains a
`perf` section: last cold-start phase durations, p50/p95 of turn first-delta). Extend
`verify-performance.py` `LIMITS_US` with: `startup_native_setup` 800 ms, `bootstrap_ipc` 250 ms,
`desktop_snapshot_warm` 150 ms (replay-measured where hardware isn't required; keep the honest
"replay ≠ physical" language from `docs/operations/performance.md`).
Done when: `python scripts/verify-performance.py` runs the extended limits and passes; diagnostics
shows phase timings.

### B2. Startup path

**PERF-2. Projection checkpoints — stop replaying the world.**
Files: `crates/storage/src/lib.rs`, `crates/core/src/lib.rs:721-735`,
`apps/desktop/src-tauri/src/goals_host.rs:191-275`.
Do: persist `(projection_snapshot_blob, last_sequence)` in `runtime_snapshots` under a new
`projection.checkpoint` key every 1,000 events and on clean shutdown; `rebuild_projection_from`
loads checkpoint then replays only the tail. Same for `replay_goal_events`: reuse the existing
supervisor snapshot as the base (it already exists) and replay only events after its sequence.
Done when: new storage test writes 10k events, reopens, and asserts <100 rows replayed; goals test
asserts identical recovered state with and without checkpoint.

**PERF-3. Defer blocking probes out of `setup()`.**
Files: `apps/desktop/src-tauri/src/main.rs:459-570`,
`native_desktop.rs:210-228`, `capabilities.rs`, `mcp_host.rs` (load path).
Do: move `CapabilityState::load` (gdbus probe, PATH scans) and the MCP `manager.json`
read-and-rewrite into the deferred async task that already starts the sidecar; `bootstrap` returns
`capabilities: null` initially and the frontend listens for a `capabilities-ready` event (add it).
Stop rewriting `manager.json` on load (write only on mutation).
Done when: `startup_native_setup` span < 800 ms in replay; app window paints before the gdbus probe
runs (assert via span ordering in a test that parses the perf report).

**PERF-4. Slim `bootstrap`.**
Files: `apps/desktop/src-tauri/src/api.rs:205-232,329-356`, `apps/desktop/src/App.tsx:3327-3356`.
Do: `bootstrap` returns config + profile + last 100 events + voice status only. Move model catalog
(`runtime_catalog`), memory export, project graph, and style prefs to lazy `invoke`s made by the
views that need them (Settings, Memory panel). Frontend: keep the startup shield keyed on the slim
bootstrap only.
Done when: `bootstrap_ipc` replay limit (250 ms) passes; Settings/Memory still render their data
(tests updated).

**PERF-5. Voice preload policy.**
Files: `apps/desktop/src-tauri/src/api.rs:284-310`, `crates/core/src/config.rs` (add
`voice.preload: bool`, default `true`), `scripts/voice-runtime.py`.
Do: when `voice.preload` and a neural profile is installed, spawn the Python worker at startup
(deferred task) and send a new `warmup` command that loads STT (and TTS if `tts_backend`
is neural and VRAM budget allows — see PERF-9). Emit `voice-state: ready` when warm.
Done when: with preload on, first `voice_stream_start` after warmup shows no `loading_model` state
(worker test in `crates/audio` with a fake worker script asserts warmup handled).

### B3. Storage engine

**PERF-6. Real schema migrations.**
Files: `crates/storage/src/lib.rs:15,91-147`.
Do: implement forward-only versioned migrations keyed on `PRAGMA user_version` (v4 = current
baseline; each future change is a numbered migration fn). On upgrade: `backup_to()` first, then
migrate in a transaction. Refuse downgrade with explicit error.
Done when: test opens a v4 DB, migrates to v5 (add an index — see PERF-7), asserts backup exists
and data intact.

**PERF-7. Append-only usage ledger.**
Files: `crates/core/src/lib.rs:515-531`, `crates/core/src/usage.rs`, `crates/storage/src/lib.rs`,
`apps/desktop/src-tauri/src/usage_host.rs:59-68`.
Do: stop the read-modify-write of the whole ledger per event. Write `provider_usage` and `egress`
rows (the tables exist and are dead) per record; keep an in-memory aggregate rebuilt at open from
SQL `GROUP BY` (add indexes on `(profile_id, day)`, `(session_id)` via migration v5).
`usage_snapshot` gains `limit`/`offset`/`from`/`to` params and returns aggregates + one page.
Done when: storage test streams 5k usage events and asserts no full-ledger JSON writes (span/count
assertion), aggregates match; `UsageEgress.tsx` paginates (test updated).

**PERF-8. Debounced snapshots + bounded IPC payloads.**
Files: `apps/desktop/src-tauri/src/goals_host.rs`, `automation_host.rs`,
`mcp_host.rs:597-610`, `artifacts_host.rs:411-423`.
Do: coalesce supervisor/scheduler/MCP persistence with a 250 ms debounce and flush-on-critical
(approval, completion, shutdown); stop cloning whole managers per mutation (wrap in `Arc` +
copy-on-write of the mutated entry). `artifact_content` returns exactly one representation chosen
by a `format` param (`raw|text|terminal`), never all three.
Done when: tests assert single persisted write for a burst of 10 mutations; artifact preview tests
updated and passing.

**PERF-9. Local model arbiter (6 GB VRAM budget).**
Files: new `crates/audio/src/arbiter.rs`, `scripts/voice-runtime.py`.
Do: a small registry tracking declared VRAM cost of loadable models (Qwen3-TTS ≈1.4 GB,
faster-whisper large-v3-turbo int8 ≈1.5 GB, vision grounding ≈1.2 GB) with priority order
active-TTS > active-STT > vision, ceiling 4.5 GB. Worker gains `unload` commands; arbiter asks the
worker to unload lowest-priority idle models before loading a new one. CPU fallbacks (Moonshine,
Kokoro, ocrs) are always permitted.
Done when: unit tests cover admit/evict decisions; worker protocol test covers `unload`.

### B4. Runtime concurrency & eventing

**PERF-10. Break the global runtime lock.**
Files: `apps/desktop/src-tauri/src/api.rs`, `main.rs:47`, `crates/runtime/src/lib.rs`.
Do: split `Mutex<OpenCodeSidecar>` into (a) lifecycle-only `RwLock` (start/stop/restart take
write), and (b) a cloneable `RuntimeHandle` (progenitor client + base URL + auth) taken by read.
`chat_send`, `runtime_resource`, `runtime_operation`, PTY, MCP calls use the handle concurrently.
Per-turn state moves to a `DashMap<SessionId, TurnState>`.
Done when: an integration-style test (FakeRuntime) runs a chat turn concurrently with 20
`runtime_resource` calls and none serialize behind the turn (assert wall-clock < serial time).

**PERF-11. Replace polls with push.**
Files: `apps/desktop/src-tauri/src/pty_host.rs`, `apps/desktop/src/PersistentTerminal.tsx:226`,
`api.rs:575,1977`, `goals_host.rs:808-814`, `automation_host.rs:340-346`.
Do: PTY output already arrives on a websocket — forward it as a Tauri event
(`pty-output:{id}`) and drop the 180 ms poll (keep `pty_read` for reattach replay). Playback
completion: replace the 100 ms poll with a blocking wait on the child in a spawned task that emits
`voice-state`. Turn status poll drops to a 15 s safety net (SSE is primary). Goals/automation
ticks: replace fixed `loop+sleep` with a `tokio_util::sync::CancellationToken` + `Notify`-driven
scheduler that sleeps until next due time and wakes on mutation; wire cancellation into shutdown.
Done when: terminal echo round-trip test uses events (no poll timer in component); shutdown test
asserts resident loops exit; CPU idle sampling in diagnostics shows no periodic wakeups < 1 s apart
except audio.

**PERF-12. Binary audio IPC.**
Files: `apps/desktop/src-tauri/src/api.rs:1708,1780`, `apps/desktop/src/useVoiceCapture.ts`.
Do: `voice_stream_chunk` accepts a raw-byte payload (Tauri 2 raw `ipc::Request` body: send
`ArrayBuffer` of little-endian i16 PCM @16 kHz from the renderer; convert once in Rust). If raw
invoke bodies prove unavailable in the pinned Tauri, fall back to base64 in a single string field
(still ~3× smaller than JSON number arrays) — but attempt raw first and record which path compiled.
Done when: a Vitest test sends a chunk as ArrayBuffer through a mocked invoke asserting no
per-sample JSON; Rust side has a unit test decoding the frame; end-to-end voice tests still pass.

### B5. Frontend loading & rendering

**PERF-13. Code-split the app.**
Files: `apps/desktop/src/App.tsx`, `vite.config.ts`, `package.json`.
Do: `React.lazy` + `Suspense` for every non-Chat destination (Terminal/xterm, Goals, Artifacts,
Automations, Integrations, MCP, Usage, Skills, Diagnostics, Settings, ScreenContext); vendor
`manualChunks` for `@xterm/*`; remove the unused `@personal-agent/ui` dependency (or import it —
decision: remove).
Done when: `bun run build` reports initial chunk < 300 KB gzip (assert in a build-size script,
`scripts/verify-bundle-size.ts`, added to CI); all views still render (tests).

**PERF-14. Streaming-render hygiene.**
Files: `apps/desktop/src/App.tsx` (ChatView).
Do: extract `MessageRow` as a memoized component keyed by message id + revision; batch
`response.delta` appends with `requestAnimationFrame` coalescing; move `setLevel` audio-meter
updates into a ref + rAF write; virtualize the transcript beyond 200 messages (simple windowing,
no new dep).
Done when: a test streams 500 deltas and asserts ≤ 1 render of non-streaming rows per 100 deltas
(React Profiler harness), and the transcript stays scroll-pinned.

---

## Workstream C — STT (STT)

Locked decisions: keep **Moonshine Medium Streaming (CPU)** as the default engine. Add **Silero
VAD v5 (ONNX, CPU)** for acoustic gating, keep **Smart Turn v3.2** for semantic endpointing. Add
**faster-whisper large-v3-turbo int8 (CUDA)** as the opt-in "Accurate" profile (config enum
`large-v3-turbo` already exists). Wake word: **openWakeWord ONNX** for built-in phrases +
STT-text-match fallback for custom phrases. All engines live in the existing Python worker; the
subprocess whisper.cpp/Piper tier remains the no-Python fallback.

**STT-1. Wake word without full STT.**
Files: `scripts/voice-runtime.py`, `apps/desktop/src/useVoiceCapture.ts:205-399`,
`apps/desktop/src-tauri/src/api.rs`.
Do: add worker commands `wake_start {phrases}` / `wake_chunk` / `wake_stop` running openWakeWord
(hey-jarvis community model bundled for the default persona; SHA-256 pinned in the verified model
profile like existing assets) over the same 16 kHz stream; emit `{wake: true, score}` events. If
the configured wake phrase has no bundled model, worker returns `{fallback: "stt-match"}` and the
current transcribe-and-match path is used but gated behind Silero VAD (STT-2) so silence costs
nothing. Frontend: route ambient audio to `wake_chunk` instead of `voice_transcribe`; remove the
`wakeProcessing` block that drops speech during inference.
Done when: worker protocol tests cover wake commands; ambient CPU while armed (replay bench in
`audio-benchmark`) drops ≥ 5× vs the STT-per-utterance path; wake-to-listen replay stays < 250 ms.

**STT-2. Silero VAD gate + endpoint fusion.**
Files: `scripts/voice-runtime.py`, `crates/audio/src/neural.rs`, `useVoiceCapture.ts:524-563`.
Do: run Silero v5 (onnxruntime, already a worker dep for Smart Turn) inside `stt_chunk`; expose
`speech_prob` per chunk in the streaming response. Endpoint decision becomes: acoustic silence
(Silero below threshold for `vad_stop_milli`) → consult Smart Turn once → final. Replace the
frontend's energy-threshold VAD with the worker's `speech_prob` (frontend keeps only a cheap
pre-gate). `voice_turn_complete` no longer errors when Smart Turn is missing — it returns
`{decision: "silence-fallback"}` (fix `api.rs:1818-1823`).
Done when: replay bench shows endpoint decision ≤ 250 ms after speech end; no `Err` path for
missing Smart Turn (unit test).

**STT-3. Accurate profile (GPU whisper) behind the arbiter.**
Files: `scripts/voice-runtime.py`, `apps/desktop/src-tauri/src/api.rs:2345-2415`,
`crates/core/src/config.rs`.
Do: worker learns `stt_engine: "faster-whisper"` with `large-v3-turbo` int8_float16 on CUDA
(installed by the existing `uv` venv installer; pin package + model revision hashes in the install
manifest). Selection: `voice.stt_backend = "whisper.cpp"` stays subprocess;
`voice.stt_model = "large-v3-turbo"` + neural profile → faster-whisper in worker. Register the
model with the PERF-9 arbiter. Streaming: feed 3–5 s windows with 0.5 s overlap for partials, full
decode on endpoint for final.
Done when: `voice_self_test` reports the active engine; install test verifies pinned hashes;
transcribe test (fixture WAV) passes with both engines.

**STT-4. Capture path modernization.**
Files: `apps/desktop/src/useVoiceCapture.ts:241,631`, new `apps/desktop/src/audio-worklet.ts`.
Do: replace `ScriptProcessorNode` with an `AudioWorklet` posting 20 ms Float32 frames;
downsample to 16 kHz mono in the worklet; keep a ScriptProcessor fallback behind feature
detection. Frames go to Rust via PERF-12 binary IPC.
Done when: voice capture tests pass with the worklet mocked; manual smoke on this machine (run
`bunx tauri dev`, speak, observe partials) recorded in the PR description.

**STT-5. Accuracy/latency benchmark harness.**
Files: `crates/audio/src/bin/audio-benchmark.rs`, `scripts/verify-performance.py`, new
`scripts/fixtures/stt-corpus/` (10 short public-domain WAVs + reference transcripts).
Do: add WER computation (edit distance) over the corpus per engine, and partial-latency
measurement (audio-time vs emit-time) in replay. Emit into `performance-report.json` as
`stt_wer_moonshine`, `stt_wer_accurate`, `stt_partial_lag_ms`.
Done when: `python scripts/verify-performance.py` gates `stt_partial_lag_ms` p95 < 700 ms and
prints WER (no hard WER gate; recorded as evidence).

---

## Workstream D — TTS + barge-in (TTS)

Locked decisions: sentence/clause-chunked streaming synthesis (engine-agnostic) + a **native Rust
playback sink** (rodio) replacing the `pw-play` subprocess. **Kokoro-82M ONNX (CPU)** becomes the
mid tier wired to the existing `kokoro` config enum. Clause streaming starts TTS while the model
turn is still streaming. AEC: enable `echoCancellation` constraint on capture now; native
`webrtc-audio-processing` AEC arrives with the cpal capture end-state (deferred to R-phase, noted
in §13 — not required for barge-in correctness because we own the playback reference and can
half-duplex the wake path).

**TTS-1. Streaming synthesis protocol.**
Files: `scripts/voice-runtime.py`, `crates/audio/src/neural.rs`, `apps/desktop/src-tauri/src/api.rs:1835-1932`.
Do: add worker command `tts_stream {text, voice, generation}` producing s16le PCM frames. Control
stays JSON-lines on stdout; audio frames go over a per-request Unix domain socket whose path Rust
passes in the command (worker connects, writes length-prefixed frames, closes). Qwen path: split
text into clauses (`.!?;:` + length cap 220 chars), synthesize clause-by-clause, stream each PCM as
soon as ready. `tts_synthesize` (whole-WAV) remains for fallback tiers.
Done when: worker protocol test (fake worker in `crates/audio`) streams 3 clauses and Rust
reassembles ordered PCM; cancellation mid-stream (generation bump) stops within one frame.

**TTS-2. Native playback sink with instant stop.**
Files: new `crates/audio/src/sink.rs` (rodio + cpal, pinned versions), `api.rs:1957-2000`,
`main.rs:73-77`.
Do: play PCM frames from TTS-1 through a rodio `Sink`; `voice_stop` calls `sink.stop()` (target
< 50 ms, the existing SPEC gate) and bumps the generation counter (which also cancels worker
synthesis — the existing invalidation contract). Keep `pw-play` as fallback when no output device
is available via cpal. Delete the 100 ms completion poll (emit completion from the sink thread).
Ducking: implement `voice.ducking_percent` as sink volume reduction while STT is capturing.
Done when: replay bench `internal_speaker_stop` still < 50 ms; new bench `tts_first_audio_ms`
added; unit test covers stop-mid-stream and ducking factor.

**TTS-3. Clause streaming from the model turn.**
Files: `apps/desktop/src-tauri/src/api.rs` (turn event pump), `apps/desktop/src/App.tsx:860-863`.
Do: move speak-on-turn into Rust: when `speakResponse` is set, a clause segmenter consumes
`response.delta` events and enqueues completed clauses to TTS with a 1-clause prebuffer;
barge-in/generation bump flushes the queue. The frontend stops calling `voice_speak` after turn
completion (Rust emits `voice-state` transitions as today).
Done when: with a FakeRuntime streaming a 3-sentence reply, first audio starts before turn
completion (integration test asserts sink received frames before `runtime-turn-complete`);
`tts_first_audio_ms` p95 < 700 ms in replay with the fake engine.

**TTS-4. Kokoro CPU tier.**
Files: `scripts/voice-runtime.py`, `api.rs:2411+` (installer), `crates/core/src/config.rs` docs.
Do: worker `tts_engine: "kokoro"` via `kokoro-onnx` (CPU, int8), voice `af_heart` default; wire
`voice.tts_backend = "kokoro"`; installer pins wheel + model hashes. Fallback ladder becomes
Qwen3 (CUDA) → Kokoro (CPU) → Piper (subprocess) with the existing `recovering` event semantics.
Done when: `voice_self_test` synthesizes via kokoro when selected; ladder test simulates Qwen
failure → kokoro used, event emitted.

**TTS-5. Make voice barge-in real.**
Files: `apps/desktop/src/App.tsx:1244-1251,279,2293`, `useVoiceCapture.ts`.
Do: stop suspending wake capture during playback. While `playbackState !== "idle"`, keep the wake
stream running with (a) `echoCancellation: true` constraint, (b) a higher wake threshold, and
(c) half-duplex guard: if AEC is unavailable (constraint reports off), listen only during
inter-clause gaps (the sink knows clause boundaries — expose `voice-state: between-clauses`).
On wake or on any speech ≥ 400 ms with `speech_prob > 0.9`: `voice_stop` (stops sink + cancels
synthesis) and enter listening. Update the UI strings to reflect measured state instead of the
hardcoded "barge-in enabled".
Done when: integration test (mocked capture feeding speech during playback) asserts stop + listen;
replay barge-in bench stays < 100 ms; UI string comes from capability state.

**TTS-6. Phrase cache.**
Files: `crates/audio/src/sink.rs` or new `cache.rs`, `api.rs`.
Do: LRU disk cache (app data, 64 MiB cap) of synthesized clauses keyed by
SHA-256(engine|voice|rate|text); acknowledgement phrases ("On it.", persona lines from config)
pre-synthesized at warmup.
Done when: unit test hits cache on second request; ack phrase first-audio < 250 ms in replay.

---

## Workstream E — MCP + connectors + productivity (INT)

Locked decisions: implement a **native MCP host** in Rust with the official `rmcp` SDK (stdio +
streamable HTTP), replacing the sidecar-owned MCP runtime (`StaticRuntimeAdapter`). **Legacy SSE is
dropped** (amended 2026-08-30): `rmcp` removed its legacy SSE client transport after 0.10.0, and
HTTP+SSE is deprecated in the MCP specification itself in favour of Streamable HTTP. Keeping it
would have pinned a networking SDK nine months behind upstream, so the host refuses a `LegacySse`
definition with an explicit reason and remediation instead.
Native **OAuth 2.1 + PKCE + dynamic client registration** for remote MCP servers. A **bundled
signed catalog** verified with the existing ed25519 code in `crates/core/src/release.rs`.
Connectors gain **Microsoft (Entra) OAuth**; Slack stays token-based (Slack OAuth requires a
confidential client secret — not embeddable; the honest path is user-supplied bot/user token or a
Slack MCP server). `crates/plugins` gets wired (packs UI + runtime).

**INT-1. Native MCP transports.**
Files: new `crates/mcp-host/` (rmcp pinned), `apps/desktop/src-tauri/src/mcp_host.rs:1196+`.
Do: implement `RuntimeAdapter` (`crates/mcp-manager`'s trait) natively: spawn stdio servers
(argv-only, env allowlist, cwd pinned, kill_on_drop), connect streamable-HTTP and SSE, list tools
with **annotations preserved** (fix `mcp_host.rs:1138`), real health = measured round-trip (fix
hardcoded `latency_ms: 1` at `:1160`), reconnect with backoff, per-server log ring. The
`ToolGateway` path (`prepare_tool_call` → gateway) is unchanged. Remove the OpenCode `/mcp`
dependency; delete the "LegacySse test unsupported" error (`:1328-1330`) by testing over the native
SSE client.
Done when: `cargo test -p personal-agent-mcp-host` covers stdio echo-server round-trip (bundle a
tiny fixture server in `scripts/fixtures/mcp-echo.ts` run via bun), HTTP + SSE against a local
fixture, annotation preservation, and health latency > 0; MCP manager UI shows real latency.

**INT-2. Remote MCP OAuth, natively.**
Files: `crates/mcp-host/src/oauth.rs`, reuse `crates/connectors` PKCE machinery.
Do: RFC 8414 metadata discovery → RFC 7591 dynamic client registration (public client) → PKCE
authorization-code with loopback redirect → token in keychain under the existing binding aliases.
Refresh single-flight with TTL like `mcp_host.rs:41-62`.
Done when: fixture OAuth server test (extend `scripts/fixtures/`) completes register→authorize→
call; no client secret anywhere (assert in test).

**INT-3. Keychain setup wizard (unblocks keychain-bound servers).**
Files: `apps/desktop/src-tauri/src/mcp_host.rs:578-587,1079,1098-1104`,
`apps/desktop/src/McpManager.tsx:707`, `McpManagerHost.tsx`.
Do: replace the message-only `OpenKeychainSetup` with `keychain_set {alias}` (value arrives once
over IPC, written via `OsSecretStore`, never persisted elsewhere, input field cleared) and
`keychain_probe {alias}` → present/absent. UI: modal flow on "Add key securely" listing required
aliases with per-alias set/verify.
Done when: MCP server with a `Keychain` binding connects after the wizard in a test using an
in-memory secret store; no secret value appears in any persisted file (assert).

**INT-4. Bundled signed catalog.**
Files: new `catalog/mcp-catalog.json` + `catalog/mcp-catalog.sig`, `mcp_host.rs:272-280`,
`McpManagerHost.tssx` catalog prop, `crates/core/src/release.rs` (reuse verify).
Do: curate ~15 entries (filesystem, fetch, git, github, gitlab, memory, sqlite, postgres, time,
playwright, notion, linear, slack, home-assistant, context7) with pinned package specs +
transport + required bindings + scopes; sign with ed25519 (private key stays out of repo; CI
verifies with the public key committed next to the catalog). `AddCatalog` installs from the
verified entry; unsigned/modified catalog → explicit refusal. Host passes `catalog` to the UI.
Done when: signature verification unit test (tampered byte → reject); UI test renders entries and
installs one (mocked exec); `verify-registries.py` untouched.

**INT-5. Microsoft OAuth + connector realism.**
Files: `crates/connectors/src/lib.rs:571-592,706-732`, `apps/desktop/src-tauri/src/connector_oauth.rs:93-94`,
`apps/desktop/src/ConnectorManager.tsx:34-43,290,302`.
Do: add `OAuthProvider::Microsoft` (Entra v2 endpoints, public client + PKCE, scopes
`offline_access User.Read Mail.Read Calendars.Read` read-only defaults; client_id from config —
document the app-registration requirement in `docs/operations/development.md`). Wire refresh (MS
supports public-client refresh). Slack template: remove the OAuth affordance, label "token-based",
link the Slack MCP server path from INT-4. Frontend: OAuth button gates on provider support,
"Refresh" no longer hardcodes gmail/calendar.
Done when: connector tests cover Microsoft metadata + refresh against the local fixture; UI shows
Slack as token-based; `ConnectorManager` tests updated.

**INT-6. Connector sync + events → automation triggers.**
Files: `crates/connectors/src/lib.rs:142-147,685-689`, `crates/automation/src/lib.rs`,
`apps/desktop/src-tauri/src/automation_host.rs:780-786`.
Do: implement `SyncCursor` for real: Gmail `historyId`, Calendar `syncToken`, GitHub `since`/ETag,
Graph delta links. A resident poller (per enabled connector, min interval 60 s, jittered) diffs and
emits `TriggerEvent::Connector{...}` into the scheduler — this lights up `ConnectorEvent`
automations. Rate-limit handling: respect `Retry-After`/429 with backoff; every request writes an
egress receipt (already content-free).
Done when: fixture-server test drives one Gmail-shaped delta poll → automation fires; backoff test
on 429; receipts recorded.

**INT-7. Wire `crates/plugins` (packs, skills registry) into the app.**
Files: `apps/desktop/src-tauri/src/` new `packs_host.rs`, `apps/desktop/src` Integrations view,
`crates/plugins`.
Do: expose pack list/install(disabled-by-default)/authorize/revoke over IPC using the existing
`OfficialPackRuntime`; packs surface their connector scopes + keychain aliases before enablement
(the crate already models this). Skills registry backs the Skills view read path.
Done when: `cargo test -p personal-agent-plugins` still passes; a UI test installs a pack disabled
and shows scopes; grep proves the crate is no longer orphaned.

**INT-8. Remaining MCP polish.**
Files: `mcp_host.rs:537,1281-1298`.
Do: propagate real `truncated` from the gateway; extend uninstall mapping with a generic
"remove-by-command" fallback that shows the exact command for confirmation instead of erroring.
Done when: unit tests cover both.

---

## Workstream F — Standalone runtime / OpenCode fork (RUN)

Decision (ADR-0002): adopt a **strangler-pattern replacement** of the sidecar behind the existing
`AgentRuntime` trait. Fork first for supply-chain ownership and small unblock patches; build the
native engine in parallel; cut over when the parity suite is green. The trait, the FakeRuntime, the
fixture provider (`scripts/fixtures/openai-compatible.ts`), and the acceptance tests are the seam
that makes this safe.

**RUN-0. Fork + own the supply chain.** (start immediately, parallel with everything)
Files: new GitHub fork `<repo-owner>/opencode` at tag v1.18.23; `scripts/fetch-opencode.ts`,
`docs/operations/opencode-1.18.23.json`, `crates/runtime/src/lib.rs:44-48`.
Do: fork, build all platform artifacts in the fork's CI, publish a release with SHA-256 manifest;
point `fetch-opencode.ts` at the fork; regenerate the checksum manifest. Carry exactly these
patches (each minimal, versioned, upstream-issue-linked per ADR-0001's patch policy):
  1. Permission names: raise `write`/`apply_patch`/`patch` permission asks under their own names
     (fixes the undocumented dependency noted in the plugin audit).
  2. Hook contract: make `tool.execute.before` throw reliably short-circuit the call (upstream
     issues #42409/#32565) — defense-in-depth only; the gateway remains the boundary.
  3. Per-session environment variables on session create (unblocks
     `crates/runtime/src/lib.rs:1672-1677`).
  4. PTY reattach tokens surviving server restart (fixes the PTY-lifetime gap) — if impractical,
     skip; native PTY (RUN-3) supersedes it.
Done when: `bun run sidecar:fetch` pulls fork artifacts with verified hashes; `cargo test
--workspace` passes; the pinned `/doc` SHA-256 is regenerated from the fork build and CI green.

**RUN-1. Native provider layer.**
Files: new `crates/llm/`.
Do: async streaming clients for (a) **Anthropic Messages API** (tool use, streaming SSE, prompt
caching headers, `context-1m` beta passthrough), (b) **OpenAI-compatible** (`/v1/chat/completions`,
tools, stream; covers OpenRouter/Ollama/LM Studio/vLLM/the existing fixture), (c) OpenAI Responses
optional later. Typed event stream mirroring the existing normalized `EventEnvelope` taxonomy
(`crates/runtime/src/lib.rs:2026-2244` is the reference shape). Keys resolved from keychain aliases
only. Retry/backoff, usage extraction (tokens + provider-reported cost), abort.
Done when: `cargo test -p personal-agent-llm` passes against `scripts/fixtures/openai-compatible.ts`
(streamed tool-call turn, abort mid-stream, usage captured); no key material in logs (assert via
redaction test).

**RUN-2. Native engine: sessions, loop, compaction, subagents.**
Files: new `crates/engine/`, `crates/storage` (light up dead tables `sessions`, `tool_runs`),
`crates/runtime` (new `NativeRuntime: AgentRuntime`).
Do: implement the agent loop: system-prompt assembly (persona/config/memory context — reuse
`api.rs`'s existing context builders), tool schema injection from the `ToolGateway` registry +
MCP host (INT-1), streaming tool-call parsing → parallel dispatch through the gateway (bounded by
`agent.default_parallelism`), result feeding, compaction via provider summarize when context
crosses threshold, subagent spawn as child sessions guarded by `DurableSupervisor::validate_delegation`
(finally calling it — `crates/agent/src/lib.rs:769-810`). Sessions persist in SQLCipher
(`sessions` table + message blobs) — they survive restarts, fixing session ownership. Structured
plan submission and per-session env are supported natively (close
`crates/runtime/src/lib.rs:1819-1823,1672-1677` behind the trait). `NativeRuntime` implements every `AgentRuntime`
method; selection via `config.runtime.engine = "sidecar" | "native"` (default stays `sidecar`
until RUN-5).
Done when: the full existing FakeRuntime test matrix runs identically against `NativeRuntime`
with the fixture provider; `AT-M2-STREAMED-TURN`-equivalent test passes natively (streamed
tool-using turn end-to-end); session survives a simulated restart test.

**RUN-3. Native coding tools + PTY.**
Files: new `crates/coding-tools/` registered into `ToolGateway`; `crates/local-execution`
(+ `portable-pty` pinned); `pty_host.rs`.
Do: native implementations of `read`, `write`, `edit`, `apply_patch`, `glob`, `grep` (use `ignore`
+ `grep-searcher` crates), `list`, `bash` (through `local-execution` argv/policy path), `task`
(subagent), `todowrite/todoread`, `webfetch` (through egress policy + robots-respecting fetch),
`websearch` (provider-configurable, explicit egress). Every consequential tool declares
reversibility and gets **checkpoint + transactional rollback** via the existing gateway stages —
this closes the threat-model residual ("compat surface lacks full checkpoint/rollback"). PTY:
`portable-pty` native sessions owned by the app (survive any runtime restart), same websocket/
event surface the frontend already uses; ConPTY/macOS variants compile-gated.
Done when: each tool has unit tests incl. workspace containment + rollback; the release-gate
fixture flow (create a file via model-facing `write`) passes on `NativeRuntime`; PTY survives a
`runtime.restart` in a test.

**RUN-4. Skills + commands + agents, natively.**
Files: `crates/plugins` (SkillRegistry), `skills_agents.rs`, `crates/engine`.
Do: load `SKILL.md` skills (progressive disclosure: name+description in prompt, body on invoke),
`.opencode`/`.claude`/`.agents` import validation (crate code exists), commands as prompt
templates, specialist agents as engine presets. Agent-authored skills go to the review queue
(REJ-005), never auto-enabled.
Done when: engine test invokes a fixture skill through the loop; import test validates and
quarantines a malformed skill.

**RUN-5. Cutover + parity gate.**
Files: config default flip, `docs/architecture/adr-0002-standalone-runtime.md`, registries.
Do: flip `runtime.engine` default to `native` when ALL hold: (1) parity suite green (every
AT-M2/M6 automated equivalent passing on native), (2) perf: first-delta p95 and turn overhead ≤
sidecar baseline in replay, (3) injection red-team suite passes (prompt-injection corpus vs native
webfetch/browser tools), (4) 14-day soak on this machine with zero data-loss incidents (goals,
sessions, artifacts intact across 20 restarts — scripted). Forked sidecar remains available as
`engine = "sidecar"` for 2 releases, then removal is its own decision.
Done when: registries updated (`CAP-RUNTIME` acceptance extended with the parity tests), default
flipped, ROADMAP M15 exit recorded.

---

## Workstream G — Full desktop control (DESK)

Locked decisions: Linux-first depth (this machine: Arch + Hyprland), Windows/macOS as
compile-gated signed-helper scaffolds. Input ladder: **AT-SPI semantic → XDG portal RemoteDesktop
(when exposed) → wlroots virtual-input protocols (Hyprland) → ydotool (opt-in daemon) → X11 XTest**.
Screen: finish **PipeWire** frames; capture ladder portal→grim→X11. Vision fallback: **ocrs** OCR
(pure Rust, CPU) + optional GPU grounding model under the PERF-9 arbiter, coordinates always
verified by fresh capture + postconditions (contracts already exist in `crates/context`).

**DESK-1. PipeWire frame consumer (finish screen context).**
Files: `apps/desktop/src-tauri/src/portal_linux.rs:279,293,396`, new `portal_frames.rs`
(`pipewire` crate pinned, Linux-only dep).
Do: after `OpenPipeWireRemote`, consume the stream fd: negotiate BGRx/RGBx, maintain a bounded
ring of the latest 2 frames (ephemeral, never persisted — existing policy), expose
`latest_frame()` to the capture path so `desktop_snapshot` prefers portal frames over `grim` when
a session is live; apply redaction rectangles before exposure (attestation contract in
`crates/context/src/coordinator.rs:154-160`). Set `pipewire_transport: true` when live.
Done when: gated live test (`PERSONAL_AGENT_PORTAL_LIVE_TEST=1`) captures a frame through the
portal on this machine; unit tests cover ring bounds + redaction application; success message no
longer says "not connected".

**DESK-2. AT-SPI performance (shared connection + parallel reads).**
Files: `apps/desktop/src-tauri/src/atspi_linux.rs:83-99,210-294,395-442`.
Do: cache the a11y bus `Connection` in a `OnceCell` (invalidate on error); reuse it for snapshots
AND actions; batch per-node property reads with `futures::join!` and walk breadth-first with a
bounded concurrency of 8. Target: warm 500-node snapshot < 150 ms.
Done when: live-gated test measures snapshot time before/after (report in PR); unit tests still
pass; PERF-1's `desktop_snapshot_warm` replay limit holds.

**DESK-3. Coordinate input backends (wlr virtual input + ydotool + XTest) and drag.**
Files: new `apps/desktop/src-tauri/src/input_linux.rs` (`wayland-client` pinned),
`native_desktop.rs:559-595,591-594,724-743`.
Do: implement `zwlr_virtual_pointer_v1` + `zwp_virtual_keyboard_v1` clients (Hyprland exposes
both) for absolute pointer move/click/scroll/drag and key events; runtime-detect protocol
availability. Extend the dispatch ladder: AT-SPI → portal RemoteDesktop (existing) → wlr virtual
input → ydotool → xdotool. Implement `Drag` as press → interpolated moves (10 steps, 8 ms apart)
→ release through whichever pointer backend is active, then postcondition-verify via fresh
snapshot. Coordinate actions require a fresh capture generation + bounds check (contract exists —
enforce it in the handler). All of this only through the existing approval/consent path.
Done when: unit tests cover ladder selection + drag interpolation math; live-gated test on this
machine moves the pointer and completes a drag in a test window; `Drag` no longer returns the
unconditional error.

**DESK-4. Window management verbs.**
Files: `crates/context/src/action.rs:132-172` (extend `DesktopAction`), `native_desktop.rs`,
`capabilities.rs` (allow-list), frontend ScreenContext panel.
Do: add `FocusWindow`, `MoveResizeWindow`, `MinimizeWindow`, `MaximizeWindow`, `CloseWindow`
(always-confirm: close), `SwitchWorkspace`. Linux backends: `hyprctl dispatch` (Hyprland), EWMH
via `x11rb` (X11); other compositors → explicit unsupported reason. Risk classes: close =
consequential; others reversible.
Done when: typed contracts + policy descriptors exist for each verb; live-gated Hyprland test
moves/resizes a window; unit tests cover degradation messages.

**DESK-5. Vision fallback (OCR + grounding), approval-gated.**
Files: new `crates/vision/` (`ocrs` pinned, model files hash-pinned in the install manifest),
worker optional grounding model, `crates/context` coordinator wiring.
Do: when the semantic tree lacks a target (`fallback_window_node` case), offer a vision path:
portal/grim frame → ocrs text boxes → fuzzy-match the requested label → candidate rect →
coordinate action via DESK-3 with mandatory fresh-capture verification; consequential effects keep
always-confirm. Optional GPU grounding (icon/widget detection) loads under the arbiter only when
OCR misses; hosted VLM only if the user configured one (explicit egress).
Done when: fixture test (rendered PNG of a fake dialog) resolves a button label to a rect and
produces a verified action plan; policy test proves confirm gates; no frame bytes persisted.

**DESK-6. Expose desktop tools to the agent.**
Files: `crates/engine`/gateway registration (RUN-2 dependency for native; sidecar path via a new
gateway tool), `goals_host.rs` tool scopes.
Do: register `desktop.observe` (snapshot: window + semantic nodes, redacted) and `desktop.act`
(typed `DesktopAction` with approval + postconditions) as gateway tools so the agent can actually
drive the desktop (today nothing feeds it — `ScreenContext` is UI-only). Zone rules: desktop zone
tasks only, never inherited by delegates (SPEC §6 already states it — enforce in descriptor).
Done when: FakeRuntime test has the model call `desktop.observe` → `desktop.act` (mock bridge) with
an approval round-trip; policy tests cover zone denial for delegates.

**DESK-7. Windows/macOS helper scaffolds (compile-gated).**
Files: new `helpers/windows-desktop/` (Rust, `windows` crate: UIA tree, SendInput, WGC) and
`helpers/macos-desktop/` (Rust, `objc2` + AX/CGEvent/ScreenCaptureKit), new
`apps/desktop/src-tauri/src/helper_bridge.rs` (authenticated local pipe, protobuf frames — same
pattern as the CLI socket), `native_dictation.rs:659-678` wiring.
Do: define the helper IPC schema (observe/act/capture/type), implement the Windows helper far
enough to compile in CI (tree read + SendInput + WGC capture), macOS equivalent (AX read + CGEvent
+ SCK), both spawned/owned by the app with per-run tokens; dictation adapters point at the bridge
instead of "helper is not connected". No signing claims — unsigned dev builds only, signing stays
an M9 operations item.
Done when: both helpers `cargo check` in CI matrix; bridge protocol unit tests pass on Linux with
a fake helper; capability discovery reports "helper built, unsigned" instead of nonexistent.

**DESK-8. Kill switch.**
Files: `main.rs` (global shortcut), `capabilities.rs`, engine/gateway.
Do: register a global `Ctrl+Alt+Escape`-style abort (configurable): cancels all in-flight desktop
and browser actions, stops input backends mid-sequence, pauses goals executors, and surfaces a
toast. It must work even while an input replay is running (checked flag between steps).
Done when: test triggers the shortcut during a mocked drag and asserts mid-sequence stop + paused
executors.

---

## Workstream H — Built-in agent browser (BROW)

Locked decisions: **CDP via `chromiumoxide`** as the primary engine behind the existing
`BrowserEngine` boundary (finally matching CAP-BROWSER/SPEC §3); system Chrome/Chromium/Edge
discovery first, managed pinned-Chromium download (hash-verified, like the sidecar fetcher) as
fallback; WebDriver adapter kept for Firefox/Safari compatibility. Visible surface: **headful
managed Chromium window** for takeover now; an in-app **CDP screencast preview panel** second.
No CEF/embedded second engine (record as REJ-011).

**BROW-1. CDP engine.**
Files: new `crates/browser/src/cdp.rs` (`chromiumoxide` pinned), `crates/browser/src/lib.rs`,
`capabilities.rs:706`.
Do: implement the engine trait over CDP: launch with `--user-data-dir=<profile>`,
`--remote-debugging-pipe` (no TCP port — kills the 4444 hardcode class), per-task **ephemeral
profile dirs** and named persistent profiles (real isolation at last), headful by default.
Navigation, tabs (targets), evaluate-free structured reads (no arbitrary JS eval from model text),
screenshots, PDF. Keep domain allow/deny + download policy from the existing policy module wired
via `Fetch.enable` interception (subresource policy enforced at network layer, egress receipts per
request — content-free).
Done when: `cargo test -p personal-agent-browser` gains CDP tests against a real headless
Chromium in CI (download pinned in CI cache): navigate fixture page, policy-blocked domain
refused, receipt recorded; profile dir isolation asserted (cookie set in task A absent in task B).

**BROW-2. Hybrid snapshot + stable handles.**
Files: `crates/browser/src/cdp.rs`, snapshot types in `crates/browser`.
Do: snapshot = CDP `Accessibility.getFullAXTree` + `DOMSnapshot` merged into the existing
generation-bound handle model (role, name, value, editable, bounds, backendNodeId as the opaque
handle). Actions: click/type/select/scroll/hover/upload via CDP input dispatch to the resolved
node's coordinates with scroll-into-view; every action validates handle generation and re-snapshots
for postconditions (mirror the desktop coordinator contract). Downloads: CDP `Browser.setDownloadBehavior`
into the quarantine dir with the existing scanning hooks; uploads via `DOM.setFileInputFiles`
restricted to user-approved paths.
Done when: fixture-page tests cover form fill, select, upload, download-to-quarantine, stale-handle
rejection after navigation.

**BROW-3. Takeover + auth takeover, real.**
Files: `crates/browser` engine, `capabilities.rs`, frontend BrowserView.
Do: takeover pauses the agent's action queue (not just a bool — `crates/browser/src/webdriver.rs:428-432` pattern
replaced): agent actions are rejected while `takeover` is active; the headful window is focused
for the user; resume re-snapshots before any further action. Auth takeover flow: agent requests
`auth_takeover(domain)` → always-confirm dialog → user logs in in the headful window → agent never
sees credentials → resume. Credentials/keychain never injected into pages.
Done when: tests assert action rejection during takeover + forced fresh snapshot on resume; UI
shows takeover state.

**BROW-4. In-app preview panel (screencast).**
Files: frontend BrowserView, `crates/browser/src/cdp.rs`.
Do: CDP `Page.startScreencast` (JPEG, quality 60, capped 15 fps) → Tauri event stream → canvas in
BrowserView with a "Take over" button (focuses the real window). Read-only preview; input stays in
the real window.
Done when: manual smoke on this machine shows live preview; frame throttling test (no more than
15 fps forwarded).

**BROW-5. Safety posture.**
Files: `crates/browser/policy.rs` (exists), engine wiring, `rejections.yaml`.
Do: keep untrusted-zone rules: page content can never trigger secret reads, cross-connector
transfer, communication/commerce, or security changes without cross-zone approval naming source,
target, effect, duration, call ceiling (threat-model language). Real-world commitments (checkout,
send, post, sign) classified always-confirm by URL/action heuristics + model-declared intent —
belt and suspenders. WebMCP: when a site advertises WebMCP tools, prefer them over DOM automation
(capability probe + preference order). Add REJ-011 (no embedded second browser engine/CEF) and
REJ-012 (no default root uinput; ydotool strictly opt-in) to `rejections.yaml` via the generator.
Done when: injection fixture suite (hostile page tries to exfiltrate a fake secret via tool calls)
passes: attempts are denied and audited; registry verify passes with the new rejections.

---

## Workstream I — Finish-everything inventory (FIX)

Everything below is from the audits and not covered by A–H. Grouped; each item keeps its evidence
anchor. Execute in listed order within each group.

### I1. Goals/planning/automation

- **FIX-1 Real planner.** Replace the linear one-task-per-criterion `task_graph`
  (`goals_host.rs:300-344`) with an LLM planning step through `AgentRuntime`: structured plan
  (typed JSON schema: tasks, dependencies, agent, tool scopes, risk zones) → validate acyclic via
  `DurableSupervisor` → store; revisions increment `plan_revision` on replan (on failure or user
  steer). Sidecar path uses a JSON-forced prompt; native path uses structured outputs. Done when:
  plan test with the fixture provider yields a ≥2-branch DAG; replan increments revision.
- **FIX-2 Delegation for real.** Wire subtask creation to `validate_delegation`
  (`crates/agent/src/lib.rs:769-810`) and per-delegate isolation (worktree via existing OpenCode
  worktree flows or native git worktree in RUN-3). Done when: delegation depth/widening tests run
  in the app layer, not just the crate.
- **FIX-3 Resumable background clarification.** Stop killing tasks/runs on clarification
  (`goals_host.rs:1057-1069`, `automation_host.rs:609-621`): park as `WaitingForUser` with a
  notification route; answering resumes (approval-resume machinery already exists). Done when:
  test answers a parked clarification and the task completes.
- **FIX-4 Real cron.** Replace `*/N`-only parsing (`crates/automation/src/lib.rs:645-660`) with a
  pinned `croner`-style evaluator (5-field, DOW/DOM, ranges/lists); `daily at HH:MM` becomes a
  real cron entry (fix drift at `automation_host.rs:946`); evaluate quiet-hours minutes
  (`automation_host.rs:1022-1024`) and move quiet-hours enforcement into the crate (`crates/automation/src/lib.rs:53`).
  Done when: cron unit tests cover `0 9 * * 1-5`, DST-agnostic UTC math, quiet-hour minutes.
- **FIX-5 Trigger producers.** File/dir watcher (`notify` crate, debounced), webhook (loopback
  HTTP listener, token-authenticated, off by default), network/device (netlink/udev via `zbus`
  where possible), heartbeat; calendar/email arrive from INT-6; add the missing
  `TriggerEvent::SystemHealth` variant (`crates/automation/src/lib.rs:22` vs `:108-118`).
  Done when: each producer has a unit test emitting a `TriggerEvent` that fires an automation.
- **FIX-6 Event schedule-key collision** (`crates/automation/src/lib.rs:252`): include a monotonic counter. Done when:
  two same-ms events both run.

### I2. Memory

- **FIX-7 Neural embedder.** Replace `FeatureHashEmbedder` (`crates/memory/src/lib.rs:653-698`)
  with a pinned multilingual ONNX model via the worker or `ort` (decision: worker command
  `embed {texts}` using `intfloat/multilingual-e5-small` int8, hash-pinned; CPU) with the
  feature-hash as offline fallback. Store vectors per memory; keep provenance labels honest
  (`"e5-small-int8"`). Done when: recall quality test (fixture corpus, top-3 contains expected)
  passes; FEATURE_AUDIT line updated.
- **FIX-8 Recall index.** Replace the full linear scan (`crates/memory/src/lib.rs:323-368`) with SQLite FTS5 for
  lexical + a flat vector table with cosine over candidates from FTS prefilter (good enough at
  personal scale; no new vector-DB dep). Done when: 10k-memory bench recalls < 50 ms.
- **FIX-9 Stop full-store rewrites.** Persist memories as rows (light up `memories`/`memory_links`
  tables) instead of one JSON blob; migrate existing blob on first open (PERF-6 migration). Done
  when: write-one-memory test shows one row insert, not a store rewrite.

### I3. Frontend completion

- **FIX-10 Markdown + code rendering.** Render assistant text as sanitized markdown (pinned
  `marked` + `dompurify`, no remote assets, CSP-safe) with code blocks, copy-per-block, and safe
  link handling (`App.tsx:2031`). Done when: snapshot tests cover md/code/injection sanitization.
- **FIX-11 Tool cards.** Render `tool.started/completed` as collapsible cards (name, duration,
  status; args/results only when `ui.show_tool_details` — sidecar path shows name/status only
  since raw args are discarded at the boundary; native path (RUN-2) shows redacted args/results
  from the gateway audit). Honor `ui.show_reasoning` as an availability indicator. Done when:
  streamed fixture turn shows cards; settings toggles change rendering.
- **FIX-12 Media CSP + export bug.** Add `media-src 'self' data:` (or blob: + object URL) to
  `tauri.conf.json` CSP for artifact audio/video previews (`ArtifactsWorkspace.tsx:367-368`); fix
  History export revoking the object URL before download completes (`App.tsx:3018-3029`) by
  revoking on `focus`/timeout. Done when: preview renders in dev smoke; export test passes;
  `security-gate.py` CSP assertions still pass.
- **FIX-13 Hotkeys for real.** Wire `ui.global_hotkey` (replace hardcoded Super+J at
  `App.tsx:383`/`main.rs`), `push_to_talk_hotkey` (`App.tsx:1461`), `command_palette_hotkey`
  (`App.tsx:3398`); implement arrow-key navigation in the model/command palettes
  (`App.tsx:2493,3647-3679`). Done when: rebinding tests pass; palettes navigable by keyboard
  (a11y test).
- **FIX-14 Truth in UI.** Remove/replace hardcoded: "Implementation audit" array
  (`App.tsx:100-179` → render from `diagnostics`), `Pipeline: Balanced` (`:2288`), footer
  platform/PRIVATE MODE (`:3641` → from `diagnostic.platform`), profile card (`:3599`), workspace
  chevron (`:3578` → remove until a switcher exists), decorative `⌘K` (`:3568`). Done when: grep
  for the hardcoded strings returns only tests.
- **FIX-15 Dead/hidden surfaces.** Migration UI (wizard over `migration_dry_run`/`migration_import`
  — the Rust is complete, `main.rs:266,303`); remove dead `projection`/`submit_message` commands or
  wire them (decision: remove); remove unused `voice_status` command or call it (decision: remove);
  delete unreachable `DomainView` specs (`App.tsx:2533-2561`); drop the session Voice/Chat filter
  that keys on fields sessions never have (`App.tsx:757-776`) or set `input_modality` natively at
  session create (decision: set it in RUN-2, keep filter). Done when: `cargo check` has no
  unused-command warnings; migration wizard drives a dry-run against the synthetic fixture.
- **FIX-16 Styling holes.** Add CSS for `.modal-backdrop`, `.empty-state`, `.mcp-wizard-pane`,
  `.execution-arguments`, `.project-graph-list`, `.egress-table` (match `.mcp-modal-backdrop`
  styling). Done when: visual test snapshots exist for connector modals.
- **FIX-17 Slash-command edge.** `/foo` with no session: create the session first, then run the
  command; clear attachments on that path (`App.tsx:890`). Done when: test covers it.
- **FIX-18 Dictation surface.** Either wire `latencyReport()` into a Voice Lab panel or delete it
  (decision: wire — it feeds the FEATURE_AUDIT benchmark gap); replace the `dictation_apply` stub
  receipts (`capabilities.rs:336-362`) by routing in-app ops through `InAppDictationBuffer`
  semantics on the Rust side and rejecting only genuinely-unroutable ops. Done when: latency panel
  renders percentiles; stub fabrication gone.

### I4. Ops/process/docs

- **FIX-19 Updates wiring.** Connect `crates/core/src/release.rs` (signed manifest verify, update
  transaction, uninstall plan) to a real flow: config gains `updates.feed_url`; check → download →
  verify signature+hash+length → encrypted DB backup (PERF-6) → stage → atomic swap on next start →
  failed-health rollback. Unsigned developer feed is acceptable for dev builds; production signing
  stays an M9 operations item and must not be claimed as done here. Wire the existing
  `updates.check_on_startup` / `automatic_download` / `automatic_install` config fields.
  Done when: fixture feed test drives check→verify→stage→rollback-on-unhealthy; tampered manifest
  rejected; `FEATURE_AUDIT.md` "Updates: Not wired" row changes with evidence.
- **FIX-20 Uninstall + export UI.** Surface `UninstallPlan` and profile export
  (`EventStore::export_events_json`) in Settings → System with the existing confirm semantics
  (export-before-delete ordering enforced by the crate). Done when: UI test walks
  export→uninstall-plan and asserts deletion requires the confirmed state.
- **FIX-21 Research subsystem.** `crates/core/src/research.rs` (citations, claims, contradictions)
  is unwired: expose it as gateway tools (`research.record`, `research.report`) so cited research
  becomes a real artifact path, and render reports in Artifacts. Done when: fixture research flow
  produces a source-linked artifact with contradiction reporting.
- **FIX-22 CLI parity + control socket.** SPEC §4 promises a length-prefixed protobuf control
  channel (mode-0600 Unix socket / current-user-SID named pipe). Today the CLI is read-only and no
  socket exists (`crates/core/src/cli.rs`). Implement the socket server in the desktop host,
  route the same handlers, and extend `personal-agent` with `chat`, `goal`, `automation`, `status
  --json`, `doctor` over it (auth: peer-uid check on Unix, SID check on Windows). Done when: CLI
  round-trip test starts the host in-process and drives a FakeRuntime turn over the socket.
- **FIX-23 Watchdog + suspend/resume.** `WatchdogBackoff` exists (`crates/platform/src/lib.rs:294`)
  and `ResourceGenerations::resume()` exists but nothing calls them. Wire: sidecar/native-engine
  supervision with bounded backoff; on resume (D-Bus `PrepareForSleep` on Linux, platform events
  elsewhere) revalidate audio devices, network, browser sessions, provider auth, and bump
  resource generations so stale handles are rejected. Done when: test simulates child death → 3
  bounded restarts → healthy; resume event invalidates handles.
- **FIX-24 Notification center.** Toast actions are impossible in Tauri (documented), so add an
  in-app notification history view fed by the same routes (completion/approval/failure) with
  read/unread and deep links into the originating goal/automation. Done when: notification appears
  in history with a working deep link.
- **FIX-25 Guest/privacy sessions.** SPEC §5 requires guest sessions with separate history and
  restricted tools; `ConversationState::set_guest` exists but nothing exposes it. Add a guest
  toggle that scopes storage to a separate profile id and narrows the tool scope set. Done when:
  guest session's events are absent from the default profile projection (test).
- **FIX-26 Docs + registries truth pass.** Update `docs/FEATURE_AUDIT.md` rows as workstreams land;
  add `docs/architecture/adr-0002-standalone-runtime.md` (§13); update `docs/architecture/system.md`
  trust topology to show the native engine and native MCP host; extend
  `scripts/generate-core-registries.py` with the new acceptance tests and rejections
  (REJ-011/REJ-012), then regenerate registries. Done when:
  `python scripts/verify-registries.py` and `python scripts/verify-packs.py` pass and the generator
  is the single source (no hand-edited YAML).
- **FIX-27 Test-depth backfill.** `crates/policy` has 2 tests for the most security-critical file
  in the tree; `crates/tools` has 4. Add: policy property tests (every Effect × Risk × zone
  combination resolves to the documented decision), consent-grant expiry/count/cost edge cases,
  gateway checkpoint-required paths, and redaction fuzz (extends A-4). Done when:
  `cargo test -p personal-agent-policy -p personal-agent-tools` covers every branch of
  `PolicyEngine::decide` (verified by a `cargo llvm-cov` report committed to
  `docs/operations/coverage.md`, policy ≥ 90% lines).

---

## 12. Execution order

Ordered so nothing depends on something that doesn't exist yet. Items on the same line may run in
parallel.

```
1.  A-1 … A-8                         (hotfixes; unblocks non-Linux CI)
2.  RUN-0                             (fork + supply chain; long-running, start now)
3.  PERF-1                            (measurement before optimization)
4.  PERF-2, PERF-3, PERF-4            (startup)      ‖  FIX-27 (test depth)
5.  PERF-6, PERF-7, PERF-8            (storage)      ‖  FIX-7, FIX-8, FIX-9 (memory)
6.  PERF-10, PERF-11, PERF-12         (concurrency)
7.  PERF-9                            (model arbiter; needed by STT-3/TTS-4/DESK-5)
8.  STT-1, STT-2, STT-4               (STT core)     ‖  PERF-13, PERF-14 (frontend loading)
9.  TTS-1, TTS-2, TTS-3               (streaming voice; needs PERF-9)
10. STT-3, STT-5, TTS-4, TTS-5, TTS-6 (voice completion)
11. INT-1, INT-2, INT-3               (native MCP)   ‖  FIX-10 … FIX-18 (frontend completion)
12. INT-4, INT-5, INT-6, INT-7, INT-8 (integrations)
13. RUN-1, RUN-2                      (native provider + engine)
14. RUN-3, RUN-4                      (native tools + PTY + skills)
15. DESK-1, DESK-2, DESK-3, DESK-4    (Linux desktop depth)
16. DESK-5, DESK-6, DESK-8            (vision, agent exposure, kill switch)
17. BROW-1, BROW-2, BROW-3            (CDP browser)
18. BROW-4, BROW-5                    (preview + safety)
19. FIX-1 … FIX-6                     (planner/automation; needs RUN-2 for structured plans)
20. DESK-7                            (Windows/macOS helper scaffolds)
21. FIX-19 … FIX-26                   (ops, CLI, docs/registries)
22. RUN-5                             (cutover gate; last)
```

Milestone mapping for `ROADMAP.md` (append; do not renumber existing M0–M9):

| Milestone | Contents | Exit gate |
|---|---|---|
| M10 Hotfix & measurement | A-1…A-8, PERF-1 | non-Linux `cargo check` green; perf report emits phase timings |
| M11 Performance | PERF-2…PERF-14 | extended `verify-performance.py` limits pass; initial bundle < 300 KB gzip |
| M12 Voice | STT-1…STT-5, TTS-1…TTS-6 | first-audio p95 < 700 ms (replay); barge-in < 100 ms; wake CPU ≥5× lower |
| M13 Integrations | INT-1…INT-8 | native MCP stdio+HTTP+SSE tests green; signed catalog verified; Microsoft OAuth round-trip |
| M14 Desktop & browser | DESK-1…DESK-8, BROW-1…BROW-5 | portal frames live on Linux; drag+window verbs; CDP suite + injection corpus green |
| M15 Standalone runtime | RUN-0…RUN-5 | parity suite green on `NativeRuntime`; 14-day soak; default flipped |
| M16 Completion & ops | FIX-1…FIX-27 | FEATURE_AUDIT has no "Not wired" rows; registries regenerate clean |

New acceptance tests to add via `scripts/generate-core-registries.py` (IDs reserved here so
`verify-registries.py` stays authoritative):

`AT-M10-CROSS-COMPILE`, `AT-M11-STARTUP-BUDGET`, `AT-M11-STORAGE-APPEND`, `AT-M11-CONCURRENT-RUNTIME`,
`AT-M12-STREAMING-TTS`, `AT-M12-WAKE-EFFICIENCY`, `AT-M12-BARGE-IN-VOICE`, `AT-M13-NATIVE-MCP`,
`AT-M13-MCP-OAUTH`, `AT-M13-SIGNED-CATALOG`, `AT-M14-PORTAL-FRAMES`, `AT-M14-DESKTOP-CONTROL`,
`AT-M14-CDP-BROWSER`, `AT-M14-BROWSER-INJECTION`, `AT-M15-RUNTIME-PARITY`, `AT-M15-SESSION-DURABILITY`,
`AT-M16-UPDATE-ROLLBACK`, `AT-M16-CLI-CONTROL`.

## 13. Deferred / explicitly out of scope

Recorded so they are not silently assumed:

- **Native AEC.** `webrtc-audio-processing` integration lands with a full cpal capture path; until
  then barge-in relies on browser `echoCancellation` + the half-duplex clause-gap guard (TTS-5).
  Do not claim AEC in UI or docs.
- **Signing/notarization.** Apple Developer ID and Windows code-signing remain external credentials
  (M9). All installers produced here are unsigned dev artifacts.
- **Mobile.** Still rejected (REJ-008). The control socket (FIX-22) plus the existing remote pairing
  protocol is the sanctioned third-party client path.
- **Embedded second browser engine (CEF/WebView2 automation).** Rejected as REJ-011: doubles the
  attack surface and the binary size for a preview we get from CDP screencast.
- **Default root `uinput`.** Rejected as REJ-012: ydotool remains strictly opt-in with a
  user-managed daemon; the wlroots virtual-input path is preferred because it is per-session and
  unprivileged.
- **Hosted STT/TTS by default.** Config fields exist; defaults stay local. Any hosted adapter must
  produce an explicit egress receipt and is blocked when `voice.offline_only` is true (already
  enforced by `VoicePipeline::enforce_privacy`).
- **Removing the forked sidecar.** Not part of RUN-5; it stays selectable for two releases after
  cutover and its removal is a separate decision.

ADR to write in FIX-26 — `docs/architecture/adr-0002-standalone-runtime.md`:

> **Context.** ADR-0001 chose a pinned upstream sidecar and SPEC §9 forbade forking. Audit shows
> the sidecar owns sessions, tool loops, MCP processes, and PTYs; PTYs die on restart, structured
> plans and per-session env are rejected, and the V1 hook cannot short-circuit (REJ-010), so the
> product's core safety and durability guarantees are bounded by an external process we do not
> control.
> **Decision.** Fork for supply-chain ownership (RUN-0) and build a native runtime behind the
> existing `AgentRuntime` trait (RUN-1…RUN-4), cutting over only when the parity gate in RUN-5 is
> green. `config.runtime.engine` selects between them.
> **Consequences.** We own provider integration, tool-loop correctness, session durability, and
> checkpoint/rollback for every consequential tool — closing the threat-model residual about the
> compatibility surface. Cost: we now maintain provider clients and a tool loop, and must track
> provider API changes ourselves. Mitigation: the trait seam, FakeRuntime, the fixture provider,
> and the parity suite make the switch reversible per-user via config.

## 14. Verification commands

Full gate (run after every workstream; all must pass):

```sh
bun install --frozen-lockfile
bun run sidecar:fetch
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo check -p personal-agent-desktop --target x86_64-pc-windows-msvc   # A-1
bun run check
bun run test
bun run --cwd apps/desktop build
bun scripts/verify-bundle-size.ts                                        # PERF-13
python scripts/verify-registries.py
python scripts/verify-packs.py
python scripts/verify-performance.py
python scripts/verify-fuzz.py
python scripts/security-gate.py
python scripts/generate-release-metadata.py --check
```

Environment-gated (run when the hardware/session allows; never inferred):

```sh
PERSONAL_AGENT_ATSPI_LIVE_TEST=1  cargo test -p personal-agent-desktop atspi
PERSONAL_AGENT_PORTAL_LIVE_TEST=1 cargo test -p personal-agent-desktop portal
PERSONAL_AGENT_VOICE_SMOKE_ROOT=~/.local/share/dev.personal-agent.desktop/voice \
  cargo test -p personal-agent-audio -- --ignored
cargo test -p personal-agent-runtime -- --ignored     # real pinned sidecar / PTY
```

Definition of done for the whole document: every task's **Done when** passes, the full gate is
green, `docs/FEATURE_AUDIT.md` contains no "Not wired" rows and no "Partial" row without a named
external blocker, and the registries regenerate without hand edits.
