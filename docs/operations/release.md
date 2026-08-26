# Release, update, export, and uninstall

Personal Agent produces unsigned development installers in CI for DEB, RPM, AppImage, NSIS, MSI, MSIX, macOS app, DMG, and PKG formats. Release signing and notarization are separate protected jobs because no private signing material belongs in the repository or ordinary pull-request CI.

Update metadata is canonical JSON signed with Ed25519. Native core verifies the manifest signature, exact target, HTTPS URL, byte length, and SHA-256 before accepting a download. Installation cannot begin until an encrypted SQLCipher backup has been recorded. A failed post-install health check enters the rollback state and restores the prior application/database pair.

Profile export writes a private mode-0600 JSON file atomically and refuses to overwrite an existing destination. Uninstall planning is non-destructive: keeping data is the default; deletion requires an explicit confirmed state, and any requested export must finish first.

Release artifacts include a deterministic CycloneDX SBOM in `release/sbom.cdx.json`, `THIRD_PARTY_NOTICES.md`, installer payload checks, source commit identity, and CI provenance. Run:

```sh
python scripts/generate-release-metadata.py --check
python scripts/security-gate.py
python scripts/verify-performance.py
```

External release credentials still required for public distribution are Apple Developer ID/notarization access and the Windows code-signing certificate. Their absence does not permit unsigned artifacts to claim a production trust status.
