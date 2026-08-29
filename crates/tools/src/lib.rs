//! Native tool gateway. No effectful call bypasses this pipeline.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use personal_agent_policy::{
    CallContext, ConsentGrant, DataZone, Effect, PolicyDecision, PolicyEngine, ToolDescriptor,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};
use thiserror::Error;
use uuid::Uuid;

/// One requested tool invocation after plan assignment.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub call_id: Uuid,
    pub goal_id: Uuid,
    pub task_id: Option<Uuid>,
    pub tool_id: String,
    pub target: String,
    pub input: Value,
    pub input_zones: BTreeSet<DataZone>,
    pub granted_scopes: BTreeSet<String>,
    pub estimated_cost_usd: f64,
    pub background: bool,
    pub user_present: bool,
    pub checkpoint_available: bool,
}

/// Sanitized tool result returned to the runtime.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub value: Value,
    pub rollback_id: Option<String>,
    pub verified: bool,
}

/// Result of a transactional rollback. The current state is checkpointed first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RollbackOutput {
    pub restored_checkpoint_id: String,
    pub rescue_checkpoint_id: String,
    pub verified: bool,
}

/// Immutable audit record, excluding raw secret values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolAudit {
    pub call_id: Uuid,
    pub tool_id: String,
    pub target: String,
    pub at: DateTime<Utc>,
    pub decision: String,
    pub succeeded: bool,
    pub output_bytes: usize,
    pub reason: String,
    pub estimated_cost_usd: f64,
}

/// Content-free outbound data record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EgressRecord {
    pub call_id: Uuid,
    pub destination: String,
    pub kind: String,
    pub size_bytes: usize,
    pub reason: String,
}

/// Gateway or implementation failure.
#[derive(Debug, Error)]
pub enum ToolError {
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("tool input is invalid: {0}")]
    InvalidInput(String),
    #[error("tool requires approval: {0}")]
    ApprovalRequired(String),
    #[error("tool call denied: {0}")]
    Denied(String),
    #[error("tool execution failed: {0}")]
    Execution(String),
    #[error("tool postcondition failed: {0}")]
    Postcondition(String),
    #[error("tool does not support rollback")]
    RollbackUnsupported,
    #[error("rollback failed: {0}")]
    Rollback(String),
}

/// Native or isolated executable implementation.
#[async_trait]
pub trait ToolImplementation: Send + Sync {
    fn descriptor(&self) -> &ToolDescriptor;
    /// Validate input against the implementation's schema.
    ///
    /// # Errors
    ///
    /// Returns a stable validation error for malformed input.
    fn validate_input(&self, input: &Value) -> Result<(), ToolError>;
    /// Create a pre-mutation checkpoint when applicable.
    ///
    /// # Errors
    ///
    /// Returns an error when required checkpoint coverage cannot be created.
    async fn checkpoint(&self, call: &ToolCall) -> Result<Option<String>, ToolError>;
    /// Perform the declared effect.
    ///
    /// # Errors
    ///
    /// Returns an implementation error without swallowing the cause.
    async fn execute(&self, call: &ToolCall) -> Result<Value, ToolError>;
    /// Verify observable postconditions.
    ///
    /// # Errors
    ///
    /// Returns a typed postcondition error when the effect did not occur.
    async fn verify(&self, call: &ToolCall, output: &Value) -> Result<(), ToolError>;
    /// Snapshot the current state immediately before rollback so the rollback
    /// operation is itself recoverable.
    async fn snapshot_current(&self, _call: &ToolCall) -> Result<Option<String>, ToolError> {
        Ok(None)
    }
    /// Restore a prior checkpoint.
    async fn rollback(&self, _call: &ToolCall, _checkpoint_id: &str) -> Result<Value, ToolError> {
        Err(ToolError::RollbackUnsupported)
    }
    /// Verify that rollback reached the checkpoint's observable state.
    async fn verify_rollback(
        &self,
        _call: &ToolCall,
        _checkpoint_id: &str,
        _output: &Value,
    ) -> Result<(), ToolError> {
        Err(ToolError::RollbackUnsupported)
    }
}

