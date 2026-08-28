# Personal Agent MCP Manager

This crate is the policy-facing registry and lifecycle state machine for Model
Context Protocol servers. It deliberately does not execute MCP tools itself.
Successful tool preparation produces a `GatewayToolRequest` that must be sent
through Personal Agent's native `ToolGateway`, preserving approvals, execution
zones, output limits, egress policy, and audit logging.

Security invariants:

- Secret values are never represented by a server definition or export. Only
  OS-keychain references and OAuth grant references are serializable.
- Local installation, updates, rollback, and uninstall require consent bound to
  the exact displayed operation digest.
- Fixed values for credential-like environment variables and authorization
  headers are rejected.
- Remote HTTP uses HTTPS. Plain HTTP is accepted only for loopback endpoints.
- Imported credential values are discarded and surfaced as migration issues.
- Protocol/tool descriptions are untrusted display data and never interpreted
  as policy instructions.

The native host supplies small `RuntimeAdapter` and `PackageAdapter`
implementations for stdio/process and Streamable HTTP lifecycle operations.
This keeps platform process creation, keychain access, OAuth, and networking at
the native boundary while leaving state transitions deterministic and testable.
