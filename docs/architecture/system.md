# System architecture

## Trust topology

```text
React UI ── Tauri IPC ──► Rust Core ──► SQLCipher event store
                              │
                              ├──► Policy ──► approval / scoped consent
                              │        │
                              │        └──► Tool Gateway ──► native / connector effect
                              │
                              ├──► Agent supervisor ──► durable goals and task DAGs
                              ├──► Audio / Browser / Automation / Memory / Platform
                              └──► AgentRuntime
                                      │ authenticated loopback HTTP + SSE
                                      ▼
                                 OpenCode 1.18.23
                                      │ safety plugin + authenticated native bridge
                                      ▼
                                  Tool Gateway
```

The UI is untrusted for authorization. It can request a core handler and render
events but never owns a database handle, provider secret, OpenCode credential,
or native capability. The OpenCode endpoint and ephemeral basic-auth secret
remain inside `crates/runtime`.

Profile database keys live in macOS Keychain, Windows Credential Manager, or
Linux Secret Service. Configuration and IPC carry only
`keychain://service/account` references. The desktop opens SQLCipher after the
OS store returns the key, rebuilds its projection, then launches the bundled
sidecar asynchronously; normalized health is appended as another event.

OpenCode model output is untrusted. Filesystem and effectful built-ins are
disabled in both the agent tool map and permission configuration. The initial
native status tool uses an ephemeral loopback credential and exact registered
session/directory scope; subsequent native/MCP tools share the same gateway.
Every admitted tool executes only after native schema, scope, data-zone, policy,
consent, checkpoint, filtering, audit, and postcondition stages.

## State model

Domain events are append-only protobuf envelopes. A monotonic sequence is the
local ordering authority; wall-clock time is display/provenance metadata. UI
state is a projection and is disposable. Stream clients reconnect with the
last applied sequence. Additive events are ignored by older projections while
schema-major incompatibility fails explicitly.

Conversation controls are a separate deterministic state machine. Sleep, mute,
quiet, stop, guest, and follow-up are not aliases: mute turns capture off,
sleep leaves only wake-word privacy state, quiet suppresses spoken output, stop
is a transient foreground abort, and guest dispatches use restricted tools and
separate history scope. Typed messages are always silent and never activate the
microphone. General and per-project sessions survive persona/model switches.

SQLCipher is keyed before schema access. File-backed databases use WAL, full
synchronous writes, foreign keys, and a busy timeout. Migrations are
transactional. Large artifacts move to encrypted content-addressed storage;
the database stores hashes and metadata.

Legacy import has a separate review boundary. The renderer requests a
metadata-only plan; native code holds the plan behind a one-time review token
and rechecks its source fingerprint after explicit confirmation. Prepared
records never appear in logs or reports. SQLCipher atomically commits a
deterministic `migration_items` record and its domain materialization: history
as legacy-origin events, Markdown as provenance-bearing memory, safe settings,
disabled automations/connectors, and quarantined extension artifacts. Completed
content-free reports are recorded in `migration_runs` and written as private
JSON/Markdown files. Environment files, traces, auth, pairing keys, MCP
arguments/headers, symlinks, and unknown config fields do not cross the boundary.

## Execution and concurrency

The agent supervisor validates an acyclic graph before execution. Default
parallelism is three, hard configurable maximum eight, and default delegation
depth three. Each task receives explicit scopes, execution zone, workspace,
browser profile, budget, retry policy, and optional idempotency key. A child
never widens its parent authority. The foreground conversation lane preempts or
pauses background work.

Retries are automatic only when the declared operation is idempotent or a
deduplication key is enforced. Completion requires evidence for every success
criterion. Provider text is evidence input, never proof by itself.

## Platform boundaries

Platform adapters return `supported`, `degraded`, or `unsupported` with reason
and remediation. Runtime permission state refines compile-time support.
Accessibility handles are preferred over pixels. Screen-based actions bind to
a fresh snapshot generation, scale, bounds, and a verified postcondition.

The control API uses the same handlers as Tauri. Unix-like systems use a
mode-0600 socket; Windows uses a current-user-SID named pipe. Frames are
length-prefixed protobuf, bounded in size, and streams resume by sequence.
