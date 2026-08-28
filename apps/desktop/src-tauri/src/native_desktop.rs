//! Safe command-backed native bridge used by the desktop host.
//!
//! The audited contract stays in `personal-agent-context`; this bridge uses
//! platform utilities without shell interpolation and reports degraded support
//! whenever a native API helper or permission is absent.

use async_trait::async_trait;
use personal_agent_context::{
    AccessibilityNode, ActiveView, ActiveViewObservation, BackendError, CaptureScope,
    CapturedFrame, DesktopAction, NativeActionEvidence, NativeDesktopBridge, NodeAction,
    NodeHandle, NodeState, PixelFormat, Rect, ScreenFrameDescriptor, SemanticRole,
    SnapshotGeneration, WindowId,
};
use personal_agent_platform::PermissionState;
use personal_agent_platform::desktop::DesktopPermissionReport;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::process::Command;

pub(crate) struct CommandNativeBridge {
    sequence: AtomicU64,
    permissions: DesktopPermissionReport,
    connected: bool,
    detail: String,
}

impl CommandNativeBridge {
    #[must_use]
    pub(crate) fn discover() -> Self {
        let platform = env::consts::OS;
        let (connected, detail, permissions) = match platform {
            "linux" => {
                let active = executable("hyprctl") || executable("xdotool");
                let capture = executable("grim") || executable("gnome-screenshot");
                let input = executable("wtype") || executable("ydotool") || executable("xdotool");
                (
                    active,
                    if active {
                        "Linux active-window command bridge connected".into()
                    } else {
                        "install hyprctl or xdotool for active-window context".into()
                    },
                    DesktopPermissionReport {
                        accessibility: capability_permission(active, "install hyprctl or xdotool"),
                        screen_capture: capability_permission(
                            capture,
                            "install grim or gnome-screenshot and grant screen capture",
                        ),
                        input_control: capability_permission(
                            input,
                            "install/authorize wtype, ydotool, or xdotool",
                        ),
                    },
                )
            }
            "macos" => {
                let active = executable("osascript");
                (
                    active,
                    "macOS Apple Events command bridge; Accessibility and Screen Recording remain OS-gated".into(),
                    DesktopPermissionReport {
                        accessibility: capability_permission(active, "grant Accessibility permission"),
                        screen_capture: capability_permission(executable("screencapture"), "grant Screen Recording permission"),
                        input_control: capability_permission(active, "grant Accessibility permission"),
                    },
                )
            }
            "windows" => {
                let active = executable("powershell.exe") || executable("pwsh.exe");
                (
                    active,
                    "Windows PowerShell native API bridge connected at the current integrity level"
                        .into(),
                    DesktopPermissionReport {
                        accessibility: capability_permission(
                            active,
                            "enable the Windows native helper",
                        ),
                        screen_capture: capability_permission(
                            active,
                            "enable screen capture for Personal Agent",
                        ),
                        input_control: capability_permission(
                            active,
                            "run the agent at the same integrity level as the target",
                        ),
                    },
                )
            }
            _ => (
                false,
                format!("{platform} has no desktop bridge"),
                DesktopPermissionReport {
                    accessibility: PermissionState::Unavailable {
                        reason: "unsupported platform".into(),
                    },
                    screen_capture: PermissionState::Unavailable {
                        reason: "unsupported platform".into(),
                    },
                    input_control: PermissionState::Unavailable {
                        reason: "unsupported platform".into(),
                    },
                },
            ),
        };
        Self {
            sequence: AtomicU64::new(0),
            permissions,
            connected,
            detail,
        }
    }

    fn next_generation(&self) -> SnapshotGeneration {
        SnapshotGeneration {
            epoch: 1,
            sequence: self.sequence.fetch_add(1, Ordering::SeqCst) + 1,
        }
    }

    async fn observe(&self) -> Result<ActiveView, BackendError> {
        match env::consts::OS {
            "linux" if executable("hyprctl") => observe_hyprland().await,
            "linux" if executable("xdotool") => observe_x11().await,
            "macos" => observe_macos().await,
            "windows" => observe_windows().await,
            platform => Err(BackendError::Unavailable(format!(
                "active-view adapter unavailable on {platform}"
            ))),
        }
    }
}

fn capability_permission(available: bool, guidance: &str) -> PermissionState {
    if available {
        PermissionState::Granted
    } else {
        PermissionState::Unavailable {
            reason: guidance.into(),
        }
    }
}

fn executable(program: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&paths).any(|directory| Path::new(&directory).join(program).is_file())
}

