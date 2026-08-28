//! Desktop automation backend discovery and conservative permission probes.
//!
//! This module describes native backends without calling unsafe platform APIs.
//! A higher layer may bind these plans to a separately audited native bridge.

use crate::{CapabilityStatus, PermissionState, PlatformCapability};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, env, path::Path};

/// Desktop family selected at build or host-probe time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopPlatform {
    Windows,
    MacOs,
    Linux,
    Unsupported,
}

/// Display-server/session family relevant to capture and input control.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSession {
    Windows,
    Aqua,
    Wayland,
    X11,
    Headless,
    Unknown,
}

/// Stable capability identifiers used by context and diagnostics.
pub mod capability_id {
    pub const ACTIVE_VIEW: &str = "desktop.active_view";
    pub const SCREEN_CAPTURE: &str = "desktop.screen_capture";
    pub const ACCESSIBILITY_TREE: &str = "desktop.accessibility_tree";
    pub const INPUT_CONTROL: &str = "desktop.input_control";
    pub const APP_LAUNCH: &str = "desktop.app_launch";
}

/// Probe inputs are explicit so support decisions can be contract-tested on any OS.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopProbeInput {
    pub operating_system: String,
    pub environment: BTreeMap<String, String>,
    pub available_executables: Vec<String>,
}

impl DesktopProbeInput {
    /// Inspect only non-secret session variables and executable presence.
    #[must_use]
    pub fn current() -> Self {
        let candidates = [
            "gio",
            "gtk-launch",
            "open",
            "explorer.exe",
            "gdbus",
            "busctl",
        ];
        Self {
            operating_system: env::consts::OS.to_owned(),
            environment: [
                "WAYLAND_DISPLAY",
                "DISPLAY",
                "XDG_CURRENT_DESKTOP",
                "DBUS_SESSION_BUS_ADDRESS",
                "AT_SPI_BUS_ADDRESS",
            ]
            .into_iter()
            .filter_map(|name| env::var(name).ok().map(|value| (name.to_owned(), value)))
            .collect(),
            available_executables: candidates
                .into_iter()
                .filter(|candidate| executable_on_path(candidate))
                .map(str::to_owned)
                .collect(),
        }
    }

    fn has_environment(&self, name: &str) -> bool {
        self.environment
            .get(name)
            .is_some_and(|value| !value.is_empty())
    }

    fn has_executable(&self, name: &str) -> bool {
        self.available_executables
            .iter()
            .any(|candidate| candidate == name)
    }
}

fn executable_on_path(program: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&paths).any(|directory| Path::new(&directory).join(program).is_file())
}

/// Native backend selection and its honest pre-connection status.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesktopBackendPlan {
    pub platform: DesktopPlatform,
    pub session: DesktopSession,
    pub screen_capture_backend: String,
    pub accessibility_backend: String,
    pub input_backend: String,
    pub launcher_backend: String,
    pub capabilities: Vec<PlatformCapability>,
}

impl DesktopBackendPlan {
    /// Look up one capability without relying on vector ordering.
    #[must_use]
    pub fn capability(&self, id: &str) -> Option<&PlatformCapability> {
        self.capabilities
            .iter()
            .find(|capability| capability.id == id)
    }
}

/// Determine the current desktop backend without claiming that permissions are granted.
#[must_use]
pub fn current_desktop_backend_plan() -> DesktopBackendPlan {
    probe_desktop_backend(&DesktopProbeInput::current())
}

/// Produce a deterministic backend plan from host signals.
#[must_use]
pub fn probe_desktop_backend(input: &DesktopProbeInput) -> DesktopBackendPlan {
    match input.operating_system.as_str() {
        "windows" => windows_plan(input),
        "macos" => macos_plan(input),
        "linux" => linux_plan(input),
        _ => unsupported_plan(input),
    }
}

fn capability(id: &str, backend: &str, status: CapabilityStatus) -> PlatformCapability {
    PlatformCapability {
        id: id.to_owned(),
        backend: backend.to_owned(),
        status,
    }
}

fn needs_bridge(reason: &str, remediation: &str) -> CapabilityStatus {
    CapabilityStatus::Degraded {
        reason: reason.to_owned(),
        remediation: Some(remediation.to_owned()),
    }
}

fn unavailable(reason: &str, remediation: &str) -> CapabilityStatus {
    CapabilityStatus::Unsupported {
        reason: reason.to_owned(),
        remediation: Some(remediation.to_owned()),
    }
}

