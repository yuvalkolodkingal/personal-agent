//! Replaceable native bridge contracts and platform adapter specifications.

use crate::{
    AccessibilityNode, ActiveView, CaptureScope, CapturedFrame, DesktopAction, Rect,
    SnapshotGeneration,
};
use async_trait::async_trait;
use personal_agent_platform::desktop::{
    DesktopBackendPlan, DesktopPermissionReport, DesktopPlatform, current_desktop_backend_plan,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Active-view result returned before accessibility content is requested.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveViewObservation {
    pub generation: SnapshotGeneration,
    pub observed_at_unix_ms: u64,
    pub view: ActiveView,
}

/// Non-sensitive evidence emitted by a native action implementation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeActionEvidence {
    pub backend_operation: String,
    pub native_target_id: Option<String>,
    pub changed: bool,
}

/// Backend identity and live authorization state shown in diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesktopBackendStatus {
    pub plan: DesktopBackendPlan,
    pub connected: bool,
    pub permissions: DesktopPermissionReport,
    pub connection_detail: String,
}

/// Failure at an OS/native bridge boundary.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BackendError {
    #[error("desktop backend is unavailable: {0}")]
    Unavailable(String),
    #[error("desktop permission is not granted: {0}")]
    PermissionDenied(String),
    #[error("native bridge disconnected: {0}")]
    Disconnected(String),
    #[error("native adapter returned invalid data: {0}")]
    InvalidData(String),
    #[error("native desktop operation failed: {0}")]
    Operation(String),
}

/// Minimal active-window boundary implemented by UIA, `AXUIElement`, or AT-SPI.
#[async_trait]
pub trait ActiveViewAdapter: Send + Sync {
    async fn active_view(&self) -> Result<ActiveViewObservation, BackendError>;
}

/// Semantic accessibility-tree boundary.
#[async_trait]
pub trait AccessibilityAdapter: Send + Sync {
    async fn accessibility_nodes(
        &self,
        view: &ActiveView,
        generation: SnapshotGeneration,
    ) -> Result<Vec<AccessibilityNode>, BackendError>;
}

/// Ephemeral screen-frame boundary. Implementations must apply OS picker scope
/// and the caller's redaction regions before returning pixels.
#[async_trait]
pub trait ScreenCaptureAdapter: Send + Sync {
    async fn capture_frame(
        &self,
        scope: &CaptureScope,
        generation: SnapshotGeneration,
        redacted_regions: &[Rect],
    ) -> Result<CapturedFrame, BackendError>;
}

/// Effectful native desktop-control boundary.
#[async_trait]
pub trait DesktopControlAdapter: Send + Sync {
    async fn execute_native(
        &self,
        action: &DesktopAction,
        generation: SnapshotGeneration,
    ) -> Result<NativeActionEvidence, BackendError>;
}

/// Complete backend accepted by the verified coordinator.
pub trait DesktopBackend:
    ActiveViewAdapter + AccessibilityAdapter + ScreenCaptureAdapter + DesktopControlAdapter
{
    fn status(&self) -> DesktopBackendStatus;
}

/// Safe native host boundary. The application may implement this through a
/// separately signed Swift/Objective-C, Windows, or D-Bus helper. This crate
/// deliberately contains no `unsafe` FFI and never falls back to an implicit no-op.
#[async_trait]
pub trait NativeDesktopBridge: Send + Sync {
    fn is_connected(&self) -> bool;
    fn permission_report(&self) -> DesktopPermissionReport;
    fn connection_detail(&self) -> String;

    async fn active_view(&self) -> Result<ActiveViewObservation, BackendError>;
    async fn accessibility_nodes(
        &self,
        view: &ActiveView,
        generation: SnapshotGeneration,
    ) -> Result<Vec<AccessibilityNode>, BackendError>;
    async fn capture_frame(
        &self,
        scope: &CaptureScope,
        generation: SnapshotGeneration,
        redacted_regions: &[Rect],
    ) -> Result<CapturedFrame, BackendError>;
    async fn execute_native(
        &self,
        action: &DesktopAction,
        generation: SnapshotGeneration,
    ) -> Result<NativeActionEvidence, BackendError>;
}