async fn output(program: &str, args: &[&str]) -> Result<Vec<u8>, BackendError> {
    let result = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| BackendError::Operation(error.to_string()))?;
    if result.status.success() {
        Ok(result.stdout)
    } else {
        Err(BackendError::Operation(
            String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        ))
    }
}

async fn observe_hyprland() -> Result<ActiveView, BackendError> {
    let bytes = output("hyprctl", &["activewindow", "-j"]).await?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| BackendError::InvalidData(error.to_string()))?;
    let pair = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_array)
            .filter(|items| items.len() == 2)
            .map(|items| {
                (
                    items[0].as_f64().unwrap_or_default(),
                    items[1].as_f64().unwrap_or_default(),
                )
            })
    };
    let bounds = pair("at")
        .zip(pair("size"))
        .map(|((x, y), (width, height))| Rect {
            x,
            y,
            width,
            height,
        });
    let application_id = value
        .get("class")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let address = value
        .get("address")
        .and_then(Value::as_str)
        .unwrap_or(&application_id)
        .to_owned();
    Ok(ActiveView {
        application_name: application_id.clone(),
        application_id,
        process_id: value
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
        window_id: WindowId(address),
        title: title.clone(),
        bounds,
        focused_node: None,
        secure_surface: secure_title(&title),
    })
}

async fn observe_x11() -> Result<ActiveView, BackendError> {
    let id = String::from_utf8_lossy(&output("xdotool", &["getactivewindow"]).await?)
        .trim()
        .to_owned();
    let title = String::from_utf8_lossy(&output("xdotool", &["getwindowname", &id]).await?)
        .trim()
        .to_owned();
    Ok(ActiveView {
        application_id: "x11-window".into(),
        application_name: "X11 application".into(),
        process_id: None,
        window_id: WindowId(id),
        title: title.clone(),
        bounds: None,
        focused_node: None,
        secure_surface: secure_title(&title),
    })
}

async fn observe_macos() -> Result<ActiveView, BackendError> {
    let script = "tell application \"System Events\" to tell first application process whose frontmost is true to return (bundle identifier as text) & \"|\" & (name as text) & \"|\" & (unix id as text) & \"|\" & (name of front window as text)";
    let text = String::from_utf8_lossy(&output("osascript", &["-e", script]).await?).to_string();
    let parts = text.trim().splitn(4, '|').collect::<Vec<_>>();
    if parts.len() != 4 {
        return Err(BackendError::InvalidData(
            "Apple Events returned invalid active-window data".into(),
        ));
    }
    Ok(ActiveView {
        application_id: parts[0].into(),
        application_name: parts[1].into(),
        process_id: parts[2].parse().ok(),
        window_id: WindowId(format!("{}:{}", parts[0], parts[3])),
        title: parts[3].into(),
        bounds: None,
        focused_node: None,
        secure_surface: secure_title(parts[3]),
    })
}

async fn observe_windows() -> Result<ActiveView, BackendError> {
    let shell = if executable("pwsh.exe") {
        "pwsh.exe"
    } else {
        "powershell.exe"
    };
    let script = "Add-Type @'\nusing System; using System.Runtime.InteropServices; public class W { [DllImport(\"user32.dll\")] public static extern IntPtr GetForegroundWindow(); [DllImport(\"user32.dll\", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int n); }\n'@; $h=[W]::GetForegroundWindow(); $s=New-Object Text.StringBuilder 1024; [void][W]::GetWindowText($h,$s,1024); \"$h|$($s.ToString())\"";
    let text = String::from_utf8_lossy(&output(shell, &["-NoProfile", "-Command", script]).await?)
        .to_string();
    let (id, title) = text.trim().split_once('|').ok_or_else(|| {
        BackendError::InvalidData("Windows returned invalid active-window data".into())
    })?;
    Ok(ActiveView {
        application_id: "windows-application".into(),
        application_name: "Windows application".into(),
        process_id: None,
        window_id: WindowId(id.into()),
        title: title.into(),
        bounds: None,
        focused_node: None,
        secure_surface: secure_title(title),
    })
}

fn secure_title(title: &str) -> bool {
    let title = title.to_ascii_lowercase();
    [
        "password",
        "keychain",
        "credential",
        "authentication",
        "lock screen",
    ]
    .iter()
    .any(|word| title.contains(word))
}

#[async_trait]
impl NativeDesktopBridge for CommandNativeBridge {
    fn is_connected(&self) -> bool {
        self.connected
    }

    fn permission_report(&self) -> DesktopPermissionReport {
        self.permissions.clone()
    }

    fn connection_detail(&self) -> String {
        self.detail.clone()
    }

