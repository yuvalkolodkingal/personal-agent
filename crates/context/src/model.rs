//! Normalized active-view and accessibility context graph.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Monotonic identity for one desktop observation epoch.
///
/// `epoch` changes after suspend, display topology changes, backend reconnects,
/// or a permission-session replacement. `sequence` changes after each snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SnapshotGeneration {
    pub epoch: u64,
    pub sequence: u64,
}

impl SnapshotGeneration {
    #[must_use]
    pub fn next(self) -> Self {
        Self {
            epoch: self.epoch,
            sequence: self.sequence.saturating_add(1),
        }
    }

    #[must_use]
    pub fn invalidate(self) -> Self {
        Self {
            epoch: self.epoch.saturating_add(1),
            sequence: 0,
        }
    }
}

/// Logical rectangle expressed in desktop coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    #[must_use]
    pub fn is_valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f64::is_finite)
            && self.width >= 0.0
            && self.height >= 0.0
    }

    #[must_use]
    pub fn contains(self, x: f64, y: f64) -> bool {
        self.is_valid()
            && x >= self.x
            && y >= self.y
            && x <= self.x + self.width
            && y <= self.y + self.height
    }
}

/// Stable window identity within a backend epoch.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct WindowId(pub String);

/// Opaque accessibility node identity valid only for one exact snapshot.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct NodeHandle {
    pub window_id: WindowId,
    pub generation: SnapshotGeneration,
    pub opaque_id: String,
}

/// Common semantic roles across UIA, `AXUIElement`, and AT-SPI.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRole {
    Application,
    Window,
    Dialog,
    Group,
    Toolbar,
    Menu,
    MenuItem,
    Button,
    Link,
    CheckBox,
    RadioButton,
    ComboBox,
    TextField,
    SearchField,
    StaticText,
    Heading,
    List,
    ListItem,
    Table,
    Row,
    Cell,
    Tab,
    Image,
    Slider,
    ScrollArea,
    Terminal,
    Document,
    Unknown,
}

/// Semantic action exposed by an accessibility node.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeAction {
    Focus,
    Press,
    SetValue,
    ReplaceSelection,
    Scroll,
    Drag,
    Expand,
    Collapse,
    Select,
}

/// Accessibility states normalized across operating systems.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Enabled,
    Focused,
    Selected,
    Checked,
    Expanded,
    Editable,
    Password,
    Offscreen,
    Busy,
}

/// One node in a flat, generation-bound accessibility graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccessibilityNode {
    pub handle: NodeHandle,
    pub role: SemanticRole,
    pub name: String,
    pub description: Option<String>,
    pub value: Option<String>,
    pub bounds: Option<Rect>,
    pub states: BTreeSet<NodeState>,
    pub actions: BTreeSet<NodeAction>,
    pub parent: Option<NodeHandle>,
    pub children: Vec<NodeHandle>,
    /// Additional non-secret native properties. Backends must exclude tokens,
    /// credentials, password values, and arbitrary process environment data.
    pub properties: BTreeMap<String, String>,
}

impl AccessibilityNode {
    #[must_use]
    pub fn is_sensitive(&self) -> bool {
        self.states.contains(&NodeState::Password)
    }
}

/// Active window metadata. `secure_surface` must be set for lock screens,
/// password managers, secure input surfaces, and OS authentication prompts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveView {
    pub application_id: String,
    pub application_name: String,
    pub process_id: Option<u32>,
    pub window_id: WindowId,
    pub title: String,
    pub bounds: Option<Rect>,
    pub focused_node: Option<NodeHandle>,
    pub secure_surface: bool,
}

/// Pixel metadata retained in a serializable snapshot. Raw frame bytes are
/// deliberately returned separately by [`crate::ScreenCaptureAdapter`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScreenFrameDescriptor {
    pub frame_id: String,
    pub width: u32,
    pub height: u32,
    pub scale_milli: u32,
    pub pixel_format: PixelFormat,
    pub redacted_regions: u32,
}

