# Threat model

## Assets

Credentials and OAuth tokens; private memory and transcripts; local files and
repositories; communications and identities; browser sessions; microphone,
camera, screen, and accessibility access; task state and approvals; money and
real-world commitments; audit integrity; update/install trust.

## Adversaries and failure sources

- Malicious or compromised web pages, documents, messages, connectors, MCP
  servers, skills, plugins, models, providers, and update infrastructure.
- A confused model, prompt injection, hallucinated targets, retry duplication,
  runaway loops, and compromised delegated agents.
- Another local process, a stolen remote token/device, filesystem tampering,
  accidental credential logging, and a user operating while distracted.
- Provider/network/process/disk/device failures and clock changes.

The single-owner model does not defend against an attacker who already has the
user’s fully unlocked OS account and equivalent desktop permissions. It still
limits accidental/model/extension effects and protects secrets at rest.

## Trust boundaries

1. UI to native core: requests are validated; UI state grants no authority.
2. Core to OpenCode/provider: per-run auth, loopback only, version pin, output
   normalized and treated as untrusted.
3. Runtime to tool gateway: effectful built-ins disabled; declared MCP tools
   cannot bypass native policy.
4. Trusted state/secrets/private memory to untrusted content: cross-zone
   effects ask or deny.
5. Workspace/desktop/isolated execution: mounts, network, tools, and inherited
   authority differ.
6. Core to connectors/plugins/browser: least scopes, revocation, provenance,
   process/WASI isolation, dedicated browser profiles.
7. Update/migration: signatures, backup/rollback, dry-run/confirmation, and
   immutable source.

## Required controls

- Exact JSON schemas and capability/precondition checks before policy.
- Tool risk and effect labels, task scopes, data zones, presence requirements,
  scoped consent, budget ceilings, and explicit always-confirm categories.
- Checkpoint before first mutation, verified postconditions, transactional
  rollback that snapshots current state, and idempotency/deduplication.
- Secret filtering and bounded output before runtime/UI; immutable audit and
  egress records without secret values.
- Guest/privacy profile separation; no voice/face authorization.
- Signed plugins, unsigned off, static preview, no renderer code, no core
  policy scope, out-of-process/WASI execution.
- Isolated browser profile by default, page generation handles, domain and
  subresource policy, download quarantine, takeover for authentication, and
  confirmation for commitments.
- Package/update signatures, checksums, SBOM, provenance, dependency/license/
  secret scans, database backup, and failed-health rollback.

## Residual risks

Accessibility and desktop tools ultimately act as the user; OS sandbox quality
differs by platform. Prompt injection cannot be perfectly classified. Hosted
STT/providers receive user data after explicit configuration. Wake words false
accept. Browser account takeover gives a page whatever the user grants during
takeover. Plugin and connector vulnerabilities remain possible within declared
scopes. Hardware/driver latency can miss audio targets.

Residual risk must appear in diagnostics and approval language. A feature that
cannot enforce its boundary is unavailable with a reason, not silently weaker.

## Verification

`AT-M5-POLICY-GATE`, `AT-M5-BROWSER-SAFE`, `AT-M5-ROLLBACK`,
`AT-M7-PLUGIN-SAFETY`, and `AT-M9-SECURITY` are release gates. Threat-model
review occurs on every new effect, data zone, connector scope, plugin runtime,
browser capability, remote protocol change, or update mechanism.