    async fn active_view(&self) -> Result<ActiveViewObservation, BackendError> {
        Ok(ActiveViewObservation {
            generation: self.next_generation(),
            observed_at_unix_ms: u64::try_from(chrono::Utc::now().timestamp_millis())
                .unwrap_or_default(),
            view: self.observe().await?,
        })
    }

    async fn accessibility_nodes(
        &self,
        view: &ActiveView,
        generation: SnapshotGeneration,
    ) -> Result<Vec<AccessibilityNode>, BackendError> {
        let handle = NodeHandle {
            window_id: view.window_id.clone(),
            generation,
            opaque_id: "active-window".into(),
        };
        Ok(vec![AccessibilityNode {
            handle,
            role: SemanticRole::Window,
            name: view.title.clone(),
            description: Some(
                "Active window; semantic child bridge is degraded on this host".into(),
            ),
            value: None,
            bounds: view.bounds,
            states: [NodeState::Enabled, NodeState::Focused].into(),
            actions: [NodeAction::Focus].into(),
            parent: None,
            children: Vec::new(),
            properties: BTreeMap::from([("application_id".into(), view.application_id.clone())]),
        }])
    }

    async fn capture_frame(
        &self,
        scope: &CaptureScope,
        generation: SnapshotGeneration,
        redacted_regions: &[Rect],
    ) -> Result<CapturedFrame, BackendError> {
        let png = capture_png(scope, self.observe().await?.bounds).await?;
        let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .map_err(|error| BackendError::InvalidData(error.to_string()))?
            .to_rgba8();
        let (width, height) = image.dimensions();
        let mut bytes = image.into_raw();
        for region in redacted_regions {
            redact_rgba(&mut bytes, width, height, *region);
        }
        Ok(CapturedFrame {
            generation,
            descriptor: ScreenFrameDescriptor {
                frame_id: uuid::Uuid::new_v4().to_string(),
                width,
                height,
                scale_milli: 1_000,
                pixel_format: PixelFormat::Rgba8,
                redacted_regions: u32::try_from(redacted_regions.len()).unwrap_or(u32::MAX),
            },
            bytes,
        })
    }

    async fn execute_native(
        &self,
        action: &DesktopAction,
        _generation: SnapshotGeneration,
    ) -> Result<NativeActionEvidence, BackendError> {
        execute_action(action).await?;
        Ok(NativeActionEvidence {
            backend_operation: format!("{:?}", action.effect()).to_ascii_lowercase(),
            native_target_id: action
                .target_handles()
                .first()
                .map(|handle| handle.opaque_id.clone()),
            changed: !matches!(
                action,
                DesktopAction::Inspect { .. } | DesktopAction::Capture { .. }
            ),
        })
    }
}

