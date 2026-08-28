//! Fail-closed context collection and verified desktop action coordination.

use crate::{
    AccessibilityNode, ActionValidationError, ActiveViewSnapshot, BackendError, CaptureScope,
    CapturedFrame, DesktopAction, DesktopActionRequest, DesktopBackend, DesktopCondition,
    DesktopEffect, NativeActionEvidence, NodeAction, NodeSelector, NodeState, Postcondition,
    PrivacyError, ScreenPrivacyPolicy, SnapshotGeneration,
};
use personal_agent_platform::PermissionState;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::{Duration, Instant, sleep};

/// Verified, content-free action receipt suitable for durable audit logs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesktopActionReceipt {
    pub request_id: String,
    pub effect: DesktopEffect,
    pub backend: String,
    pub before_generation: SnapshotGeneration,
    pub after_generation: SnapshotGeneration,
    pub native_evidence: Option<NativeActionEvidence>,
    pub verified_postconditions: usize,
    pub verified: bool,
}

/// Successful result. Raw frame bytes remain ephemeral and are never serializable.
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopActionOutcome {
    pub receipt: DesktopActionReceipt,
    pub snapshot: ActiveViewSnapshot,
    pub captured_frame: Option<CapturedFrame>,
}

/// An effect may have occurred but could not be proven. Callers must surface this
/// state and must not retry automatically because doing so could duplicate effects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedDesktopEffect {
    pub request_id: String,
    pub evidence: NativeActionEvidence,
    pub reason: String,
}

