# Personal Agent — Product and Engineering Specification

This version-controlled contract normalizes the authoritative build
specification supplied at repository creation. The capability and acceptance
registries are part of the contract; a claim of completion is invalid when it
disagrees with those registries.

## 1. Product contract

Personal Agent is a greenfield, public, Apache-2.0 desktop assistant. It must
reproduce every useful legacy Jarvis behavior, add every distinct useful
competitor capability in `capabilities.yaml`, and remain a safe, bounded,
fully agentic system. Personal Agent is the public name; **JARVIS** is the
default configurable persona.

The product turns goals into explicit plans and durable task DAGs, delegates
isolated specialist work, runs safe work concurrently, continues unattended,
recovers from failures, schedules and monitors work, revises plans from
observations, pauses for policy, survives restarts, and supports explain,
steer, pause, resume, cancel, retry, checkpoint, and rollback. Agentic does not
mean unrestricted.

First-class targets are Windows 11 x64/ARM64, current and previous macOS on
Intel/Apple Silicon, and Linux x64/ARM64 across major distributions, GNOME,
KDE, X11, and major wlroots compositors. There is no first-party mobile app.
An optional capability-negotiated remote protocol may serve third-party
clients.

One installation may privately manage OpenCode, an isolated browser, optional
local-model processes, a watchdog, and a wake sentinel. No helper is a
separate user-managed product.

Completion requires complete legacy and competitor registry coverage; critical
flows on all platforms; fresh install, migration, update, rollback, export,
and uninstall evidence; no open critical/high security findings; and explicit,
actionable unsupported states.

## 2. Research and capability registry

Historical inputs are the legacy repository’s 124-project inventory,
landscape, harness, and browser-agent surveys. Active products are rechecked
from primary sources and recorded in `competitors.yaml`. Each distinct useful
behavior appears once in `capabilities.yaml`; unsafe, obsolete, novelty-only,
advertising, trademark-copying, or contradictory behavior appears in
`rejections.yaml`.

Every capability records a stable ID/category, behavior, products and links,
verification date/confidence, legacy status, security/privacy implications,
implementation route, platform state, dependencies, milestone, and acceptance
tests. `legacy-capabilities.yaml` exhaustively maps legacy modules, commands,
settings, tests, documentation, limitations, and migration inputs.

## 3. Fixed technology and repository architecture

- Tauri 2 shell, stable Rust native core, React/TypeScript frontend.
- Bun is pinned exactly; Cargo dependencies and lockfile are pinned.
- SQLCipher SQLite in WAL mode, with secrets only in the OS keychain.
- Protocol Buffers generate Rust and TypeScript IPC/domain types.
- JSON Schema Draft 2020-12 defines config, tool, plugin, and skill manifests.
- A pinned stable OpenCode sidecar is behind `AgentRuntime`; the beta embedded
  V2 SDK is not the production default.
- Optional Chromium is controlled through a replaceable CDP engine.
- Local inference uses ONNX Runtime and whisper.cpp-compatible interfaces.

The React UI communicates only with Rust core through Tauri IPC. It never
talks directly to OpenCode, credentials, tools, or SQLite. Rust core owns the
encrypted store, policy, audio, agent supervisor, native tools, automation,
browser, and runtime adapter. OpenCode tool calls return through the native
tool gateway. Effectful OpenCode built-ins that cannot be reliably intercepted
are disabled and replaced.

## 4. Public interfaces and domain model

`AgentRuntime` owns lifecycle/health, provider/model discovery, session begin,
resume, compact, fork, and abort, structured prompt/plan submission, normalized
streaming events, permission/clarification answers, per-session model/effort/
agent/directory selection, and version compatibility.

Every append-only event contains schema version, stable ID, wall-clock time,
monotonic sequence, origin, profile, optional session/goal/task/agent IDs,
type, and payload. UI projections must rebuild from events.

A goal contains objective, success criteria, origin/time, priority/deadline,
autonomy policy, cost/token/time/tool budgets, plan revision, status, verified
result, and artifacts. A task contains ownership/dependencies, agent/model
policy, workspace/browser/tools, risk/zone, retry/attempts, idempotency,
checkpoint, progress/output/result. Graphs are acyclic. Defaults: parallelism
three (maximum eight), delegation depth three.

Every tool declares stable ID/version/description; input/output schemas;
requirements; scopes; risk; effect; idempotency; reversibility/rollback;
data zones; presence; and platform support. Each call performs schema,
precondition, data-zone, policy, consent, checkpoint, execution, filtering,
audit/egress/cost, and postcondition stages.

