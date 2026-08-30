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

## Publishing a GitHub release

`.github/workflows/release.yml` builds and attaches every installer. Cut one by
pushing a tag, or run the workflow manually with a tag input:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The workflow re-runs the full §14 gate first, so a tag can never publish a red
tree. It then builds, per target:

| Platform | Targets | Artifacts |
|---|---|---|
| Linux | `x86_64`, `aarch64` | `.deb`, `.rpm` (Fedora/RHEL), `.AppImage` |
| Arch Linux | `x86_64` | `.pkg.tar.zst` |
| Windows | `x86_64`, `aarch64` | `.msi`, `.exe` (NSIS) |
| macOS | Apple silicon, Intel | `.dmg`, `.app` |

The release is created as a **draft** so artifacts can be checked before it is
made public.

Tauri 2.11 ships no pacman bundler, so the Arch package is built separately by
`makepkg` inside an `archlinux:base-devel` container using
`packaging/arch/PKGBUILD`. It builds natively rather than repackaging the
Ubuntu-built binary, so it links against Arch's own webkit2gtk, and its
`package()` reuses the Debian payload Tauri already produces so the install
layout cannot drift between the two.

**Everything published this way is unsigned.** Windows SmartScreen will warn on
the NSIS and MSI installers, and macOS Gatekeeper will refuse the `.dmg` without
an explicit user override. Signing needs credentials that are deliberately not
in this repository: an Apple Developer ID with notarization access, and a
Windows code-signing certificate. Add them as repository secrets and extend the
`bundle` job before treating any artifact as production-trusted.