/// Context/control coordination failure.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum CoordinatorError {
    #[error(transparent)]
    Validation(#[from] ActionValidationError),
    #[error(transparent)]
    Privacy(#[from] PrivacyError),
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error(transparent)]
    Model(#[from] crate::ContextModelError),
    #[error("native target does not expose required semantic action: {0}")]
    UnsupportedNodeAction(&'static str),
    #[error("desktop condition was not satisfied: {0}")]
    ConditionFailed(String),
    #[error("desktop wait timed out after {timeout_ms} ms")]
    WaitTimeout { timeout_ms: u64 },
    #[error("effect may have occurred but its postcondition was not verified: {0:?}")]
    UnverifiedEffect(UnverifiedDesktopEffect),
}

/// Policy-enforcing coordinator. Native bridge implementations cannot bypass
/// handle validation, privacy exclusions, authorization, or verification by
/// returning a nominal success value.
pub struct DesktopCoordinator<B> {
    backend: B,
    privacy: ScreenPrivacyPolicy,
}

impl<B: DesktopBackend> DesktopCoordinator<B> {
    #[must_use]
    pub fn new(backend: B, privacy: ScreenPrivacyPolicy) -> Self {
        Self { backend, privacy }
    }

    #[must_use]
    pub fn backend(&self) -> &B {
        &self.backend
    }

    #[must_use]
    pub fn privacy_policy(&self) -> &ScreenPrivacyPolicy {
        &self.privacy
    }

    pub fn set_privacy_policy(&mut self, privacy: ScreenPrivacyPolicy) {
        self.privacy = privacy;
    }

    /// Collect semantic context only after active-window privacy evaluation.
    ///
    /// # Errors
    ///
    /// Rejects denied/excluded views, disconnected backends, and malformed or
    /// sensitive native accessibility data.
    pub async fn snapshot(
        &self,
        scope: &CaptureScope,
    ) -> Result<ActiveViewSnapshot, CoordinatorError> {
        require_permission(
            &self.backend.status().permissions.accessibility,
            "accessibility",
        )?;
        let observation = self.backend.active_view().await?;
        self.privacy.authorize(scope, &observation.view)?;
        let mut nodes = self
            .backend
            .accessibility_nodes(&observation.view, observation.generation)
            .await?;
        if self
            .privacy
            .redact_semantics(&observation.view.application_id)
        {
            redact_semantics(&mut nodes);
        }
        let snapshot = ActiveViewSnapshot {
            generation: observation.generation,
            observed_at_unix_ms: observation.observed_at_unix_ms,
            view: observation.view,
            nodes,
            frame: None,
            backend: self.backend.status().plan.accessibility_backend,
            degraded_reasons: Vec::new(),
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Capture one ephemeral frame after privacy and generation checks.
    ///
    /// # Errors
    ///
    /// Rejects disallowed capture scope, stale snapshots, malformed frame data,
    /// and bridge failures.
    pub async fn capture(
        &self,
        scope: &CaptureScope,
        snapshot: &ActiveViewSnapshot,
    ) -> Result<CapturedFrame, CoordinatorError> {
        snapshot.validate()?;
        self.privacy.authorize(scope, &snapshot.view)?;
        require_permission(
            &self.backend.status().permissions.screen_capture,
            "screen capture",
        )?;
        let frame = self
            .backend
            .capture_frame(scope, snapshot.generation, &self.privacy.redacted_regions)
            .await?;
        frame.validate(snapshot.generation)?;
        let required_redactions = u32::try_from(self.privacy.redacted_regions.len())
            .map_err(|_| BackendError::InvalidData("too many privacy redactions".into()))?;
        if frame.descriptor.redacted_regions < required_redactions {
            return Err(CoordinatorError::Backend(BackendError::InvalidData(
                "native capture did not attest every requested redaction".into(),
            )));
        }
        Ok(frame)
    }

    /// Execute or observe one request and report success only after verification.
    ///
    /// # Errors
    ///
    /// Returns typed errors for invalid authorization, stale handles, privacy
    /// denial, native failure, timeout, or unverified postconditions.
    pub async fn execute(
        &self,
        request: &DesktopActionRequest,
        before: ActiveViewSnapshot,
    ) -> Result<DesktopActionOutcome, CoordinatorError> {
        request.validate()?;
        before.validate()?;
        self.privacy
            .authorize(&CaptureScope::ActiveWindow, &before.view)?;
        for handle in request.action.target_handles() {
            before.resolve(handle)?;
        }
        preflight_semantic_action(&request.action, &before)?;

        match &request.action {
            DesktopAction::Inspect { .. } => Ok(observe_outcome(request, before, None)),
            DesktopAction::Capture { scope } => {
                let frame = self.capture(scope, &before).await?;
                let mut snapshot = before;
                snapshot.frame = Some(frame.descriptor.clone());
                Ok(observe_outcome(request, snapshot, Some(frame)))
            }
            DesktopAction::Assert { condition } => {
                verify_condition(condition, &before)?;
                Ok(observe_outcome(request, before, None))
            }
            DesktopAction::WaitFor {
                condition,
                timeout_ms,
                poll_interval_ms,
            } => {
                let snapshot = self
                    .wait_for(condition, *timeout_ms, *poll_interval_ms, before)
                    .await?;
                Ok(observe_outcome(request, snapshot, None))
            }
            _ => self.execute_effect(request, before).await,
        }
    }

    async fn execute_effect(
        &self,
        request: &DesktopActionRequest,
        before: ActiveViewSnapshot,
    ) -> Result<DesktopActionOutcome, CoordinatorError> {
        if !matches!(request.action, DesktopAction::Launch { .. }) {
            require_permission(
                &self.backend.status().permissions.input_control,
                "input control",
            )?;
        }
        let evidence = self
            .backend
            .execute_native(&request.action, before.generation)
            .await?;
        let after_result = self.snapshot(&CaptureScope::ActiveWindow).await;
        let after = match after_result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return Err(CoordinatorError::UnverifiedEffect(
                    UnverifiedDesktopEffect {
                        request_id: request.request_id.clone(),
                        evidence,
                        reason: error.to_string(),
                    },
                ));
            }
        };
        let verification = verify_postconditions(&request.postconditions, &before, &after);
        if let Err(error) = verification {
            return Err(CoordinatorError::UnverifiedEffect(
                UnverifiedDesktopEffect {
                    request_id: request.request_id.clone(),
                    evidence,
                    reason: error.to_string(),
                },
            ));
        }
        Ok(DesktopActionOutcome {
            receipt: DesktopActionReceipt {
                request_id: request.request_id.clone(),
                effect: request.action.effect(),
                backend: self.backend.status().plan.input_backend,
                before_generation: before.generation,
                after_generation: after.generation,
                native_evidence: Some(evidence),
                verified_postconditions: request.postconditions.len(),
                verified: true,
            },
            snapshot: after,
            captured_frame: None,
        })
    }

    async fn wait_for(
        &self,
        condition: &DesktopCondition,
        timeout_ms: u64,
        poll_interval_ms: u64,
        initial: ActiveViewSnapshot,
    ) -> Result<ActiveViewSnapshot, CoordinatorError> {
        if verify_condition(condition, &initial).is_ok() {
            return Ok(initial);
        }
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if Instant::now() >= deadline {
                return Err(CoordinatorError::WaitTimeout { timeout_ms });
            }
            sleep(Duration::from_millis(poll_interval_ms)).await;
            let snapshot = self.snapshot(&CaptureScope::ActiveWindow).await?;
            if verify_condition(condition, &snapshot).is_ok() {
                return Ok(snapshot);
            }
        }
    }
}

fn require_permission(
    permission: &PermissionState,
    capability: &str,
) -> Result<(), CoordinatorError> {
    let reason = match permission {
        PermissionState::Granted => return Ok(()),
        PermissionState::Denied { guidance } | PermissionState::NotDetermined { guidance } => {
            format!("{capability}: {guidance}")
        }
        PermissionState::Unavailable { reason } => format!("{capability}: {reason}"),
    };
    Err(CoordinatorError::Backend(BackendError::PermissionDenied(
        reason,
    )))
}

fn observe_outcome(
    request: &DesktopActionRequest,
    snapshot: ActiveViewSnapshot,
    captured_frame: Option<CapturedFrame>,
) -> DesktopActionOutcome {
    DesktopActionOutcome {
        receipt: DesktopActionReceipt {
            request_id: request.request_id.clone(),
            effect: DesktopEffect::Observe,
            backend: snapshot.backend.clone(),
            before_generation: snapshot.generation,
            after_generation: snapshot.generation,
            native_evidence: None,
            verified_postconditions: request.postconditions.len(),
            verified: true,
        },
        snapshot,
        captured_frame,
    }
}

fn redact_semantics(nodes: &mut [AccessibilityNode]) {
    for node in nodes {
        node.name.clear();
        node.description = None;
        node.value = None;
        node.properties.clear();
    }
}

fn preflight_semantic_action(
    action: &DesktopAction,
    snapshot: &ActiveViewSnapshot,
) -> Result<(), CoordinatorError> {
    let required = match action {
        DesktopAction::Click { target, .. } => Some((target, NodeAction::Press, "press")),
        DesktopAction::TypeText {
            target,
            replace_selection,
            ..
        } => Some((
            target,
            if *replace_selection {
                NodeAction::ReplaceSelection
            } else {
                NodeAction::SetValue
            },
            "edit text",
        )),
        DesktopAction::Scroll {
            target: Some(target),
            ..
        } => Some((target, NodeAction::Scroll, "scroll")),
        DesktopAction::Focus { target } => Some((target, NodeAction::Focus, "focus")),
        DesktopAction::Drag { target, .. } => Some((target, NodeAction::Drag, "drag")),
        _ => None,
    };
    if let Some((target, required, label)) = required {
        let node = snapshot.resolve(target)?;
        if !node.actions.contains(&required) {
            return Err(CoordinatorError::UnsupportedNodeAction(label));
        }
        if matches!(action, DesktopAction::TypeText { .. })
            && (!node.states.contains(&NodeState::Editable) || node.is_sensitive())
        {
            return Err(CoordinatorError::UnsupportedNodeAction(
                "edit a non-editable or password field",
            ));
        }
    }
    Ok(())
}

fn find_node<'a>(
    selector: &NodeSelector,
    snapshot: &'a ActiveViewSnapshot,
) -> Result<&'a AccessibilityNode, CoordinatorError> {
    match selector {
        NodeSelector::Handle(handle) => Ok(snapshot.resolve(handle)?),
        NodeSelector::Semantic {
            window_id,
            role,
            name,
        } => snapshot
            .nodes
            .iter()
            .find(|node| {
                window_id
                    .as_ref()
                    .is_none_or(|expected| expected == &node.handle.window_id)
                    && role.as_ref().is_none_or(|expected| expected == &node.role)
                    && (name.is_empty() || node.name.eq_ignore_ascii_case(name))
            })
            .ok_or_else(|| CoordinatorError::ConditionFailed("node does not exist".into())),
    }
}