/// Pixel formats accepted from native bridges.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    Bgra8,
    Rgba8,
}

/// In-memory frame whose bytes must not be persisted by default.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedFrame {
    pub generation: SnapshotGeneration,
    pub descriptor: ScreenFrameDescriptor,
    pub bytes: Vec<u8>,
}

impl CapturedFrame {
    /// Validate dimensions and exact byte count before image processing.
    ///
    /// # Errors
    ///
    /// Rejects overflow, empty frames, stale generations, and truncated data.
    pub fn validate(&self, expected: SnapshotGeneration) -> Result<(), ContextModelError> {
        if self.generation != expected {
            return Err(ContextModelError::StaleGeneration {
                expected,
                actual: self.generation,
            });
        }
        if self.descriptor.width == 0 || self.descriptor.height == 0 {
            return Err(ContextModelError::InvalidFrame("empty dimensions".into()));
        }
        let expected_bytes = usize::try_from(self.descriptor.width)
            .ok()
            .and_then(|width| {
                usize::try_from(self.descriptor.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| ContextModelError::InvalidFrame("frame dimensions overflow".into()))?;
        if self.bytes.len() != expected_bytes {
            return Err(ContextModelError::InvalidFrame(format!(
                "expected {expected_bytes} bytes, received {}",
                self.bytes.len()
            )));
        }
        Ok(())
    }
}

/// Accessibility-first active-view snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveViewSnapshot {
    pub generation: SnapshotGeneration,
    pub observed_at_unix_ms: u64,
    pub view: ActiveView,
    pub nodes: Vec<AccessibilityNode>,
    pub frame: Option<ScreenFrameDescriptor>,
    pub backend: String,
    pub degraded_reasons: Vec<String>,
}

impl ActiveViewSnapshot {
    /// Find a node only when its handle belongs to this exact view generation.
    ///
    /// # Errors
    ///
    /// Rejects stale, cross-window, blank, or missing handles.
    pub fn resolve(&self, handle: &NodeHandle) -> Result<&AccessibilityNode, ContextModelError> {
        self.validate_handle(handle)?;
        self.nodes
            .iter()
            .find(|node| node.handle == *handle)
            .ok_or(ContextModelError::UnknownHandle)
    }

    /// Validate a handle before it reaches a native adapter.
    ///
    /// # Errors
    ///
    /// Rejects handles from another generation/window or with blank identity.
    pub fn validate_handle(&self, handle: &NodeHandle) -> Result<(), ContextModelError> {
        if handle.generation != self.generation {
            return Err(ContextModelError::StaleGeneration {
                expected: self.generation,
                actual: handle.generation,
            });
        }
        if handle.window_id != self.view.window_id {
            return Err(ContextModelError::CrossWindowHandle);
        }
        if handle.opaque_id.trim().is_empty() {
            return Err(ContextModelError::InvalidHandle);
        }
        Ok(())
    }