fn windows_plan(input: &DesktopProbeInput) -> DesktopBackendPlan {
    let launch_status = if input.has_executable("explorer.exe") {
        CapabilityStatus::Supported
    } else {
        needs_bridge(
            "the Windows launcher was not found on PATH",
            "bind the native Windows host bridge",
        )
    };
    DesktopBackendPlan {
        platform: DesktopPlatform::Windows,
        session: DesktopSession::Windows,
        screen_capture_backend: "Windows Graphics Capture".into(),
        accessibility_backend: "Windows UI Automation".into(),
        input_backend: "UI Automation patterns / SendInput fallback".into(),
        launcher_backend: "Windows application activation".into(),
        capabilities: vec![
            capability(
                capability_id::ACTIVE_VIEW,
                "Windows UI Automation",
                needs_bridge(
                    "the safe Windows UIA host bridge is not connected",
                    "start the signed native bridge and grant requested access",
                ),
            ),
            capability(
                capability_id::SCREEN_CAPTURE,
                "Windows Graphics Capture",
                needs_bridge(
                    "capture requires a native bridge and user-selected capture target",
                    "connect the signed bridge and choose a window or display",
                ),
            ),
            capability(
                capability_id::ACCESSIBILITY_TREE,
                "Windows UI Automation",
                needs_bridge(
                    "UI Automation has not been connected",
                    "connect the signed Windows native bridge",
                ),
            ),
            capability(
                capability_id::INPUT_CONTROL,
                "UIA patterns / SendInput",
                needs_bridge(
                    "input control has not been connected and cannot cross Windows integrity levels",
                    "connect the bridge; run the agent at the same integrity level as the target",
                ),
            ),
            capability(
                capability_id::APP_LAUNCH,
                "Windows activation",
                launch_status,
            ),
        ],
    }
}

fn macos_plan(input: &DesktopProbeInput) -> DesktopBackendPlan {
    let launch_status = if input.has_executable("open") {
        CapabilityStatus::Supported
    } else {
        needs_bridge(
            "the macOS open launcher was not found on PATH",
            "bind NSWorkspace through the signed native bridge",
        )
    };
    DesktopBackendPlan {
        platform: DesktopPlatform::MacOs,
        session: DesktopSession::Aqua,
        screen_capture_backend: "ScreenCaptureKit".into(),
        accessibility_backend: "AXUIElement".into(),
        input_backend: "AX actions / CGEvent fallback".into(),
        launcher_backend: "NSWorkspace / open".into(),
        capabilities: vec![
            capability(
                capability_id::ACTIVE_VIEW,
                "AXUIElement",
                needs_bridge(
                    "Accessibility permission and the native bridge have not been probed",
                    "enable Accessibility for Personal Agent and connect the signed bridge",
                ),
            ),
            capability(
                capability_id::SCREEN_CAPTURE,
                "ScreenCaptureKit",
                needs_bridge(
                    "Screen Recording permission and the native bridge have not been probed",
                    "enable Screen Recording for Personal Agent and connect the signed bridge",
                ),
            ),
            capability(
                capability_id::ACCESSIBILITY_TREE,
                "AXUIElement",
                needs_bridge(
                    "Accessibility permission and the native bridge have not been probed",
                    "enable Accessibility for Personal Agent",
                ),
            ),
            capability(
                capability_id::INPUT_CONTROL,
                "AX actions / CGEvent",
                needs_bridge(
                    "input control requires Accessibility permission",
                    "enable Accessibility and connect the signed bridge",
                ),
            ),
            capability(
                capability_id::APP_LAUNCH,
                "NSWorkspace / open",
                launch_status,
            ),
        ],
    }
}