fn verify_condition(
    condition: &DesktopCondition,
    snapshot: &ActiveViewSnapshot,
) -> Result<(), CoordinatorError> {
    let satisfied = match condition {
        DesktopCondition::NodeExists { target } => find_node(target, snapshot).is_ok(),
        DesktopCondition::NodeFocused { target } => find_node(target, snapshot)?
            .states
            .contains(&NodeState::Focused),
        DesktopCondition::NodeValueContains { target, text } => find_node(target, snapshot)?
            .value
            .as_ref()
            .is_some_and(|value| value.contains(text)),
        DesktopCondition::NodeValueEquals { target, text } => {
            find_node(target, snapshot)?.value.as_deref() == Some(text)
        }
        DesktopCondition::ApplicationActive { application_id } => snapshot
            .view
            .application_id
            .eq_ignore_ascii_case(application_id),
        DesktopCondition::WindowTitleContains { text } => snapshot
            .view
            .title
            .to_lowercase()
            .contains(&text.to_lowercase()),
        DesktopCondition::WindowExists { window_id } => snapshot.view.window_id == *window_id,
    };
    if satisfied {
        Ok(())
    } else {
        Err(CoordinatorError::ConditionFailed(format!("{condition:?}")))
    }
}

fn verify_postconditions(
    postconditions: &[Postcondition],
    before: &ActiveViewSnapshot,
    after: &ActiveViewSnapshot,
) -> Result<(), CoordinatorError> {
    for postcondition in postconditions {
        match postcondition {
            Postcondition::Condition(condition) => verify_condition(condition, after)?,
            Postcondition::GenerationAdvanced => {
                if after.generation <= before.generation {
                    return Err(CoordinatorError::ConditionFailed(
                        "desktop generation did not advance".into(),
                    ));
                }
            }
            Postcondition::ApplicationLaunched { application_id } => {
                verify_condition(
                    &DesktopCondition::ApplicationActive {
                        application_id: application_id.clone(),
                    },
                    after,
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AccessibilityAdapter, ActiveView, ActiveViewAdapter, ActiveViewObservation,
        ApplicationTarget, AuthorizationError, DesktopBackendStatus, DesktopControlAdapter,
        MouseButton, NodeHandle, PixelFormat, Rect, ScreenCaptureAdapter, ScreenFrameDescriptor,
        SemanticRole, WindowId,
    };
    use async_trait::async_trait;
    use personal_agent_platform::{
        PermissionState,
        desktop::{DesktopPermissionReport, DesktopProbeInput, probe_desktop_backend},
    };
    use std::{
        collections::BTreeMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    #[derive(Clone)]
    struct MockBackend {
        snapshots: Arc<Mutex<Vec<ActiveViewSnapshot>>>,
        native_calls: Arc<AtomicUsize>,
        accessibility_calls: Arc<AtomicUsize>,
        permissions: DesktopPermissionReport,
        attest_redactions: bool,
    }

    impl MockBackend {
        fn new(snapshots: Vec<ActiveViewSnapshot>) -> Self {
            Self {
                snapshots: Arc::new(Mutex::new(snapshots)),
                native_calls: Arc::new(AtomicUsize::new(0)),
                accessibility_calls: Arc::new(AtomicUsize::new(0)),
                permissions: DesktopPermissionReport {
                    screen_capture: PermissionState::Granted,
                    accessibility: PermissionState::Granted,
                    input_control: PermissionState::Granted,
                },
                attest_redactions: true,
            }
        }
    }

    #[async_trait]
    impl ActiveViewAdapter for MockBackend {
        async fn active_view(&self) -> Result<ActiveViewObservation, BackendError> {
            let mut guard = self.snapshots.lock().expect("snapshots");
            let snapshot = if guard.len() > 1 {
                guard.remove(0)
            } else {
                guard[0].clone()
            };
            Ok(ActiveViewObservation {
                generation: snapshot.generation,
                observed_at_unix_ms: snapshot.observed_at_unix_ms,
                view: snapshot.view,
            })
        }
    }

    #[async_trait]
    impl AccessibilityAdapter for MockBackend {
        async fn accessibility_nodes(
            &self,
            _view: &ActiveView,
            generation: SnapshotGeneration,
        ) -> Result<Vec<AccessibilityNode>, BackendError> {
            self.accessibility_calls.fetch_add(1, Ordering::SeqCst);
            self.snapshots
                .lock()
                .expect("snapshots")
                .iter()
                .find(|snapshot| snapshot.generation == generation)
                .map(|snapshot| snapshot.nodes.clone())
                .ok_or_else(|| BackendError::InvalidData("missing fixture generation".into()))
        }
    }

    #[async_trait]
    impl ScreenCaptureAdapter for MockBackend {
        async fn capture_frame(
            &self,
            _scope: &CaptureScope,
            generation: SnapshotGeneration,
            redacted_regions: &[Rect],
        ) -> Result<CapturedFrame, BackendError> {
            Ok(CapturedFrame {
                generation,
                descriptor: ScreenFrameDescriptor {
                    frame_id: "frame".into(),
                    width: 2,
                    height: 2,
                    scale_milli: 1_000,
                    pixel_format: PixelFormat::Bgra8,
                    redacted_regions: if self.attest_redactions {
                        u32::try_from(redacted_regions.len())
                            .map_err(|_| BackendError::InvalidData("too many redactions".into()))?
                    } else {
                        0
                    },
                },
                bytes: vec![0; 16],
            })
        }
    }

    #[async_trait]
    impl DesktopControlAdapter for MockBackend {
        async fn execute_native(
            &self,
            _action: &DesktopAction,
            _generation: SnapshotGeneration,
        ) -> Result<NativeActionEvidence, BackendError> {
            self.native_calls.fetch_add(1, Ordering::SeqCst);
            Ok(NativeActionEvidence {
                backend_operation: "mock.press".into(),
                native_target_id: Some("button".into()),
                changed: true,
            })
        }
    }

    impl DesktopBackend for MockBackend {
        fn status(&self) -> DesktopBackendStatus {
            DesktopBackendStatus {
                plan: probe_desktop_backend(&DesktopProbeInput {
                    operating_system: "linux".into(),
                    environment: BTreeMap::default(),
                    available_executables: Vec::new(),
                }),
                connected: true,
                permissions: self.permissions.clone(),
                connection_detail: "fixture".into(),
            }
        }
    }

    fn snapshot(sequence: u64, title: &str) -> ActiveViewSnapshot {
        let generation = SnapshotGeneration { epoch: 1, sequence };
        let window_id = WindowId("w1".into());
        let handle = NodeHandle {
            window_id: window_id.clone(),
            generation,
            opaque_id: "button".into(),
        };
        ActiveViewSnapshot {
            generation,
            observed_at_unix_ms: sequence,
            view: ActiveView {
                application_id: "org.example.Editor".into(),
                application_name: "Editor".into(),
                process_id: Some(7),
                window_id,
                title: title.into(),
                bounds: Some(Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 800.0,
                    height: 600.0,
                }),
                focused_node: Some(handle.clone()),
                secure_surface: false,
            },
            nodes: vec![AccessibilityNode {
                handle,
                role: SemanticRole::Button,
                name: "Run".into(),
                description: None,
                value: None,
                bounds: None,
                states: [NodeState::Enabled, NodeState::Focused].into(),
                actions: [NodeAction::Press].into(),
                parent: None,
                children: Vec::new(),
                properties: BTreeMap::default(),
            }],
            frame: None,
            backend: "mock".into(),
            degraded_reasons: Vec::new(),
        }
    }

    fn privacy() -> ScreenPrivacyPolicy {
        ScreenPrivacyPolicy {
            capture_enabled: true,
            ..ScreenPrivacyPolicy::default()
        }
    }

    fn click_request(
        handle: NodeHandle,
        postconditions: Vec<Postcondition>,
    ) -> DesktopActionRequest {
        DesktopActionRequest {
            request_id: "click-run".into(),
            action: DesktopAction::Click {
                target: handle,
                button: MouseButton::Primary,
                click_count: 1,
            },
            authorization: crate::ActionAuthorization {
                user_present: true,
                approved_effects: [DesktopEffect::Interact].into(),
                sensitive_text_approved: false,
            },
            postconditions,
        }
    }

    #[tokio::test]
    async fn stale_handle_is_rejected_before_native_dispatch() {
        let current = snapshot(2, "Ready");
        let backend = MockBackend::new(vec![current.clone()]);
        let calls = backend.native_calls.clone();
        let mut stale = current.nodes[0].handle.clone();
        stale.generation.sequence = 1;
        let coordinator = DesktopCoordinator::new(backend, privacy());
        let result = coordinator
            .execute(
                &click_request(stale, vec![Postcondition::GenerationAdvanced]),
                current,
            )
            .await;
        assert!(matches!(result, Err(CoordinatorError::Model(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn native_success_is_not_success_until_postconditions_pass() {
        let before = snapshot(1, "Ready");
        let after = snapshot(2, "Done");
        let handle = before.nodes[0].handle.clone();
        let backend = MockBackend::new(vec![after]);
        let calls = backend.native_calls.clone();
        let coordinator = DesktopCoordinator::new(backend, privacy());
        let result = coordinator
            .execute(
                &click_request(
                    handle,
                    vec![Postcondition::Condition(
                        DesktopCondition::WindowTitleContains {
                            text: "Never appears".into(),
                        },
                    )],
                ),
                before,
            )
            .await;
        assert!(matches!(result, Err(CoordinatorError::UnverifiedEffect(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn verified_action_returns_fresh_generation_and_receipt() {
        let before = snapshot(1, "Ready");
        let after = snapshot(2, "Done");
        let handle = before.nodes[0].handle.clone();
        let coordinator = DesktopCoordinator::new(MockBackend::new(vec![after]), privacy());
        let outcome = coordinator
            .execute(
                &click_request(
                    handle,
                    vec![
                        Postcondition::GenerationAdvanced,
                        Postcondition::Condition(DesktopCondition::WindowTitleContains {
                            text: "Done".into(),
                        }),
                    ],
                ),
                before,
            )
            .await
            .expect("verified effect");
        assert!(outcome.receipt.verified);
        assert_eq!(outcome.receipt.verified_postconditions, 2);
        assert_eq!(outcome.snapshot.generation.sequence, 2);
    }

    #[tokio::test]
    async fn privacy_denial_happens_before_accessibility_collection() {
        let backend = MockBackend::new(vec![snapshot(1, "Private")]);
        let calls = backend.accessibility_calls.clone();
        let coordinator = DesktopCoordinator::new(backend, ScreenPrivacyPolicy::default());
        assert!(matches!(
            coordinator.snapshot(&CaptureScope::ActiveWindow).await,
            Err(CoordinatorError::Privacy(PrivacyError::CaptureDisabled))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unknown_permission_never_becomes_implicit_access() {
        let mut backend = MockBackend::new(vec![snapshot(1, "Ready")]);
        backend.permissions.accessibility = PermissionState::NotDetermined {
            guidance: "run the system permission check".into(),
        };
        let coordinator = DesktopCoordinator::new(backend, privacy());
        assert!(matches!(
            coordinator.snapshot(&CaptureScope::ActiveWindow).await,
            Err(CoordinatorError::Backend(BackendError::PermissionDenied(_)))
        ));
    }

    #[tokio::test]
    async fn capture_bytes_remain_ephemeral_and_are_dimension_checked() {
        let current = snapshot(1, "Ready");
        let coordinator =
            DesktopCoordinator::new(MockBackend::new(vec![current.clone()]), privacy());
        let request = DesktopActionRequest {
            request_id: "capture".into(),
            action: DesktopAction::Capture {
                scope: CaptureScope::ActiveWindow,
            },
            authorization: crate::ActionAuthorization::observe_only(),
            postconditions: Vec::new(),
        };
        let outcome = coordinator
            .execute(&request, current)
            .await
            .expect("valid capture");
        assert_eq!(outcome.captured_frame.expect("frame").bytes.len(), 16);
        assert!(
            serde_json::to_string(&outcome.receipt)
                .expect("receipt")
                .contains("capture")
        );
    }

    #[tokio::test]
    async fn capture_fails_when_native_bridge_omits_requested_redaction() {
        let current = snapshot(1, "Ready");
        let mut backend = MockBackend::new(vec![current.clone()]);
        backend.attest_redactions = false;
        let privacy = ScreenPrivacyPolicy {
            capture_enabled: true,
            redacted_regions: vec![Rect {
                x: 1.0,
                y: 1.0,
                width: 1.0,
                height: 1.0,
            }],
            ..ScreenPrivacyPolicy::default()
        };
        let coordinator = DesktopCoordinator::new(backend, privacy);
        assert!(matches!(
            coordinator
                .capture(&CaptureScope::ActiveWindow, &current)
                .await,
            Err(CoordinatorError::Backend(BackendError::InvalidData(_)))
        ));
    }

    #[test]
    fn background_effect_and_unapproved_text_are_rejected() {
        let target = snapshot(1, "Ready").nodes[0].handle.clone();
        let action = DesktopAction::Launch {
            application: ApplicationTarget {
                stable_id: "org.example.Editor".into(),
                arguments: vec![],
            },
        };
        let authorization = crate::ActionAuthorization {
            user_present: false,
            approved_effects: [DesktopEffect::LaunchApplication].into(),
            sensitive_text_approved: false,
        };
        assert_eq!(
            authorization.authorize(&action),
            Err(AuthorizationError::UserPresenceRequired)
        );
        let type_action = DesktopAction::TypeText {
            target,
            text: crate::RedactedText::new("private"),
            replace_selection: false,
        };
        let authorization = crate::ActionAuthorization {
            user_present: true,
            approved_effects: [DesktopEffect::WriteText].into(),
            sensitive_text_approved: false,
        };
        assert_eq!(
            authorization.authorize(&type_action),
            Err(AuthorizationError::TextApprovalRequired)
        );
    }
}
