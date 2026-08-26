//! Native tool gateway. No effectful call bypasses this pipeline.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use personal_agent_policy::{
    CallContext, ConsentGrant, DataZone, PolicyDecision, PolicyEngine, ToolDescriptor,
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
}

/// Registered gateway with one fixed core policy engine.
#[derive(Default)]
pub struct ToolGateway {
    policy: PolicyEngine,
    tools: HashMap<String, Arc<dyn ToolImplementation>>,
    audits: Vec<ToolAudit>,
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
                    reason: error.to_string(),
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
            tool_id: call.tool_id,
            target: call.target,
            at: Utc::now(),
            decision: decision_label,
            succeeded: true,
            output_bytes: encoded.len(),
            reason: "postcondition verified".into(),
        });
        Ok(ToolOutput {
            value: filtered,
            rollback_id,
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
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_agent_policy::{Effect, Idempotency, Risk};
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
}