/// Registered gateway with one fixed core policy engine.
#[derive(Default)]
pub struct ToolGateway {
    policy: PolicyEngine,
    tools: HashMap<String, Arc<dyn ToolImplementation>>,
    audits: Vec<ToolAudit>,
    egress: Vec<EgressRecord>,
    max_output_bytes: usize,
}

impl ToolGateway {
    #[must_use]
    pub fn new(max_output_bytes: usize) -> Self {
        Self {
            max_output_bytes,
            ..Self::default()
        }
    }
    pub fn register(&mut self, tool: Arc<dyn ToolImplementation>) {
        self.tools.insert(tool.descriptor().id.clone(), tool);
    }
    #[must_use]
    pub fn audits(&self) -> &[ToolAudit] {
        &self.audits
    }
    #[must_use]
    pub fn egress(&self) -> &[EgressRecord] {
        &self.egress
    }

    /// Execute validation → policy → checkpoint → effect → filtering → verification → audit.
    ///
    /// # Errors
    ///
    /// Returns errors for unknown tools, invalid input, approval/denial,
    /// checkpoint/execution failure, oversized output, or failed verification.
    pub async fn call(
        &mut self,
        mut call: ToolCall,
        grants: &[ConsentGrant],
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .tools
            .get(&call.tool_id)
            .cloned()
            .ok_or_else(|| ToolError::UnknownTool(call.tool_id.clone()))?;
        tool.validate_input(&call.input)?;
        let context = CallContext {
            goal_id: call.goal_id,
            task_id: call.task_id,
            tool: tool.descriptor(),
            target: &call.target,
            active_input_zones: &call.input_zones,
            granted_scopes: &call.granted_scopes,
            estimated_cost_usd: call.estimated_cost_usd,
            background: call.background,
            user_present: call.user_present,
            checkpoint_available: call.checkpoint_available,
        };
        let decision = self.policy.decide(&context, grants);
        let decision_label = match &decision {
            PolicyDecision::Allow { reason, .. } => reason.clone(),
            PolicyDecision::Ask { reason } => {
                return Err(ToolError::ApprovalRequired(reason.clone()));
            }
            PolicyDecision::Deny { reason } => return Err(ToolError::Denied(reason.clone())),
        };
        let rollback_id = tool.checkpoint(&call).await?;
        if tool.descriptor().reversible
            && tool.descriptor().effect != Effect::Observe
            && rollback_id.is_none()
        {
            return Err(ToolError::Execution(
                "required checkpoint implementation returned no checkpoint".into(),
            ));
        }
        if rollback_id.is_some() {
            call.checkpoint_available = true;
        }
        let raw = match tool.execute(&call).await {
            Ok(value) => value,
            Err(error) => {
                self.audits.push(ToolAudit {
                    call_id: call.call_id,
                    tool_id: call.tool_id.clone(),
                    target: call.target.clone(),
                    at: Utc::now(),
                    decision: decision_label,
                    succeeded: false,
                    output_bytes: 0,
                    reason: "tool implementation returned an execution error".into(),
                    estimated_cost_usd: call.estimated_cost_usd,
                });
                return Err(error);
            }
        };
        let filtered = redact_secrets(raw);
        let encoded = serde_json::to_vec(&filtered)
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        if encoded.len() > self.max_output_bytes {
            return Err(ToolError::Execution(format!(
                "tool output exceeds {} byte limit",
                self.max_output_bytes
            )));
        }
        tool.verify(&call, &filtered).await?;
        self.audits.push(ToolAudit {
            call_id: call.call_id,
            tool_id: call.tool_id.clone(),
            target: call.target.clone(),
            at: Utc::now(),
            decision: decision_label,
            succeeded: true,
            output_bytes: encoded.len(),
            reason: "postcondition verified".into(),
            estimated_cost_usd: call.estimated_cost_usd,
        });
        if matches!(
            tool.descriptor().effect,
            Effect::ExternalWrite
                | Effect::Communication
                | Effect::Commerce
                | Effect::Security
                | Effect::Power
        ) {
            self.egress.push(EgressRecord {
                call_id: call.call_id,
                destination: call.target.clone(),
                kind: format!("{:?}", tool.descriptor().effect).to_lowercase(),
                size_bytes: encoded.len(),
                reason: "declared tool effect".into(),
            });
        }
        Ok(ToolOutput {
            value: filtered,
            rollback_id,
            verified: true,
        })
    }

