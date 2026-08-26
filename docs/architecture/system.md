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
                                      │ Personal Agent MCP only for effects
                                      ▼
                                  Tool Gateway
```

The UI is untrusted for authorization. It can request a core handler and render
events but never owns a database handle, provider secret, OpenCode credential,
or native capability. The OpenCode endpoint and ephemeral basic-auth secret
remain inside `crates/runtime`.

OpenCode model output is untrusted. Effectful built-ins are disabled in both
the agent tool map and permission configuration. Personal Agent tools are
declared over MCP and execute only after native schema, scope, data-zone,
policy, consent, checkpoint, filtering, audit, and postcondition stages.

## State model

Domain events are append-only protobuf envelopes. A monotonic sequence is the
local ordering authority; wall-clock time is display/provenance metadata. UI
state is a projection and is disposable. Stream clients reconnect with the
last applied sequence. Additive events are ignored by older projections while
schema-major incompatibility fails explicitly.

SQLCipher is keyed before schema access. File-backed databases use WAL, full
synchronous writes, foreign keys, and a busy timeout. Migrations are
transactional. Large artifacts move to encrypted content-addressed storage;
the database stores hashes and metadata.

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
