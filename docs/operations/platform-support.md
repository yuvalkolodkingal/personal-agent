# Platform support matrix

| Capability | Windows | macOS | Linux |
| --- | --- | --- | --- |
| Desktop shell | Tauri/WebView2 | Tauri/WebKit | Tauri/WebKitGTK |
| Accessibility | UI Automation | Accessibility API | AT-SPI/portal |
| Screen | Graphics Capture | ScreenCaptureKit | xdg portal/PipeWire |
| Input | SendInput | Accessibility API | portal/wlroots/X11 |
| Audio/AEC | WASAPI | CoreAudio/VoiceProcessingIO | PipeWire/native modules |
| Secrets | Credential Manager | Keychain | Secret Service |
| Local control | user-SID named pipe | mode-0600 Unix socket | mode-0600 Unix socket |
| Packages | MSIX, NSIS | DMG, PKG | AppImage, DEB, RPM |
| Start at login | registry-backed entry | LaunchAgent | desktop autostart entry |
| Tray | native notification area | menu bar status item | AppIndicator-compatible tray |

This table names intended backends, not a claim that runtime permission is
already granted. Every adapter reports supported/degraded/unsupported with a
reason and remediation. Unknown compositors and projects default to safer
isolation. GNOME/KDE/wlroots differences are explicit; no nominal feature may
silently no-op.

The CI matrix defines native x64 and ARM64 jobs on Ubuntu, Windows, and macOS.
Those jobs fetch and checksum the matching OpenCode sidecar, execute the
authenticated sidecar compatibility test, run the Rust suite, and build an
unsigned Tauri development executable. Installer, desktop-permission,
distribution-specific, and physical audio/display validation remain milestone
gates rather than being inferred from compilation.

Autostart is off until the user enables it. Failure to create or inspect the
platform entry is returned to the settings UI as an actionable error; it is not
treated as success. Linux tray click events differ by desktop implementation,
so the context menu is the supported show/quit surface there.