Desktop uses Tauri IPC. CLI calls the same handlers over bounded,
length-prefixed protobuf frames through a mode-0600 Unix socket or a
current-user-SID Windows named pipe. Streams resume by sequence. The primary
CLI is `personal-agent`; `jarvisctl` is a migration shim.

`config.toml` is human-editable and strictly validated from one canonical
schema. Safe repair is allowed; invalid permission/risk acknowledgements are
fatal. Inapplicable values are preserved but reported. Secrets are keychain
aliases, never plaintext.

## 5. Functional contract

### Voice and conversation

Wake/hotkey/push-to-talk, multiple wake/stop/sleep phrases, enrollment,
measurable tuning, voting/refractory/noise gates, wake/hybrid/continuous modes,
VAD hysteresis/pre-roll/adaptive/semantic endpointing, partial/final
transcripts and biasing, local/hosted STT with explicit privacy-preserving
fallback, streaming local/hosted TTS, voice lifecycle, clause streaming,
prebuffer, barge-in, clipping/self-echo rejection, native AEC or explicit
half-duplex fallback, gain/ducking/hot-swap, quiet/whisper, optional speaker
verification, multilingual/translation, dictation, and opt-in meeting labels
are required. Offline voice works with networking disabled. Microphone state
is always visible.

Conversations persist across general/project contexts. Typed turns remain
silent. Sleep, mute, quiet, and stop differ. Provider/model/effort/project/
persona switches preserve unrelated state. Personas configure name/style/
register/address/voice and may narrow tools. Affective behavior is behavior,
not sentience. Guest sessions separate history and restrict tools. Voice is
concise while the workspace can be rich. Clarifications support constrained
answers.

### Agent, memory, tools, and browser

Consequential multi-step work starts with a validated typed plan. Plans and
DAG revisions are durable. Delegates use isolated sessions, worktrees, browser
profiles, and context windows. Planner/executor/reviewer roles, templates,
bounded parallelism/depth, a priority user lane, idempotent-only retry,
provider/model/tool recovery, crash-safe resumption, steering, pause/resume/
cancel/retry/checkpoint, compact background updates, all resource ceilings,
and final criteria verification are required. Text alone never completes a
goal.

Memory tiers are working, episodic, semantic, procedural, project, and
relationship/entity. Explicit remember requests are trusted. Inference is
reviewed; background observations cannot create trusted facts. Provenance,
confidence, sensitivity, expiry, conflicts, and recall markers are retained.
Retrieval combines FTS5 and a pinned permissive multilingual local vector
model. Users can inspect/edit/reject/export/delete. Compaction records source
links.

Desktop tools cover apps, windows, accessibility, input, screen capture,
clipboard, files/search, terminal/process, notifications, media/brightness,
health, power, camera with consent, OCR, and image understanding. Backends are
native Accessibility/ScreenCaptureKit/CoreAudio on macOS; UIA/Graphics
Capture/SendInput/WASAPI on Windows; AT-SPI/portal/PipeWire/wlroots/X11 on
Linux. Structured accessibility wins; pixel actions require fresh scale-aware
captures, bounds checks, and verification.

The optional browser uses isolated profiles unless users explicitly opt into
personal state. It provides stable opaque structured handles, text/snapshots/
screenshots/live view, navigation/tabs/forms/files/downloads, handle
invalidation, domain/subresource policy, quarantine/scanning hooks, takeover,
auth takeover, confirmation for commitments, WebMCP preference, replaceable
engine, and an untrusted-content boundary that withholds unrelated secrets,
email, memory, and connectors without cross-zone approval.

### Research, artifacts, extensions, and automation

Research supports citations/provenance, multi-source comparison and
contradictions, saved projects, OCR/extraction, and terminal-safe output.
Artifacts include code/diffs/tables/charts/diagrams, sanitized reports, media,
PDF/doc/sheet/deck generation, versioning/source links, and whiteboard cards.

Extensions support Agent Skills `SKILL.md`, progressive disclosure,
`.opencode`/`.claude`/`.agents` import, scoped experts, reusable commands,
requirement gating, trigger evals, self-authored proposal review, MCP transports,
OAuth/revocation, signed manifests/scopes/static analysis/provenance/preview,
WASI or process isolation, declarative UI only, immutable core policy, and
unsigned-off-by-default. Official packs cover productivity, development,
communications where terms permit, Home Assistant/media, cloud documents,
maps/travel/weather/news/commerce, and creative providers.

Automations support one-shot/interval/cron, file, calendar/email, webhook,
connector, health/network/device, semantic-change, and heartbeat triggers.
Management includes previous state, concurrency/missed-run policy, quiet hours,
routing, suspended approvals, user preemption, failure pauses, briefs/alerts,
normal policy, and no trusted background memories.

