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

This table names intended backends, not a claim that runtime permission is
already granted. Every adapter reports supported/degraded/unsupported with a
reason and remediation. Unknown compositors and projects default to safer
isolation. GNOME/KDE/wlroots differences are explicit; no nominal feature may
silently no-op.
