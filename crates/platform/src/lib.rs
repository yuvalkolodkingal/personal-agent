//! Honest cross-platform capability reporting.

use serde::{Deserialize, Serialize};

/// Platform support is never represented as a silent no-op.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CapabilityStatus {
    Supported,
    Degraded {
        reason: String,
        remediation: Option<String>,
    },
    Unsupported {
        reason: String,
        remediation: Option<String>,
    },
}

/// Named native capability shown in diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlatformCapability {
    pub id: String,
    pub backend: String,
    pub status: CapabilityStatus,
}

/// Compile-target capability matrix. Runtime permission checks refine it.
#[must_use]
pub fn compile_time_capabilities() -> Vec<PlatformCapability> {
    let platform = std::env::consts::OS;
    let (desktop, capture, audio) = match platform {
        "macos" => (
            "Accessibility API",
            "ScreenCaptureKit",
            "CoreAudio/VoiceProcessingIO",
        ),
        "windows" => (
            "UI Automation/SendInput",
            "Windows Graphics Capture",
            "WASAPI",
        ),
        "linux" => ("AT-SPI/xdg-desktop-portal", "PipeWire portal", "PipeWire"),
        other => {
            return vec![PlatformCapability {
                id: "desktop".into(),
                backend: other.into(),
                status: CapabilityStatus::Unsupported {
                    reason: format!("{other} is not a supported desktop target"),
                    remediation: Some("Use Windows, macOS, or Linux".into()),
                },
            }];
        }
    };
    vec![
        PlatformCapability {
            id: "desktop.accessibility".into(),
            backend: desktop.into(),
            status: CapabilityStatus::Supported,
        },
        PlatformCapability {
            id: "screen.capture".into(),
            backend: capture.into(),
            status: CapabilityStatus::Supported,
        },
        PlatformCapability {
            id: "audio.duplex".into(),
            backend: audio.into(),
            status: CapabilityStatus::Supported,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_capability_has_a_backend_and_explicit_state() {
        for capability in compile_time_capabilities() {
            assert!(!capability.backend.is_empty());
            assert!(!capability.id.is_empty());
        }
    }
}
