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

All repository-local gates pass as of 2026-08-26. No known critical or high-severity finding is open in the implemented code. `cargo audit`, `cargo deny`, and gitleaks run in the protected Security workflow; their first private-GitHub run is required before a release tag. Live browser, OS automation, microphone/speaker, connector, and provider red teams remain environment-gated and cannot be replaced by fixture claims.
