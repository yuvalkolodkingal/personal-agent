//! Non-Linux XDG portal stub. Native Windows and macOS capability states remain
//! explicit rather than pretending the Linux portal exists.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

const UNSUPPORTED_REASON: &str = "XDG Desktop Portal is available only on Linux";
const UNSUPPORTED_REMEDIATION: &str =
    "Use the platform-native screen capture and input backends on Windows or macOS";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PortalSessionKind {
    ScreenCast,
    RemoteDesktop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PortalSessionPhase {
    Idle,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PortalConsentState {
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PortalInterfaces {
    pub screencast_version: Option<u32>,
    pub remote_desktop_version: Option<u32>,
    pub available_source_types: u32,
    pub available_cursor_modes: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PortalStream {
    pub node_id: u32,
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PortalStatus {
    pub interfaces: PortalInterfaces,
    pub phase: PortalSessionPhase,
    pub consent: PortalConsentState,
    pub kind: Option<PortalSessionKind>,
    pub streams: Vec<PortalStream>,
    pub pipewire_transport: bool,
    pub detail: String,
}

pub(crate) struct WaylandPortalManager;

impl WaylandPortalManager {
    #[must_use]
    pub(crate) fn live() -> Arc<Self> {
        Arc::new(Self)
    }

    fn unavailable() -> PortalStatus {
        PortalStatus {
            interfaces: PortalInterfaces {
                screencast_version: None,
                remote_desktop_version: None,
                available_source_types: 0,
                available_cursor_modes: 0,
            },
            phase: PortalSessionPhase::Idle,
            consent: PortalConsentState::Unavailable,
            kind: None,
            streams: Vec::new(),
            pipewire_transport: false,
            detail: format!("{UNSUPPORTED_REASON}. {UNSUPPORTED_REMEDIATION}"),
        }
    }

    #[must_use]
    pub(crate) fn status(&self) -> PortalStatus {
        Self::unavailable()
    }

    pub(crate) async fn probe(&self) -> PortalStatus {
        Self::unavailable()
    }

    pub(crate) async fn connect(&self, _: bool, _: &str) -> Result<PortalStatus, String> {
        Err(UNSUPPORTED_REASON.into())
    }

    pub(crate) async fn cancel(&self) -> PortalStatus {
        Self::unavailable()
    }

    pub(crate) async fn disconnect(&self) -> PortalStatus {
        Self::unavailable()
    }

    pub(crate) async fn notify_pointer_axis(&self, _: f64, _: f64) -> Result<(), String> {
        Err("XDG RemoteDesktop is available only on Linux".into())
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn status_reports_an_actionable_unsupported_state() {
        let status = WaylandPortalManager::live().status();

        assert_eq!(status.interfaces.screencast_version, None);
        assert_eq!(status.interfaces.remote_desktop_version, None);
        assert_eq!(status.phase, PortalSessionPhase::Idle);
        assert_eq!(status.consent, PortalConsentState::Unavailable);
        assert_eq!(
            status.detail,
            format!("{UNSUPPORTED_REASON}. {UNSUPPORTED_REMEDIATION}")
        );
    }
}