fn linux_plan(input: &DesktopProbeInput) -> DesktopBackendPlan {
    let wayland = input.has_environment("WAYLAND_DISPLAY");
    let x11 = input.has_environment("DISPLAY");
    let session = if wayland {
        DesktopSession::Wayland
    } else if x11 {
        DesktopSession::X11
    } else {
        DesktopSession::Headless
    };
    let session_bus = input.has_environment("DBUS_SESSION_BUS_ADDRESS");
    let has_dbus_client = input.has_executable("gdbus") || input.has_executable("busctl");
    let portal_available = session_bus && has_dbus_client;
    let signals = LinuxProbeSignals {
        session,
        session_bus,
        portal_available,
    };
    let accessibility_status = if session_bus {
        needs_bridge(
            "the AT-SPI bus is available but no accessibility bridge is connected",
            "enable the desktop accessibility bus and connect the AT-SPI adapter",
        )
    } else {
        unavailable(
            "no D-Bus user session is available for AT-SPI",
            "run Personal Agent inside a graphical user session",
        )
    };
    let capture_status = linux_capture_status(&signals);
    let input_status = linux_input_status(&signals);
    let launch_status = if input.has_executable("gio") || input.has_executable("gtk-launch") {
        CapabilityStatus::Supported
    } else {
        unavailable(
            "neither gio nor gtk-launch is available",
            "install GLib desktop utilities",
        )
    };
    DesktopBackendPlan {
        platform: DesktopPlatform::Linux,
        session,
        screen_capture_backend: if wayland {
            "XDG ScreenCast portal / PipeWire".into()
        } else {
            "X11 capture bridge / PipeWire".into()
        },
        accessibility_backend: "AT-SPI over D-Bus".into(),
        input_backend: if wayland {
            "AT-SPI / XDG RemoteDesktop portal".into()
        } else {
            "AT-SPI semantic actions".into()
        },
        launcher_backend: "GIO desktop activation".into(),
        capabilities: vec![
            capability(
                capability_id::ACTIVE_VIEW,
                "AT-SPI / desktop portal",
                accessibility_status.clone(),
            ),
            capability(
                capability_id::SCREEN_CAPTURE,
                if wayland {
                    "XDG ScreenCast portal / PipeWire"
                } else {
                    "X11 capture bridge"
                },
                capture_status,
            ),
            capability(
                capability_id::ACCESSIBILITY_TREE,
                "AT-SPI over D-Bus",
                accessibility_status,
            ),
            capability(
                capability_id::INPUT_CONTROL,
                if wayland {
                    "AT-SPI / XDG RemoteDesktop portal"
                } else {
                    "AT-SPI semantic actions"
                },
                input_status,
            ),
            capability(
                capability_id::APP_LAUNCH,
                "GIO desktop activation",
                launch_status,
            ),
        ],
    }
}

struct LinuxProbeSignals {
    session: DesktopSession,
    session_bus: bool,
    portal_available: bool,
}

fn linux_capture_status(signals: &LinuxProbeSignals) -> CapabilityStatus {
    if signals.session == DesktopSession::Wayland && signals.portal_available {
        needs_bridge(
            "the XDG ScreenCast portal is available but has not granted a session",
            "choose a screen or window in the system portal prompt",
        )
    } else if signals.session == DesktopSession::X11 {
        needs_bridge(
            "X11 capture requires the audited capture bridge",
            "connect the X11/PipeWire capture adapter",
        )
    } else {
        unavailable(
            "no graphical display session was detected",
            "run Personal Agent inside Wayland or X11",
        )
    }
}

fn linux_input_status(signals: &LinuxProbeSignals) -> CapabilityStatus {
    if signals.session == DesktopSession::Wayland && signals.portal_available {
        needs_bridge(
            "Wayland input requires a user-granted XDG RemoteDesktop portal session",
            "approve the RemoteDesktop portal prompt for the selected session",
        )
    } else if signals.session == DesktopSession::X11 && signals.session_bus {
        needs_bridge(
            "desktop control is limited to semantic AT-SPI actions until the adapter connects",
            "connect the AT-SPI adapter; avoid unrestricted global input injection",
        )
    } else {
        unavailable(
            "no supported graphical control session is available",
            "start a desktop session with AT-SPI or XDG RemoteDesktop support",
        )
    }
}

fn unsupported_plan(input: &DesktopProbeInput) -> DesktopBackendPlan {
    let status = unavailable(
        &format!(
            "{} is not a supported desktop platform",
            input.operating_system
        ),
        "use Windows, macOS, or Linux",
    );
    DesktopBackendPlan {
        platform: DesktopPlatform::Unsupported,
        session: DesktopSession::Unknown,
        screen_capture_backend: "unavailable".into(),
        accessibility_backend: "unavailable".into(),
        input_backend: "unavailable".into(),
        launcher_backend: "unavailable".into(),
        capabilities: [
            capability_id::ACTIVE_VIEW,
            capability_id::SCREEN_CAPTURE,
            capability_id::ACCESSIBILITY_TREE,
            capability_id::INPUT_CONTROL,
            capability_id::APP_LAUNCH,
        ]
        .into_iter()
        .map(|id| capability(id, "unavailable", status.clone()))
        .collect(),
    }
}

/// Fine-grained permission state supplied by a connected native bridge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesktopPermissionReport {
    pub screen_capture: PermissionState,
    pub accessibility: PermissionState,
    pub input_control: PermissionState,
}