    /// Transactionally restore a tool checkpoint after first preserving the
    /// current state as a rescue checkpoint.
    ///
    /// # Errors
    ///
    /// Rejects unknown/non-reversible tools, missing rescue snapshots, restore
    /// failures, oversized output, or failed rollback verification.
    pub async fn rollback(
        &mut self,
        call: &ToolCall,
        checkpoint_id: &str,
    ) -> Result<RollbackOutput, ToolError> {
        let tool = self
            .tools
            .get(&call.tool_id)
            .cloned()
            .ok_or_else(|| ToolError::UnknownTool(call.tool_id.clone()))?;
        if !tool.descriptor().reversible {
            return Err(ToolError::RollbackUnsupported);
        }
        let rescue_checkpoint_id = tool
            .snapshot_current(call)
            .await?
            .ok_or_else(|| ToolError::Rollback("current state could not be checkpointed".into()))?;
        let raw = tool.rollback(call, checkpoint_id).await?;
        let filtered = redact_secrets(raw);
        let encoded = serde_json::to_vec(&filtered)
            .map_err(|error| ToolError::Rollback(error.to_string()))?;
        if encoded.len() > self.max_output_bytes {
            return Err(ToolError::Rollback(format!(
                "rollback output exceeds {} byte limit",
                self.max_output_bytes
            )));
        }
        tool.verify_rollback(call, checkpoint_id, &filtered).await?;
        self.audits.push(ToolAudit {
            call_id: call.call_id,
            tool_id: call.tool_id.clone(),
            target: call.target.clone(),
            at: Utc::now(),
            decision: "rollback requested through native gateway".into(),
            succeeded: true,
            output_bytes: encoded.len(),
            reason: "current state snapshotted and prior checkpoint restored".into(),
            estimated_cost_usd: call.estimated_cost_usd,
        });
        Ok(RollbackOutput {
            restored_checkpoint_id: checkpoint_id.into(),
            rescue_checkpoint_id,
            verified: true,
        })
    }
}

fn redact_secrets(value: Value) -> Value {
    match value {
        Value::Object(mut map) => {
            for (key, item) in &mut map {
                if ["secret", "password", "token", "api_key", "credential"]
                    .iter()
                    .any(|word| key.to_ascii_lowercase().contains(word))
                {
                    *item = Value::String("[REDACTED]".into());
                } else {
                    *item = redact_secrets(item.take());
                }
            }
            Value::Object(map)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact_secrets).collect()),
        Value::String(text) => Value::String(redact_secret_text(&text)),
        other => other,
    }
}

fn redact_secret_text(text: &str) -> String {
    if contains_secret_pattern(text) {
        "[REDACTED]".into()
    } else {
        text.into()
    }
}

fn contains_secret_pattern(text: &str) -> bool {
    // Preserve the established sentinels before applying the more structured
    // detectors below. A PEM body must never survive merely because its exact
    // label is new to us.
    text.contains("-----BEGIN")
        || text.contains("Bearer ")
        || text.contains("sk-")
        || text.contains("ghp_")
        || text.contains("github_pat_")
        || contains_prefixed_run(text, "AKIA", 16, |byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit()
        })
        || contains_prefixed_run(text, "sk-", 20, |byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
        })
        || ["xoxb-", "xoxa-", "xoxp-", "xoxr-", "xoxs-"]
            .iter()
            .any(|prefix| text.contains(prefix))
        || contains_jwt(text)
        || contains_keyed_long_secret(text)
}

fn contains_prefixed_run(
    text: &str,
    prefix: &str,
    minimum_length: usize,
    allowed: impl Fn(u8) -> bool,
) -> bool {
    text.match_indices(prefix).any(|(start, _)| {
        text.as_bytes()[start + prefix.len()..]
            .iter()
            .copied()
            .take_while(|byte| allowed(*byte))
            .count()
            >= minimum_length
    })
}

