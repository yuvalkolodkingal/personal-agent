# ADR-0001: Pinned OpenCode sidecar behind AgentRuntime

- Status: accepted
- Date: 2026-08-26
- Decision owners: Personal Agent maintainers

## Context

OpenCode provides broad provider support, local/custom providers, a protected
server, OpenAPI, SSE, agents, skills, and MCP. Its embedded V2 SDK is still
documented as beta. A permanent fork would create a security and maintenance
liability.

The current stable release checked from the official GitHub release API is
1.18.23, published 2026-08-25. The server supports loopback binding and basic
auth through `OPENCODE_SERVER_PASSWORD`.

## Decision

Bundle exact OpenCode 1.18.23 artifacts with per-platform checksums. Start it
on loopback with an ephemeral per-run credential and owned lifetime. Generate
the network client from `/doc`, normalize SSE into Personal Agent events, and
expose nothing OpenCode-specific above `AgentRuntime`.

The artifact fetcher selects the target triple, verifies the release archive's
SHA-256 before extraction, checks the executable's reported version, and feeds
Tauri `externalBin`; neither archives nor binaries enter Git. Runtime startup
independently probes `--version`, requires the exact pin, and waits for an
authenticated `/api/health` response. The CI matrix is configured to execute
this path on every supported OS/architecture runner.

Startup also hashes the authenticated `/doc` response and compares it to the
contract fingerprint recorded with the release artifact manifest. A matching
version string with a changed API document is rejected before sessions begin.
The reviewed response is projected deterministically into the routes and
transitive schemas Personal Agent consumes, then compiled into the Rust client.
CI checks both the source fingerprint and generated projection for drift.

The production adapter uses the pinned stable routes for directory-scoped API
calls, provider discovery, session lifecycle, prompt admission, permissions,
and questions. It
passes the chosen model and agent explicitly on each prompt so upstream defaults
cannot silently change a resumed session. V2 remains outside the production
path. SSE framing is handled at the boundary because it is a streaming
transport: chunk boundaries and CRLF are normalized, raw tool inputs/results
are discarded, and reasoning text is represented only as an availability
signal.

Evaluate V2 only behind the same boundary. It cannot become production default
while upstream labels it beta. The M2 compatibility suite includes an isolated
OpenAI-compatible provider and a harmless native-gateway status turn through
the real stable sidecar, without reading a user's OpenCode profile.

The bundled safety plugin is a legacy-compatible module with exactly one
runtime export: its default plugin function. OpenCode 1.18.23 interprets every
runtime export from a legacy module as a plugin entry, so exporting constants
would reject the whole module before hooks register. Unit tests lock that
module shape, the real-sidecar fixture inspects the provider request tool names,
and installer smoke checks compare the packaged plugin byte-for-byte with the
reviewed source.

Both the version probe and server run with a cleared child environment. The
adapter restores only a narrow allowlist needed for OS mechanics and redirects
home, XDG, Windows application-data, temporary, and explicit OpenCode config
paths into a mode-0700 application-owned profile. Ambient provider credentials,
proxy variables, package-manager configuration, global plugins, and home-level
`.opencode` content are therefore not adopted implicitly. Provider onboarding
must use native secret storage and explicit user consent.

Project-local `opencode.json` and `.opencode` configuration are disabled. The
native adapter supplies only provider definitions admitted by onboarding. The
plugin exposes one initial `personal_agent_gateway_status` vertical slice over
an ephemeral authenticated loopback channel. Rust accepts it only when the
session ID and canonical working directory match its registry, then runs the
normal native schema, policy, output filtering, verification, and audit path.
OpenCode filesystem built-ins are disabled, so this compatibility turn cannot
fall back to reading a fixture directly.

## Safety consequence

V1 `tool.execute.before` hooks cannot short-circuit a call, and recent upstream
reports show argument mutation inconsistencies. Therefore a hook is not the
safety boundary. Filesystem and effectful built-ins are disabled; native
Personal Agent gateway tools provide the only admitted path. Unknown permission
requests fail closed.

Temporary upstream patches, if ever required, must be minimal, versioned,
linked to an upstream issue, and removed after compatibility verification.

## Sources

- <https://dev.opencode.ai/docs/server/>
- <https://opencode.ai/docs/providers>
- <https://opencode.ai/v2/docs/build/sdk>
- <https://github.com/anomalyco/opencode/releases/tag/v1.18.23>
- <https://github.com/anomalyco/opencode/issues/42409>
- <https://github.com/anomalyco/opencode/issues/32565>
