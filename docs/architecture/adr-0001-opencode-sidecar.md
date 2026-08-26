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

Evaluate V2 only behind the same boundary. It cannot become production default
while upstream labels it beta.

## Safety consequence

V1 `tool.execute.before` hooks cannot short-circuit a call, and recent upstream
reports show argument mutation inconsistencies. Therefore a hook is not the
safety boundary. Effectful built-ins are disabled; native Personal Agent MCP
tools provide the only effect path. Unknown permission requests fail closed.

Temporary upstream patches, if ever required, must be minimal, versioned,
linked to an upstream issue, and removed after compatibility verification.

## Sources

- <https://dev.opencode.ai/docs/server/>
- <https://opencode.ai/docs/providers>
- <https://opencode.ai/v2/docs/build/sdk>
- <https://github.com/anomalyco/opencode/releases/tag/v1.18.23>
- <https://github.com/anomalyco/opencode/issues/42409>
- <https://github.com/anomalyco/opencode/issues/32565>