fn contains_jwt(text: &str) -> bool {
    let bytes = text.as_bytes();
    (0..bytes.len()).any(|start| {
        if !bytes[start..].starts_with(b"eyJ") {
            return false;
        }
        let mut cursor = start + 3;
        while bytes
            .get(cursor)
            .is_some_and(|byte| is_base64_url_byte(*byte))
        {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'.') {
            return false;
        }
        cursor += 1;
        let second_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| is_base64_url_byte(*byte))
        {
            cursor += 1;
        }
        if cursor == second_start || bytes.get(cursor) != Some(&b'.') {
            return false;
        }
        cursor += 1;
        bytes
            .get(cursor)
            .is_some_and(|byte| is_base64_url_byte(*byte))
    })
}

fn contains_keyed_long_secret(text: &str) -> bool {
    const KEY_NAMES: [&[u8]; 4] = [b"token", b"secret", b"key", b"password"];
    let bytes = text.as_bytes();

    (0..bytes.len()).any(|start| {
        KEY_NAMES.iter().any(|key| {
            let Some(candidate) = bytes.get(start..start + key.len()) else {
                return false;
            };
            if !candidate.eq_ignore_ascii_case(key) {
                return false;
            }

            let mut cursor = start + key.len();
            if bytes
                .get(cursor)
                .is_some_and(|byte| matches!(byte, b'\'' | b'"'))
            {
                cursor += 1;
            }
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            if !bytes
                .get(cursor)
                .is_some_and(|byte| matches!(byte, b':' | b'='))
            {
                return false;
            }
            cursor += 1;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            if bytes
                .get(cursor)
                .is_some_and(|byte| matches!(byte, b'\'' | b'"'))
            {
                cursor += 1;
            }

            bytes[cursor..]
                .iter()
                .copied()
                .take_while(|byte| is_base64_byte(*byte))
                .count()
                >= 32
        })
    })
}

fn is_base64_url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'=')
}