    /// Validate graph invariants at a trust boundary.
    ///
    /// # Errors
    ///
    /// Rejects duplicate handles, invalid rectangles, stale node references,
    /// password values, cross-window edges, and dangling graph edges.
    pub fn validate(&self) -> Result<(), ContextModelError> {
        if self.view.application_id.trim().is_empty() || self.view.window_id.0.trim().is_empty() {
            return Err(ContextModelError::InvalidSnapshot(
                "active application and window identity are required".into(),
            ));
        }
        if self.view.bounds.is_some_and(|bounds| !bounds.is_valid()) {
            return Err(ContextModelError::InvalidSnapshot(
                "active window has invalid bounds".into(),
            ));
        }
        let handles: BTreeSet<_> = self.nodes.iter().map(|node| node.handle.clone()).collect();
        if handles.len() != self.nodes.len() {
            return Err(ContextModelError::InvalidSnapshot(
                "duplicate accessibility handles".into(),
            ));
        }
        for node in &self.nodes {
            self.validate_handle(&node.handle)?;
            if node.bounds.is_some_and(|bounds| !bounds.is_valid()) {
                return Err(ContextModelError::InvalidSnapshot(
                    "accessibility node has invalid bounds".into(),
                ));
            }
            if node.is_sensitive() && node.value.is_some() {
                return Err(ContextModelError::SensitiveValueExposed);
            }
            for edge in node.parent.iter().chain(&node.children) {
                self.validate_handle(edge)?;
                if !handles.contains(edge) {
                    return Err(ContextModelError::DanglingHandle);
                }
            }
        }
        if let Some(focused) = &self.view.focused_node {
            self.validate_handle(focused)?;
            if !handles.contains(focused) {
                return Err(ContextModelError::DanglingHandle);
            }
        }
        Ok(())
    }
}

/// Invalid native context returned across the adapter boundary.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ContextModelError {
    #[error("stale desktop generation: expected {expected:?}, got {actual:?}")]
    StaleGeneration {
        expected: SnapshotGeneration,
        actual: SnapshotGeneration,
    },
    #[error("node handle belongs to another window")]
    CrossWindowHandle,
    #[error("node handle is invalid")]
    InvalidHandle,
    #[error("node handle does not exist in the snapshot")]
    UnknownHandle,
    #[error("snapshot contains a dangling node handle")]
    DanglingHandle,
    #[error("native adapter exposed a sensitive node value")]
    SensitiveValueExposed,
    #[error("invalid active-view snapshot: {0}")]
    InvalidSnapshot(String),
    #[error("invalid captured frame: {0}")]
    InvalidFrame(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(password_value: Option<&str>) -> ActiveViewSnapshot {
        let generation = SnapshotGeneration {
            epoch: 4,
            sequence: 9,
        };
        let window_id = WindowId("window".into());
        let handle = NodeHandle {
            window_id: window_id.clone(),
            generation,
            opaque_id: "password".into(),
        };
        ActiveViewSnapshot {
            generation,
            observed_at_unix_ms: 1,
            view: ActiveView {
                application_id: "org.example.Login".into(),
                application_name: "Login".into(),
                process_id: Some(5),
                window_id,
                title: "Sign in".into(),
                bounds: None,
                focused_node: Some(handle.clone()),
                secure_surface: false,
            },
            nodes: vec![AccessibilityNode {
                handle,
                role: SemanticRole::TextField,
                name: "Password".into(),
                description: None,
                value: password_value.map(str::to_owned),
                bounds: None,
                states: [NodeState::Enabled, NodeState::Password].into(),
                actions: [NodeAction::Focus, NodeAction::SetValue].into(),
                parent: None,
                children: Vec::new(),
                properties: BTreeMap::new(),
            }],
            frame: None,
            backend: "fixture".into(),
            degraded_reasons: Vec::new(),
        }
    }

    #[test]
    fn native_password_values_are_rejected_at_boundary() {
        assert_eq!(
            snapshot(Some("secret")).validate(),
            Err(ContextModelError::SensitiveValueExposed)
        );
        snapshot(None).validate().expect("redacted password field");
    }

    #[test]
    fn generation_change_invalidates_all_handles() {
        let current = snapshot(None);
        let mut stale = current.nodes[0].handle.clone();
        stale.generation = stale.generation.invalidate();
        assert!(matches!(
            current.resolve(&stale),
            Err(ContextModelError::StaleGeneration { .. })
        ));
    }

    #[test]
    fn raw_frame_length_must_exactly_match_dimensions() {
        let generation = SnapshotGeneration {
            epoch: 1,
            sequence: 1,
        };
        let mut frame = CapturedFrame {
            generation,
            descriptor: ScreenFrameDescriptor {
                frame_id: "f".into(),
                width: 3,
                height: 2,
                scale_milli: 1_000,
                pixel_format: PixelFormat::Rgba8,
                redacted_regions: 0,
            },
            bytes: vec![0; 24],
        };
        frame.validate(generation).expect("exact frame length");
        frame.bytes.pop();
        assert!(matches!(
            frame.validate(generation),
            Err(ContextModelError::InvalidFrame(_))
        ));
    }
}
