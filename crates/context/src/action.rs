//! Typed desktop action language, authorization, and postconditions.

use crate::{CaptureScope, NodeHandle, Rect, SemanticRole, WindowId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt};
use thiserror::Error;

/// Text payload whose debug representation cannot leak dictated or secret text.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RedactedText(String);

impl RedactedText {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for RedactedText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RedactedText([REDACTED])")
    }
}

/// Semantic inspection scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectScope {
    ActiveView,
    Window(WindowId),
    Node(NodeHandle),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Primary,
    Secondary,
    Middle,
}

/// Application launch identity. Platform adapters should prefer stable desktop
/// identifiers/bundle identifiers over arbitrary executable paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplicationTarget {
    pub stable_id: String,
    pub arguments: Vec<String>,
}

impl ApplicationTarget {
    /// Validate the platform-neutral launch request.
    ///
    /// # Errors
    ///
    /// Rejects blank IDs, NUL bytes, and unbounded argument collections.
    pub fn validate(&self) -> Result<(), ActionValidationError> {
        if self.stable_id.trim().is_empty() || self.stable_id.contains('\0') {
            return Err(ActionValidationError::InvalidApplicationId);
        }
        if self.arguments.len() > 128
            || self
                .arguments
                .iter()
                .any(|argument| argument.contains('\0') || argument.len() > 16_384)
        {
            return Err(ActionValidationError::InvalidArguments);
        }
        Ok(())
    }
}

/// Node lookup used by waits and postconditions. Action targets use strict
/// handles; postconditions may use a semantic locator because a fresh snapshot
/// necessarily has a newer generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "selector", rename_all = "snake_case")]
pub enum NodeSelector {
    Handle(NodeHandle),
    Semantic {
        window_id: Option<WindowId>,
        role: Option<SemanticRole>,
        name: String,
    },
}

/// Observable desktop state used by wait/assert and verification.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "condition", rename_all = "snake_case")]
pub enum DesktopCondition {
    NodeExists { target: NodeSelector },
    NodeFocused { target: NodeSelector },
    NodeValueContains { target: NodeSelector, text: String },
    NodeValueEquals { target: NodeSelector, text: String },
    ApplicationActive { application_id: String },
    WindowTitleContains { text: String },
    WindowExists { window_id: WindowId },
}

impl DesktopCondition {
    #[must_use]
    pub fn target_handle(&self) -> Option<&NodeHandle> {
        let selector = match self {
            Self::NodeExists { target }
            | Self::NodeFocused { target }
            | Self::NodeValueContains { target, .. }
            | Self::NodeValueEquals { target, .. } => Some(target),
            Self::ApplicationActive { .. }
            | Self::WindowTitleContains { .. }
            | Self::WindowExists { .. } => None,
        }?;
        match selector {
            NodeSelector::Handle(handle) => Some(handle),
            NodeSelector::Semantic { .. } => None,
        }
    }
}

/// Cross-platform desktop action DSL.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DesktopAction {
    Inspect {
        scope: InspectScope,
    },
    Capture {
        scope: CaptureScope,
    },
    Click {
        target: NodeHandle,
        button: MouseButton,
        click_count: u8,
    },
    TypeText {
        target: NodeHandle,
        text: RedactedText,
        replace_selection: bool,
    },
    Scroll {
        target: Option<NodeHandle>,
        delta_x: i32,
        delta_y: i32,
    },
    Focus {
        target: NodeHandle,
    },
    Launch {
        application: ApplicationTarget,
    },
    Drag {
        target: NodeHandle,
        destination: Rect,
    },
    WaitFor {
        condition: DesktopCondition,
        timeout_ms: u64,
        poll_interval_ms: u64,
    },
    Assert {
        condition: DesktopCondition,
    },
}

impl DesktopAction {
    #[must_use]
    pub fn effect(&self) -> DesktopEffect {
        match self {
            Self::Inspect { .. }
            | Self::Capture { .. }
            | Self::WaitFor { .. }
            | Self::Assert { .. } => DesktopEffect::Observe,
            Self::Focus { .. } | Self::Scroll { .. } => DesktopEffect::Navigate,
            Self::Click { .. } | Self::Drag { .. } => DesktopEffect::Interact,
            Self::TypeText { .. } => DesktopEffect::WriteText,
            Self::Launch { .. } => DesktopEffect::LaunchApplication,
        }
    }

    #[must_use]
    pub fn target_handles(&self) -> Vec<&NodeHandle> {
        match self {
            Self::Inspect {
                scope: InspectScope::Node(target),
            }
            | Self::Click { target, .. }
            | Self::TypeText { target, .. }
            | Self::Focus { target }
            | Self::Drag { target, .. }
            | Self::Scroll {
                target: Some(target),
                ..
            } => vec![target],
            Self::WaitFor { condition, .. } | Self::Assert { condition } => {
                condition.target_handle().into_iter().collect()
            }
            Self::Inspect { .. }
            | Self::Capture { .. }
            | Self::Scroll { target: None, .. }
            | Self::Launch { .. } => Vec::new(),
        }
    }