/// Cross-platform adapter that binds normalized contracts to one native bridge.
pub struct BridgeDesktopBackend<B> {
    bridge: B,
    plan: DesktopBackendPlan,
}

impl<B> BridgeDesktopBackend<B> {
    /// Bind a bridge to the plan for the current platform/session.
    #[must_use]
    pub fn current(bridge: B) -> Self {
        Self {
            bridge,
            plan: current_desktop_backend_plan(),
        }
    }

    /// Bind an explicit plan. Primarily useful for native-host composition and tests.
    #[must_use]
    pub fn with_plan(bridge: B, plan: DesktopBackendPlan) -> Self {
        Self { bridge, plan }
    }

    /// Windows Graphics Capture + UI Automation + `SendInput` adapter contract.
    ///
    /// # Errors
    ///
    /// Rejects a plan for another platform.
    pub fn windows(bridge: B, plan: DesktopBackendPlan) -> Result<Self, BackendError> {
        Self::for_platform(bridge, plan, DesktopPlatform::Windows)
    }

    /// `ScreenCaptureKit` + `AXUIElement` + `CGEvent` adapter contract.
    ///
    /// # Errors
    ///
    /// Rejects a plan for another platform.
    pub fn macos(bridge: B, plan: DesktopBackendPlan) -> Result<Self, BackendError> {
        Self::for_platform(bridge, plan, DesktopPlatform::MacOs)
    }

    /// XDG portals/PipeWire + AT-SPI adapter contract.
    ///
    /// # Errors
    ///
    /// Rejects a plan for another platform.
    pub fn linux(bridge: B, plan: DesktopBackendPlan) -> Result<Self, BackendError> {
        Self::for_platform(bridge, plan, DesktopPlatform::Linux)
    }

    fn for_platform(
        bridge: B,
        plan: DesktopBackendPlan,
        expected: DesktopPlatform,
    ) -> Result<Self, BackendError> {
        if plan.platform != expected {
            return Err(BackendError::InvalidData(format!(
                "expected {expected:?} plan, received {:?}",
                plan.platform
            )));
        }
        Ok(Self { bridge, plan })
    }

    #[must_use]
    pub fn bridge(&self) -> &B {
        &self.bridge
    }
}

/// Explicit aliases make platform composition self-documenting.
pub type WindowsDesktopAdapter<B> = BridgeDesktopBackend<B>;
pub type MacOsDesktopAdapter<B> = BridgeDesktopBackend<B>;
pub type LinuxDesktopAdapter<B> = BridgeDesktopBackend<B>;

#[async_trait]
impl<B: NativeDesktopBridge> ActiveViewAdapter for BridgeDesktopBackend<B> {
    async fn active_view(&self) -> Result<ActiveViewObservation, BackendError> {
        ensure_connected(&self.bridge)?;
        self.bridge.active_view().await
    }
}

#[async_trait]
impl<B: NativeDesktopBridge> AccessibilityAdapter for BridgeDesktopBackend<B> {
    async fn accessibility_nodes(
        &self,
        view: &ActiveView,
        generation: SnapshotGeneration,
    ) -> Result<Vec<AccessibilityNode>, BackendError> {
        ensure_connected(&self.bridge)?;
        self.bridge.accessibility_nodes(view, generation).await
    }
}

#[async_trait]
impl<B: NativeDesktopBridge> ScreenCaptureAdapter for BridgeDesktopBackend<B> {
    async fn capture_frame(
        &self,
        scope: &CaptureScope,
        generation: SnapshotGeneration,
        redacted_regions: &[Rect],
    ) -> Result<CapturedFrame, BackendError> {
        ensure_connected(&self.bridge)?;
        self.bridge
            .capture_frame(scope, generation, redacted_regions)
            .await
    }
}

#[async_trait]
impl<B: NativeDesktopBridge> DesktopControlAdapter for BridgeDesktopBackend<B> {
    async fn execute_native(
        &self,
        action: &DesktopAction,
        generation: SnapshotGeneration,
    ) -> Result<NativeActionEvidence, BackendError> {
        ensure_connected(&self.bridge)?;
        self.bridge.execute_native(action, generation).await
    }
}

