# Red-team and security gate report

The deterministic red-team suite covers:

- untrusted page instructions attempting secret, connector, download, or real-world submission effects;
- stale browser-handle reuse and non-HTTP navigation;
- personal browser profile use without opt-in;
- unsigned plugins, arbitrary renderer code, and core-policy rewrite scopes;
- remote clients requesting unoffered capabilities or replaying pairing proofs;
- hosted speech adapters attempting to run during network-disabled operation;
- consequential task retry without an idempotency receipt;
- rollback without a rescue checkpoint;
- legacy import credential canaries, symlinks, malformed records, and source mutation;
- tampered update payloads and signed manifests;
- repository credential patterns, unsafe CSP widening, pack authorization defaults, and generated-SBOM drift.

A bounded mutation corpus also changes 2,048 inputs each for signed release
metadata, official pack manifests, and secret-shaped tool output. Parsers must
fail closed, validated packs must retain disabled/keychain-only authority, and
no secret shape may survive output filtering.

All repository-local gates pass as of 2026-08-26. `cargo audit 0.22.2`
reported no vulnerability failure; its seventeen maintenance/unsoundness warnings
are confined to Tauri's transitive Linux GTK3/WebKitGTK stack and are individually
documented in `deny.toml`. `cargo deny 0.20.2` passes the advisory, compatible
license, ban, and source policy (duplicate transitive versions remain warnings),
and gitleaks 8.30.1 found no leak across both commits then present. No known
critical or high-severity finding is open in the implemented code.

The protected Security workflow remains required before a release tag; its first
private-GitHub run was rejected before step one by the account billing/spending
gate. Live browser, OS automation, microphone/speaker, connector, and provider
red teams remain environment-gated and cannot be replaced by fixture claims.