/// Permission probes must never infer a grant from mere API availability.
pub trait DesktopPermissionProbe: Send + Sync {
    /// Return current OS authorization without triggering a permission prompt.
    fn probe_permissions(&self) -> DesktopPermissionReport;
}

/// Safe default used until a native host bridge supplies authoritative probes.
#[derive(Clone, Debug)]
pub struct ConservativePermissionProbe {
    plan: DesktopBackendPlan,
}

impl ConservativePermissionProbe {
    #[must_use]
    pub fn new(plan: DesktopBackendPlan) -> Self {
        Self { plan }
    }
}

impl DesktopPermissionProbe for ConservativePermissionProbe {
    fn probe_permissions(&self) -> DesktopPermissionReport {
        if self.plan.platform == DesktopPlatform::Unsupported
            || self.plan.session == DesktopSession::Headless
        {
            let reason = "native desktop permissions cannot be queried on this host".to_owned();
            return DesktopPermissionReport {
                screen_capture: PermissionState::Unavailable {
                    reason: reason.clone(),
                },
                accessibility: PermissionState::Unavailable {
                    reason: reason.clone(),
                },
                input_control: PermissionState::Unavailable { reason },
            };
        }
        DesktopPermissionReport {
            screen_capture: PermissionState::NotDetermined {
                guidance: "connect the native bridge and run the screen permission check".into(),
            },
            accessibility: PermissionState::NotDetermined {
                guidance: "connect the native bridge and run the accessibility permission check"
                    .into(),
            },
            input_control: PermissionState::NotDetermined {
                guidance: "connect the native bridge and run the input-control permission check"
                    .into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(os: &str, environment: &[(&str, &str)], executables: &[&str]) -> DesktopProbeInput {
        DesktopProbeInput {
            operating_system: os.into(),
            environment: environment
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect(),
            available_executables: executables.iter().map(|value| (*value).into()).collect(),
        }
    }

    #[test]
    fn linux_wayland_uses_portals_without_claiming_permission() {
        let plan = probe_desktop_backend(&input(
            "linux",
            &[
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1/bus"),
            ],
            &["gdbus", "gio"],
        ));
        assert_eq!(plan.session, DesktopSession::Wayland);
        assert_eq!(plan.platform, DesktopPlatform::Linux);
        assert!(plan.screen_capture_backend.contains("PipeWire"));
        assert!(matches!(
            plan.capability(capability_id::SCREEN_CAPTURE)
                .expect("capture")
                .status,
            CapabilityStatus::Degraded { .. }
        ));
        assert!(matches!(
            plan.capability(capability_id::APP_LAUNCH)
                .expect("launch")
                .status,
            CapabilityStatus::Supported
        ));
    }

    #[test]
    fn headless_linux_fails_closed() {
        let plan = probe_desktop_backend(&input("linux", &[], &["gio"]));
        assert_eq!(plan.session, DesktopSession::Headless);
        for id in [
            capability_id::ACTIVE_VIEW,
            capability_id::SCREEN_CAPTURE,
            capability_id::ACCESSIBILITY_TREE,
            capability_id::INPUT_CONTROL,
        ] {
            assert!(matches!(
                plan.capability(id).expect("capability").status,
                CapabilityStatus::Unsupported { .. }
            ));
        }
    }

    #[test]
    fn platform_specs_name_required_native_apis() {
        let windows = probe_desktop_backend(&input("windows", &[], &["explorer.exe"]));
        assert!(windows.screen_capture_backend.contains("Windows Graphics"));
        assert!(windows.accessibility_backend.contains("UI Automation"));
        assert!(windows.input_backend.contains("SendInput"));

        let macos = probe_desktop_backend(&input("macos", &[], &["open"]));
        assert!(macos.screen_capture_backend.contains("ScreenCaptureKit"));
        assert!(macos.accessibility_backend.contains("AXUIElement"));
        assert!(macos.input_backend.contains("CGEvent"));
    }

    #[test]
    fn conservative_probe_never_invents_a_permission_grant() {
        let plan = probe_desktop_backend(&input("macos", &[], &["open"]));
        let report = ConservativePermissionProbe::new(plan).probe_permissions();
        assert!(matches!(
            report.screen_capture,
            PermissionState::NotDetermined { .. }
        ));
        assert!(matches!(
            report.accessibility,
            PermissionState::NotDetermined { .. }
        ));
        assert!(matches!(
            report.input_control,
            PermissionState::NotDetermined { .. }
        ));
    }
}