    /// Validate bounded values before policy evaluation or adapter dispatch.
    ///
    /// # Errors
    ///
    /// Rejects empty text, invalid click/scroll/drag values, excessive waits,
    /// malformed conditions, or unsafe launch identities.
    pub fn validate(&self) -> Result<(), ActionValidationError> {
        match self {
            Self::Click { click_count, .. } if !(1..=3).contains(click_count) => {
                Err(ActionValidationError::InvalidClickCount)
            }
            Self::TypeText { text, .. } if text.is_empty() => Err(ActionValidationError::EmptyText),
            Self::Scroll {
                delta_x: 0,
                delta_y: 0,
                ..
            } => Err(ActionValidationError::EmptyScroll),
            Self::Drag { destination, .. } if !destination.is_valid() => {
                Err(ActionValidationError::InvalidDestination)
            }
            Self::Launch { application } => application.validate(),
            Self::WaitFor {
                timeout_ms,
                poll_interval_ms,
                ..
            } if *timeout_ms == 0
                || *timeout_ms > 300_000
                || *poll_interval_ms < 25
                || *poll_interval_ms > *timeout_ms =>
            {
                Err(ActionValidationError::InvalidWait)
            }
            Self::Assert { condition } | Self::WaitFor { condition, .. } => {
                validate_condition(condition)
            }
            _ => Ok(()),
        }
    }
}

fn validate_condition(condition: &DesktopCondition) -> Result<(), ActionValidationError> {
    let valid = match condition {
        DesktopCondition::NodeExists { target } | DesktopCondition::NodeFocused { target } => {
            valid_selector(target)
        }
        DesktopCondition::NodeValueContains { target, text }
        | DesktopCondition::NodeValueEquals { target, text } => {
            valid_selector(target) && !text.trim().is_empty()
        }
        DesktopCondition::ApplicationActive {
            application_id: text,
        }
        | DesktopCondition::WindowTitleContains { text } => !text.trim().is_empty(),
        DesktopCondition::WindowExists { window_id } => !window_id.0.trim().is_empty(),
    };
    if valid {
        Ok(())
    } else {
        Err(ActionValidationError::InvalidCondition)
    }
}

fn valid_selector(selector: &NodeSelector) -> bool {
    match selector {
        NodeSelector::Handle(handle) => !handle.opaque_id.trim().is_empty(),
        NodeSelector::Semantic { role, name, .. } => role.is_some() || !name.trim().is_empty(),
    }
}

/// Coarse effect category evaluated outside all native adapters.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopEffect {
    Observe,
    Navigate,
    Interact,
    WriteText,
    LaunchApplication,
}

/// Call-scoped authorization from the policy/approval layer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActionAuthorization {
    pub user_present: bool,
    pub approved_effects: BTreeSet<DesktopEffect>,
    /// Input has been confirmed non-secret or is explicitly authorized for the target.
    pub sensitive_text_approved: bool,
}

impl ActionAuthorization {
    #[must_use]
    pub fn observe_only() -> Self {
        Self {
            user_present: true,
            approved_effects: [DesktopEffect::Observe].into(),
            sensitive_text_approved: false,
        }
    }

    /// Check a desktop effect before any backend receives it.
    ///
    /// # Errors
    ///
    /// Denies absent effect grants, background control, and typing without an
    /// explicit text approval bit.
    pub fn authorize(&self, action: &DesktopAction) -> Result<(), AuthorizationError> {
        let effect = action.effect();
        if !self.approved_effects.contains(&effect) {
            return Err(AuthorizationError::EffectNotApproved(effect));
        }
        if effect != DesktopEffect::Observe && !self.user_present {
            return Err(AuthorizationError::UserPresenceRequired);
        }
        if matches!(action, DesktopAction::TypeText { .. }) && !self.sensitive_text_approved {
            return Err(AuthorizationError::TextApprovalRequired);
        }
        Ok(())
    }
}

/// Postcondition that must be observable in a fresh snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "postcondition", rename_all = "snake_case")]
pub enum Postcondition {
    Condition(DesktopCondition),
    GenerationAdvanced,
    ApplicationLaunched { application_id: String },
}

/// One action request after policy planning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DesktopActionRequest {
    pub request_id: String,
    pub action: DesktopAction,
    pub authorization: ActionAuthorization,
    pub postconditions: Vec<Postcondition>,
}