async fn capture_png(
    scope: &CaptureScope,
    active_bounds: Option<Rect>,
) -> Result<Vec<u8>, BackendError> {
    match env::consts::OS {
        "linux" if executable("grim") => {
            let geometry = match scope {
                CaptureScope::ActiveWindow => active_bounds.map(|bounds| {
                    format!(
                        "{},{} {}x{}",
                        bounds.x, bounds.y, bounds.width, bounds.height
                    )
                }),
                CaptureScope::Window(_) => active_bounds.map(|bounds| {
                    format!(
                        "{},{} {}x{}",
                        bounds.x, bounds.y, bounds.width, bounds.height
                    )
                }),
                CaptureScope::Display(_) => None,
            };
            let args = geometry
                .as_ref()
                .map_or_else(|| vec!["-"], |geometry| vec!["-g", geometry.as_str(), "-"]);
            output("grim", &args).await
        }
        "macos" => {
            let file = tempfile::NamedTempFile::new()
                .map_err(|error| BackendError::Operation(error.to_string()))?;
            let path = file.path().to_string_lossy().to_string();
            output("screencapture", &["-x", "-t", "png", &path]).await?;
            std::fs::read(file.path()).map_err(|error| BackendError::Operation(error.to_string()))
        }
        "windows" => Err(BackendError::Unavailable(
            "Windows pixel capture requires the WGC helper; semantic context remains available"
                .into(),
        )),
        _ => Err(BackendError::Unavailable(
            "no screen-capture utility is available".into(),
        )),
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Values are clamped to the image's unsigned bounds first.
fn redact_rgba(bytes: &mut [u8], width: u32, height: u32, region: Rect) {
    let start_x = region.x.max(0.0).floor() as u32;
    let start_y = region.y.max(0.0).floor() as u32;
    let end_x = (region.x + region.width)
        .max(0.0)
        .ceil()
        .min(f64::from(width)) as u32;
    let end_y = (region.y + region.height)
        .max(0.0)
        .ceil()
        .min(f64::from(height)) as u32;
    for y in start_y.min(height)..end_y.min(height) {
        for x in start_x.min(width)..end_x.min(width) {
            let index = usize::try_from((y * width + x) * 4).unwrap_or(usize::MAX);
            if let Some(pixel) = bytes.get_mut(index..index.saturating_add(4)) {
                pixel.copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
}

async fn execute_action(action: &DesktopAction) -> Result<(), BackendError> {
    match action {
        DesktopAction::Launch { application } => {
            launch(&application.stable_id, &application.arguments)
        }
        DesktopAction::TypeText { text, .. } => type_text(text.expose()).await,
        DesktopAction::Focus { target } | DesktopAction::Click { target, .. } => {
            focus_window(&target.window_id.0).await
        }
        DesktopAction::Scroll { delta_y, .. } => scroll(*delta_y).await,
        DesktopAction::Inspect { .. }
        | DesktopAction::Capture { .. }
        | DesktopAction::WaitFor { .. }
        | DesktopAction::Assert { .. } => Ok(()),
        DesktopAction::Drag { .. } => Err(BackendError::Unavailable(
            "drag requires a coordinate-capable native bridge".into(),
        )),
    }
}

fn launch(application: &str, arguments: &[String]) -> Result<(), BackendError> {
    let (program, prefix): (&str, Vec<String>) = match env::consts::OS {
        "macos" => (
            "open",
            vec!["-a".into(), application.into(), "--args".into()],
        ),
        "windows" => (
            if executable("pwsh.exe") {
                "pwsh.exe"
            } else {
                "powershell.exe"
            },
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "Start-Process".into(),
                application.into(),
                "-ArgumentList".into(),
            ],
        ),
        _ => (application, Vec::new()),
    };
    let mut command = Command::new(program);
    command
        .args(prefix)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
        .spawn()
        .map_err(|error| BackendError::Operation(error.to_string()))?;
    Ok(())
}

async fn type_text(text: &str) -> Result<(), BackendError> {
    match env::consts::OS {
        "linux" if executable("wtype") => {
            output("wtype", &["--", text]).await?;
            Ok(())
        }
        "linux" if executable("ydotool") => {
            output("ydotool", &["type", "--", text]).await?;
            Ok(())
        }
        "linux" if executable("xdotool") => {
            output("xdotool", &["type", "--clearmodifiers", "--", text]).await?;
            Ok(())
        }
        "macos" => {
            let mut child = Command::new("osascript")
                .args([
                    "-e",
                    "on run argv",
                    "-e",
                    "tell application \"System Events\" to keystroke (item 1 of argv)",
                    "-e",
                    "end run",
                    "--",
                    text,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| BackendError::Operation(error.to_string()))?;
            let status = child
                .wait()
                .await
                .map_err(|error| BackendError::Operation(error.to_string()))?;
            if status.success() {
                Ok(())
            } else {
                Err(BackendError::Operation("Apple Events typing failed".into()))
            }
        }
        "windows" => Err(BackendError::Unavailable(
            "text injection requires the signed Windows SendInput helper".into(),
        )),
        _ => Err(BackendError::Unavailable(
            "no text-input adapter is available".into(),
        )),
    }
}

async fn focus_window(window_id: &str) -> Result<(), BackendError> {
    if env::consts::OS == "linux" && executable("hyprctl") {
        output(
            "hyprctl",
            &["dispatch", "focuswindow", &format!("address:{window_id}")],
        )
        .await?;
        Ok(())
    } else if env::consts::OS == "linux" && executable("xdotool") {
        output("xdotool", &["windowactivate", "--sync", window_id]).await?;
        Ok(())
    } else {
        Err(BackendError::Unavailable(
            "focused-window activation is unavailable".into(),
        ))
    }
}

async fn scroll(delta_y: i32) -> Result<(), BackendError> {
    let clicks = delta_y
        .unsigned_abs()
        .div_ceil(120)
        .clamp(1, 20)
        .to_string();
    if env::consts::OS == "linux" && executable("ydotool") {
        let direction = if delta_y < 0 { "4" } else { "5" };
        output("ydotool", &["click", "--repeat", &clicks, direction]).await?;
        Ok(())
    } else if env::consts::OS == "linux" && executable("xdotool") {
        let direction = if delta_y < 0 { "4" } else { "5" };
        output("xdotool", &["click", "--repeat", &clicks, direction]).await?;
        Ok(())
    } else {
        Err(BackendError::Unavailable(
            "scroll adapter is unavailable".into(),
        ))
    }
}