impl<B: NativeDesktopBridge> DesktopBackend for BridgeDesktopBackend<B> {
    fn status(&self) -> DesktopBackendStatus {
        DesktopBackendStatus {
            plan: self.plan.clone(),
            connected: self.bridge.is_connected(),
            permissions: self.bridge.permission_report(),
            connection_detail: self.bridge.connection_detail(),
        }
    }
}

fn ensure_connected(bridge: &impl NativeDesktopBridge) -> Result<(), BackendError> {
    if bridge.is_connected() {
        Ok(())
    } else {
        Err(BackendError::Disconnected(bridge.connection_detail()))
    }
}

/// Fail-closed bridge used until the OS integration process is connected.
#[derive(Clone, Debug)]
pub struct UnavailableNativeBridge {
    reason: String,
    permissions: DesktopPermissionReport,
}

impl UnavailableNativeBridge {
    #[must_use]
    pub fn new(reason: impl Into<String>, permissions: DesktopPermissionReport) -> Self {
        Self {
            reason: reason.into(),
            permissions,
        }
    }

    fn error(&self) -> BackendError {
        BackendError::Unavailable(self.reason.clone())
    }
}

#[async_trait]
impl NativeDesktopBridge for UnavailableNativeBridge {
    fn is_connected(&self) -> bool {
        false
    }

    fn permission_report(&self) -> DesktopPermissionReport {
        self.permissions.clone()
    }

    fn connection_detail(&self) -> String {
        self.reason.clone()
    }

    async fn active_view(&self) -> Result<ActiveViewObservation, BackendError> {
        Err(self.error())
    }

    async fn accessibility_nodes(
        &self,
        _view: &ActiveView,
        _generation: SnapshotGeneration,
    ) -> Result<Vec<AccessibilityNode>, BackendError> {
        Err(self.error())
    }

    async fn capture_frame(
        &self,
        _scope: &CaptureScope,
        _generation: SnapshotGeneration,
        _redacted_regions: &[Rect],
    ) -> Result<CapturedFrame, BackendError> {
        Err(self.error())
    }

    async fn execute_native(
        &self,
        _action: &DesktopAction,
        _generation: SnapshotGeneration,
    ) -> Result<NativeActionEvidence, BackendError> {
        Err(self.error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_agent_platform::{PermissionState, desktop::DesktopProbeInput};
    use std::collections::BTreeMap;

    fn denied_permissions() -> DesktopPermissionReport {
        DesktopPermissionReport {
            screen_capture: PermissionState::Denied {
                guidance: "enable screen recording".into(),
            },
            accessibility: PermissionState::Denied {
                guidance: "enable accessibility".into(),
            },
            input_control: PermissionState::Denied {
                guidance: "enable accessibility".into(),
            },
        }
    }

    #[tokio::test]
    async fn disconnected_bridge_returns_error_instead_of_empty_context() {
        let plan = personal_agent_platform::desktop::probe_desktop_backend(&DesktopProbeInput {
            operating_system: "linux".into(),
            environment: BTreeMap::default(),
            available_executables: vec![],
        });
        let backend = BridgeDesktopBackend::with_plan(
            UnavailableNativeBridge::new("bridge not started", denied_permissions()),
            plan,
        );
        assert!(!backend.status().connected);
        assert!(matches!(
            backend.active_view().await,
            Err(BackendError::Disconnected(_))
        ));
    }

    #[test]
    fn platform_adapter_constructor_rejects_mismatched_native_plan() {
        let plan = personal_agent_platform::desktop::probe_desktop_backend(&DesktopProbeInput {
            operating_system: "linux".into(),
            environment: BTreeMap::default(),
            available_executables: vec![],
        });
        let bridge = UnavailableNativeBridge::new("fixture", denied_permissions());
        assert!(matches!(
            BridgeDesktopBackend::windows(bridge, plan),
            Err(BackendError::InvalidData(_))
        ));
    }
}