### HUD and workspace

The compact voice HUD and full workspace coordinate Chat, Goals/tasks,
Browser, Projects/terminal, Artifacts/whiteboard, History, Memory, Automations,
Integrations, Skills/agents, Usage/egress, Diagnostics, and Settings. They show
streaming transcripts/responses, exact tool progress, scoped approvals and
rollback coverage, hierarchy, stop/steer/pause/takeover, selectors, command
palette/hotkey, tray, themes/glass, reduced motion, keyboard/screen-reader/
scale support, and generated settings with purpose-built audio/permission/
credential controls. Color and animation never carry essential meaning alone.

## 6. Security and autonomy

Bounded default autonomy permits read-only/local/reversible actions inside an
approved goal and registered workspace/connector. Communication, commerce,
unrecoverable deletion, credentials/auth/security, power, external-directory
writes, real-world browser commitments, continuous microphone/camera, and
scope widening always confirm.

Consent includes goal/task, tool/effect, targets, expiry, call count, cost,
background permission, and revocation ID. Checkpoints precede a task’s first
mutation. Git uses private refs; non-Git uses an encrypted content-addressed
journal. Rollback is transactional and snapshots current state first. Coverage
is labeled full/partial/none.

Execution zones are isolated, registered workspace, and desktop. Unknown
projects default isolated; delegates never inherit desktop. Data zones are
user instruction, trusted local, private memory, secret, connector, untrusted,
and generated. Untrusted content cannot directly trigger secret reads,
cross-connector transfer, communication, commerce, or security changes.

Audit covers tools/outcomes, approvals/grants, egress, connectors, usage/cost,
memory, checkpoints/rollback, extensions, and policy. Secret values are never
recorded; transcripts and arguments are configurable.

## 7. Storage and migration

Transactional tables are events, profiles, sessions, goals, tasks, task_edges,
agent_runs, tool_runs, permission_requests, consent_grants, checkpoints,
memories, memory_links, automations, automation_runs, artifacts, connectors,
provider_usage, egress, settings, migration_runs, and internal migration_items.
Large blobs are encrypted and content-addressed.

Legacy discovery never modifies source. Dry-run precedes personal copying;
versioned mappings convert settings; history becomes legacy events; memory
keeps names/timestamps; skills/experts validate before enablement; plaintext
secrets never move; OpenCode auth adoption needs consent; remote devices
re-pair; import is idempotent/re-runnable; reports are machine/human readable;
the old profile remains untouched.

## 8. Packaging, operations, and verification

Deliver signed/notarized DMG/PKG, signed MSIX/NSIS, AppImage/DEB/RPM, x64/ARM64,
stable/beta signed updates, atomic DB backup, failed-health rollback, SBOM,
licenses, and provenance. CI contains no credentials/signing keys/personal
data. One app-level autostart and tray agent own helpers; quit stops them.
Watchdog uses bounded backoff; suspend/resume revalidates devices, network,
browser, and providers. Uninstall offers export/deletion.

Diagnostics report capability, audio, providers/models/OpenCode, connectors/
OAuth, permissions, DB/migration, browser, latency/resources/budgets, sanitized
support bundle, and version coherence.

CI covers Windows x64, macOS ARM64 plus Intel compile, and Ubuntu x64; Rust
format/lint/test; TypeScript lint/type/test; contract drift; dependencies,
licenses, secrets; and installer smoke. Performance targets: hotkey-to-listen
<100 ms, wake-to-listen <250 ms, speaker stop <50 ms, barge-in <100 ms,
offline deterministic command <500 ms, cloud EOS-to-audio p95 <2.5 s when
provider permits, idle CPU <1%, idle RSS <250 MB excluding models, warm UI
startup <3 s. Reports include p50/p95/max/count.

Required suites cover unit/property, contracts, crash consistency, migration,
OpenCode compatibility, fake/live providers, audio, platform automation,
browser fixtures/live, agent recovery, memory, automation, approval/rollback,
prompt injection/extensions, accessibility/visual, update/rollback,
suspend/device changes, and process/network/provider/disk chaos. A skipped test
must be recorded as platform-inapplicable.

## 9. Assumptions and exclusions

Desktop only; single owner plus guest/privacy sessions; upstream OpenCode not a
fork; useful local operation without a base-installer LLM; large verified
models optional; authorized APIs instead of prohibited scraping; face and
voice are not authorization; no spoken eval, committed secrets, invisible
scope widening, ads, or false consciousness; platform limitations are honest.
