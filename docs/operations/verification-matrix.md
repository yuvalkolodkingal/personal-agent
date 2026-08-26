# Verification matrix

| Area | Deterministic evidence | External evidence still required |
|---|---|---|
| Foundation | SQLCipher recovery, config/keychain rules, contracts, lifecycle tests, Linux DEB payload | Installed GUI launch on Windows/macOS/Linux |
| OpenCode | Pinned checksums, isolated profile, fake and synthetic streamed tool turns | User-configured real provider on each OS |
| Voice | Network-disabled local replay, wake voting, pre-roll, VAD, endpointing, stop budget | Licensed speech/WER, microphones, speakers, AEC and physical barge-in |
| Workspace | Typecheck/build and five interaction/accessibility tests | Cross-OS screenshot and screen-reader pass |
| Tools/browser | Policy, injection, quarantine, stale handles, checkpoints and rollback | Native accessibility/portal tasks and bundled Chromium live site |
| Agent/memory/automation | Four-hour virtual restart/provider chaos, effect receipts, review/conflict retrieval, missed runs | Multi-hour wall-clock soak with a real provider |
| Packs/remote | Ten manifests/evals, disabled connectors, exact fresh pairing grants | OAuth/live account interoperability |
| Migration | Co-located/split fixtures, idempotency, content-free reports, no canary transfer | Optional user-reviewed personal import |
| Release | Signed-metadata fixture, encrypted backup, rollback, export, uninstall plan, SBOM, 6,144-case mutation corpus | Signing/notarization, physical performance, cross-OS install/update/uninstall |

External cells must be reported as unavailable or pending; they are never silently promoted to passed.