fn is_base64_byte(byte: u8) -> bool {
    is_base64_url_byte(byte) || matches!(byte, b'+' | b'/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_agent_policy::{Effect, Idempotency, Risk};
    use std::sync::Mutex;
    struct ReadTool {
        descriptor: ToolDescriptor,
    }
    #[async_trait]
    impl ToolImplementation for ReadTool {
        fn descriptor(&self) -> &ToolDescriptor {
            &self.descriptor
        }
        fn validate_input(&self, input: &Value) -> Result<(), ToolError> {
            if input.is_object() {
                Ok(())
            } else {
                Err(ToolError::InvalidInput("object required".into()))
            }
        }
        async fn checkpoint(&self, _: &ToolCall) -> Result<Option<String>, ToolError> {
            Ok(None)
        }
        async fn execute(&self, _: &ToolCall) -> Result<Value, ToolError> {
            Ok(serde_json::json!({"token":"do-not-leak","value":42}))
        }
        async fn verify(&self, _: &ToolCall, _: &Value) -> Result<(), ToolError> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn allowed_outputs_are_secret_filtered_and_audited() {
        let descriptor = ToolDescriptor {
            id: "system.observe".into(),
            version: "1.0.0".into(),
            description: "observe".into(),
            scopes: ["system.read".into()].into(),
            risk: Risk::Read,
            effect: Effect::Observe,
            idempotency: Idempotency::Safe,
            reversible: false,
            zones_read: [DataZone::TrustedLocalState].into(),
            zones_written: [DataZone::AgentGenerated].into(),
            user_presence: false,
        };
        let mut gateway = ToolGateway::new(4096);
        gateway.register(Arc::new(ReadTool { descriptor }));
        let call = ToolCall {
            call_id: Uuid::now_v7(),
            goal_id: Uuid::now_v7(),
            task_id: None,
            tool_id: "system.observe".into(),
            target: "local".into(),
            input: serde_json::json!({}),
            input_zones: [DataZone::UserInstruction].into(),
            granted_scopes: ["system.read".into()].into(),
            estimated_cost_usd: 0.0,
            background: false,
            user_present: true,
            checkpoint_available: false,
        };
        let output = gateway.call(call, &[]).await.expect("call");
        assert_eq!(output.value["token"], "[REDACTED]");
        assert_eq!(gateway.audits().len(), 1);
    }

    #[test]
    fn mutation_corpus_secret_shapes_are_always_redacted() {
        let legacy_secret_shapes = [
            ["-----BE", "GIN PRIVATE KEY-----fixture"].concat(),
            ["Bear", "er fixture-token"].concat(),
            ["s", "k-fixture-token"].concat(),
            ["gh", "p_fixture-token"].concat(),
            ["github_", "pat_fixture-token"].concat(),
        ];
        let pattern_secret_shapes = [
            ["AK", "IA1234567890ABCDEF"].concat(),
            ["s", "k-abcdefghijklmnopqrst"].concat(),
            ["s", "k-abcd_EFGH-ijklmnop1234"].concat(),
            ["xox", "b-fixture"].concat(),
            ["xox", "a-fixture"].concat(),
            ["xox", "p-fixture"].concat(),
            ["xox", "r-fixture"].concat(),
            ["xox", "s-fixture"].concat(),
            ["ey", "JhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.signature"].concat(),
            [
                "-----BE",
                "GIN RSA PRIVATE KEY-----\nfixture\n-----END RSA PRIVATE KEY-----",
            ]
            .concat(),
            [
                "-----BE",
                "GIN OPENSSH PRIVATE KEY-----\nfixture\n-----END OPENSSH PRIVATE KEY-----",
            ]
            .concat(),
            ["token=", "0123456789abcdef0123456789abcdef"].concat(),
            ["SECRET : ", "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef"].concat(),
            ["key=", "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXo012345"].concat(),
            ["password = ", "abcd_EFGH-ijklmnop_QRST-uvwxyz123456"].concat(),
            ["\"token\": \"", "0123456789abcdef0123456789abcdef\""].concat(),
        ];
        assert!(pattern_secret_shapes.len() >= 12);
        let secret_shapes = legacy_secret_shapes
            .iter()
            .chain(pattern_secret_shapes.iter())
            .collect::<Vec<_>>();
        let mut state = 0xa076_1d64_78bd_642f_u64;
        for _ in 0..2_048 {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let secret = secret_shapes[usize::try_from(state).unwrap_or(0) % secret_shapes.len()];
            let padding = format!("{:016x}", state.wrapping_mul(0x2545_f491_4f6c_dd1d));
            let value = serde_json::json!({
                "ordinary": format!("{padding}{secret}{padding}"),
                "nested": [{"authorization": secret}],
            });
            let redacted = redact_secrets(value);
            let serialized = serde_json::to_string(&redacted).expect("serialize");
            assert!(!serialized.contains(secret));
            assert!(serialized.contains("[REDACTED]"));
        }
    }

    #[test]
    fn similar_non_secret_text_is_not_over_redacted() {
        for text in [
            "AKIA1234",
            "eyJhbGciOiJIUzI1NiJ9.only-one-dot",
            "token=short",
        ] {
            assert_eq!(redact_secret_text(text), text);
        }
    }

    struct ReversibleTool {
        descriptor: ToolDescriptor,
        state: Arc<Mutex<String>>,
    }

    #[async_trait]
    impl ToolImplementation for ReversibleTool {
        fn descriptor(&self) -> &ToolDescriptor {
            &self.descriptor
        }
        fn validate_input(&self, input: &Value) -> Result<(), ToolError> {
            input
                .get("value")
                .and_then(Value::as_str)
                .map(|_| ())
                .ok_or_else(|| ToolError::InvalidInput("value string required".into()))
        }
        async fn checkpoint(&self, _: &ToolCall) -> Result<Option<String>, ToolError> {
            Ok(Some(self.state.lock().expect("state").clone()))
        }
        async fn execute(&self, call: &ToolCall) -> Result<Value, ToolError> {
            let next = call.input["value"].as_str().expect("validated").to_owned();
            *self.state.lock().expect("state") = next.clone();
            Ok(serde_json::json!({"value":next}))
        }
        async fn verify(&self, _: &ToolCall, output: &Value) -> Result<(), ToolError> {
            if self.state.lock().expect("state").as_str() == output["value"] {
                Ok(())
            } else {
                Err(ToolError::Postcondition("state mismatch".into()))
            }
        }
        async fn snapshot_current(&self, _: &ToolCall) -> Result<Option<String>, ToolError> {
            Ok(Some(self.state.lock().expect("state").clone()))
        }
        async fn rollback(&self, _: &ToolCall, checkpoint_id: &str) -> Result<Value, ToolError> {
            *self.state.lock().expect("state") = checkpoint_id.into();
            Ok(serde_json::json!({"value":checkpoint_id}))
        }
        async fn verify_rollback(
            &self,
            _: &ToolCall,
            checkpoint_id: &str,
            _: &Value,
        ) -> Result<(), ToolError> {
            if self.state.lock().expect("state").as_str() == checkpoint_id {
                Ok(())
            } else {
                Err(ToolError::Postcondition("rollback mismatch".into()))
            }
        }
    }

    #[tokio::test]
    async fn rollback_snapshots_current_state_before_verified_restore() {
        let descriptor = ToolDescriptor {
            id: "file.fixture.write".into(),
            version: "1.0.0".into(),
            description: "fixture write".into(),
            scopes: ["file.write".into()].into(),
            risk: Risk::Reversible,
            effect: Effect::ExternalWrite,
            idempotency: Idempotency::WithKey,
            reversible: true,
            zones_read: [DataZone::TrustedLocalState].into(),
            zones_written: [DataZone::TrustedLocalState].into(),
            user_presence: false,
        };
        let state = Arc::new(Mutex::new("before".into()));
        let mut gateway = ToolGateway::new(4096);
        gateway.register(Arc::new(ReversibleTool {
            descriptor,
            state: Arc::clone(&state),
        }));
        let call = ToolCall {
            call_id: Uuid::now_v7(),
            goal_id: Uuid::now_v7(),
            task_id: None,
            tool_id: "file.fixture.write".into(),
            target: "registered-workspace/file".into(),
            input: serde_json::json!({"value":"after"}),
            input_zones: [DataZone::UserInstruction].into(),
            granted_scopes: ["file.write".into()].into(),
            estimated_cost_usd: 0.0,
            background: false,
            user_present: true,
            checkpoint_available: true,
        };
        let output = gateway.call(call.clone(), &[]).await.expect("mutation");
        assert_eq!(output.rollback_id.as_deref(), Some("before"));
        assert_eq!(state.lock().expect("state").as_str(), "after");
        let restored = gateway.rollback(&call, "before").await.expect("rollback");
        assert_eq!(restored.rescue_checkpoint_id, "after");
        assert_eq!(state.lock().expect("state").as_str(), "before");
        assert!(restored.verified);
        assert_eq!(gateway.egress().len(), 1);
        assert_eq!(gateway.egress()[0].destination, "registered-workspace/file");
    }

    #[tokio::test]
    async fn reversible_mutation_fails_before_execution_without_real_checkpoint() {
        let descriptor = ToolDescriptor {
            id: "broken.write".into(),
            version: "1".into(),
            description: "broken".into(),
            scopes: ["write".into()].into(),
            risk: Risk::Reversible,
            effect: Effect::LocalWrite,
            idempotency: Idempotency::WithKey,
            reversible: true,
            zones_read: [DataZone::TrustedLocalState].into(),
            zones_written: [DataZone::TrustedLocalState].into(),
            user_presence: false,
        };
        let mut gateway = ToolGateway::new(4096);
        gateway.register(Arc::new(ReadTool { descriptor }));
        let call = ToolCall {
            call_id: Uuid::now_v7(),
            goal_id: Uuid::now_v7(),
            task_id: None,
            tool_id: "broken.write".into(),
            target: "workspace".into(),
            input: serde_json::json!({}),
            input_zones: [DataZone::UserInstruction].into(),
            granted_scopes: ["write".into()].into(),
            estimated_cost_usd: 0.0,
            background: false,
            user_present: true,
            checkpoint_available: true,
        };
        assert!(
            matches!(gateway.call(call, &[]).await, Err(ToolError::Execution(message)) if message.contains("checkpoint"))
        );
        assert!(gateway.audits().is_empty());
    }
}