impl DesktopActionRequest {
    /// Require identity, valid action input, authorization, and verification for effects.
    ///
    /// # Errors
    ///
    /// Rejects incomplete requests before native dispatch.
    pub fn validate(&self) -> Result<(), ActionValidationError> {
        if self.request_id.trim().is_empty() || self.request_id.len() > 256 {
            return Err(ActionValidationError::InvalidRequestId);
        }
        self.action.validate()?;
        self.authorization
            .authorize(&self.action)
            .map_err(ActionValidationError::Authorization)?;
        if self.action.effect() != DesktopEffect::Observe && self.postconditions.is_empty() {
            return Err(ActionValidationError::MissingPostcondition);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AuthorizationError {
    #[error("desktop effect was not approved: {0:?}")]
    EffectNotApproved(DesktopEffect),
    #[error("effectful desktop control requires user presence")]
    UserPresenceRequired,
    #[error("typing requires explicit approval for the text payload")]
    TextApprovalRequired,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ActionValidationError {
    #[error("request ID is blank or too long")]
    InvalidRequestId,
    #[error("application ID is blank or contains NUL")]
    InvalidApplicationId,
    #[error("application arguments are invalid or exceed limits")]
    InvalidArguments,
    #[error("click count must be between one and three")]
    InvalidClickCount,
    #[error("typed text cannot be empty")]
    EmptyText,
    #[error("scroll delta cannot be zero on both axes")]
    EmptyScroll,
    #[error("drag destination is invalid")]
    InvalidDestination,
    #[error("wait timeout or interval is outside safe bounds")]
    InvalidWait,
    #[error("desktop condition is blank or invalid")]
    InvalidCondition,
    #[error("effectful desktop action is missing a postcondition")]
    MissingPostcondition,
    #[error(transparent)]
    Authorization(AuthorizationError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SnapshotGeneration;
    use serde_json::json;

    fn handle() -> NodeHandle {
        NodeHandle {
            window_id: WindowId("w".into()),
            generation: SnapshotGeneration {
                epoch: 1,
                sequence: 2,
            },
            opaque_id: "button".into(),
        }
    }

    #[test]
    fn text_is_redacted_from_debug_output() {
        let text = RedactedText::new("correct horse battery staple");
        let debug = format!("{text:?}");
        assert!(!debug.contains("horse"));
        assert_eq!(text.expose(), "correct horse battery staple");
    }

    #[test]
    fn effectful_requests_require_grant_presence_and_postcondition() {
        let action = DesktopAction::Click {
            target: handle(),
            button: MouseButton::Primary,
            click_count: 1,
        };
        let request = DesktopActionRequest {
            request_id: "r1".into(),
            action,
            authorization: ActionAuthorization::observe_only(),
            postconditions: Vec::new(),
        };
        assert!(matches!(
            request.validate(),
            Err(ActionValidationError::Authorization(
                AuthorizationError::EffectNotApproved(DesktopEffect::Interact)
            ))
        ));

        let authorized = DesktopActionRequest {
            authorization: ActionAuthorization {
                user_present: true,
                approved_effects: [DesktopEffect::Interact].into(),
                sensitive_text_approved: false,
            },
            ..request
        };
        assert_eq!(
            authorized.validate(),
            Err(ActionValidationError::MissingPostcondition)
        );
    }

    #[test]
    fn wait_values_are_bounded() {
        let action = DesktopAction::WaitFor {
            condition: DesktopCondition::WindowTitleContains {
                text: "Done".into(),
            },
            timeout_ms: 500,
            poll_interval_ms: 50,
        };
        action.validate().expect("valid bounded wait");
        assert_eq!(
            DesktopAction::WaitFor {
                condition: DesktopCondition::WindowTitleContains {
                    text: "Done".into()
                },
                timeout_ms: 301_000,
                poll_interval_ms: 50,
            }
            .validate(),
            Err(ActionValidationError::InvalidWait)
        );
    }

    #[test]
    fn desktop_frontend_json_deserializes_with_semantic_postcondition() {
        let request: DesktopActionRequest = serde_json::from_value(json!({
            "request_id": "type-1",
            "action": {
                "action": "type_text",
                "target": {
                    "window_id": "window-1",
                    "generation": { "epoch": 2, "sequence": 4 },
                    "opaque_id": "editor"
                },
                "text": "Hello world",
                "replace_selection": false
            },
            "authorization": {
                "user_present": true,
                "approved_effects": ["write_text"],
                "sensitive_text_approved": true
            },
            "postconditions": [
                {
                    "postcondition": "condition",
                    "condition": "node_value_contains",
                    "target": {
                        "selector": "semantic",
                        "window_id": "window-1",
                        "role": "text_field",
                        "name": "Document body"
                    },
                    "text": "Hello world"
                },
                { "postcondition": "generation_advanced" }
            ]
        }))
        .expect("frontend action request");
        request.validate().expect("valid verified action request");
        assert!(matches!(request.action, DesktopAction::TypeText { .. }));
        assert!(matches!(
            request.postconditions.first(),
            Some(Postcondition::Condition(
                DesktopCondition::NodeValueContains { .. }
            ))
        ));
    }
}
