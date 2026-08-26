//! Capability, consent, and data-flow policy for every tool invocation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

/// Trust label carried by every input and output.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DataZone {
    UserInstruction,
    TrustedLocalState,
    PrivateMemory,
    Secret,
    ConnectorData,
    UntrustedContent,
    AgentGenerated,
}

/// Consequence level independent of the tool's mechanism.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Risk {
    Read,
    Reversible,
    Consequential,
    Irreversible,
}

/// Externally meaningful effect category.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    Observe,
    LocalWrite,
    ExternalWrite,
    Communication,
    Commerce,
    Security,
    Power,
}

/// Whether automatic retry can duplicate an effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Idempotency {
    Safe,
    WithKey,
    Unsafe,
}

/// Policy-relevant tool declaration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub id: String,
    pub version: String,
    pub description: String,
    pub scopes: BTreeSet<String>,
    pub risk: Risk,
    pub effect: Effect,
    pub idempotency: Idempotency,
    pub reversible: bool,
    pub zones_read: BTreeSet<DataZone>,
    pub zones_written: BTreeSet<DataZone>,
    pub user_presence: bool,
}

/// Scoped and revocable authorization.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConsentGrant {
    pub id: Uuid,
    pub goal_id: Uuid,
    pub task_id: Option<Uuid>,
    pub tool_ids: BTreeSet<String>,
    pub effects: BTreeSet<Effect>,
    pub target_patterns: BTreeSet<String>,
    pub expires_at: DateTime<Utc>,
    pub maximum_calls: u32,
    pub calls_used: u32,
    pub cost_ceiling_usd: Option<f64>,
    pub background: bool,
    pub revoked: bool,
}

impl ConsentGrant {
    /// Return true only when every dimension of the grant covers the call.
    #[must_use]
    pub fn covers(&self, call: &CallContext<'_>) -> bool {
        !self.revoked
            && self.expires_at > Utc::now()
            && self.calls_used < self.maximum_calls
            && self.goal_id == call.goal_id
            && self.task_id.is_none_or(|id| Some(id) == call.task_id)
            && self.tool_ids.contains(&call.tool.id)
            && self.effects.contains(&call.tool.effect)
            && (!call.background || self.background)
            && self
                .cost_ceiling_usd
                .is_none_or(|ceiling| call.estimated_cost_usd <= ceiling)
            && self
                .target_patterns
                .iter()
                .any(|pattern| target_matches(pattern, call.target))
    }
}

fn target_matches(pattern: &str, target: &str) -> bool {
    pattern == target
        || pattern
            .strip_suffix('*')
            .is_some_and(|prefix| target.starts_with(prefix))
}

/// Inputs used to make one decision.
pub struct CallContext<'a> {
    pub goal_id: Uuid,
    pub task_id: Option<Uuid>,
    pub tool: &'a ToolDescriptor,
    pub target: &'a str,
    pub active_input_zones: &'a BTreeSet<DataZone>,
    pub granted_scopes: &'a BTreeSet<String>,
    pub estimated_cost_usd: f64,
    pub background: bool,
    pub user_present: bool,
    pub checkpoint_available: bool,
}

/// Explainable policy outcome recorded in the audit log.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum PolicyDecision {
    Allow {
        reason: String,
        consent_id: Option<Uuid>,
    },
    Ask {
        reason: String,
    },
    Deny {
        reason: String,
    },
}

/// Bounded default policy. Agents and plugins cannot replace it.
#[derive(Default)]
pub struct PolicyEngine;

impl PolicyEngine {
    /// Evaluate capability, trust-zone, presence, checkpoint, and consent gates.
    #[must_use]
    pub fn decide(&self, call: &CallContext<'_>, grants: &[ConsentGrant]) -> PolicyDecision {
        if !call.tool.scopes.is_subset(call.granted_scopes) {
            return PolicyDecision::Deny {
                reason: "tool requires capability scopes not granted to this task".into(),
            };
        }
        if call.tool.user_presence && !call.user_present {
            return PolicyDecision::Deny {
                reason: "this tool requires the user to be present".into(),
            };
        }
        if matches!(
            call.tool.risk,
            Risk::Reversible | Risk::Consequential | Risk::Irreversible
        ) && call.tool.effect != Effect::Observe
            && !call.checkpoint_available
            && call.tool.reversible
        {
            return PolicyDecision::Deny {
                reason: "a checkpoint is required before the first reversible mutation".into(),
            };
        }
        let untrusted = call
            .active_input_zones
            .contains(&DataZone::UntrustedContent);
        let cross_zone_effect = call.tool.zones_read.contains(&DataZone::Secret)
            || call.tool.zones_read.contains(&DataZone::PrivateMemory)
            || matches!(
                call.tool.effect,
                Effect::Communication | Effect::Commerce | Effect::Security
            );
        if untrusted && cross_zone_effect {
            return PolicyDecision::Ask {
                reason: "untrusted content is controlling a cross-zone action".into(),
            };
        }
        let always_confirm =
            matches!(
                call.tool.effect,
                Effect::Communication | Effect::Commerce | Effect::Security | Effect::Power
            ) || matches!(call.tool.risk, Risk::Consequential | Risk::Irreversible)
                || (call.tool.effect == Effect::ExternalWrite && !call.tool.reversible);
        if let Some(grant) = grants.iter().find(|grant| grant.covers(call)) {
            return PolicyDecision::Allow {
                reason: "covered by scoped consent".into(),
                consent_id: Some(grant.id),
            };
        }
        if always_confirm {
            return PolicyDecision::Ask {
                reason: "consequential effects require explicit scoped consent".into(),
            };
        }
        PolicyDecision::Allow {
            reason: "bounded policy permits read-only or reversible local work".into(),
            consent_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(effect: Effect, risk: Risk) -> ToolDescriptor {
        ToolDescriptor {
            id: "mail.send".into(),
            version: "1.0.0".into(),
            description: "send mail".into(),
            scopes: ["mail.send".into()].into(),
            risk,
            effect,
            idempotency: Idempotency::WithKey,
            reversible: false,
            zones_read: [DataZone::UserInstruction].into(),
            zones_written: [DataZone::ConnectorData].into(),
            user_presence: false,
        }
    }

    #[test]
    fn communications_always_ask_without_consent() {
        let tool = descriptor(Effect::Communication, Risk::Consequential);
        let scopes = ["mail.send".into()].into();
        let zones = [DataZone::UserInstruction].into();
        let call = CallContext {
            goal_id: Uuid::now_v7(),
            task_id: None,
            tool: &tool,
            target: "alice@example.com",
            active_input_zones: &zones,
            granted_scopes: &scopes,
            estimated_cost_usd: 0.0,
            background: false,
            user_present: true,
            checkpoint_available: true,
        };
        assert!(matches!(
            PolicyEngine.decide(&call, &[]),
            PolicyDecision::Ask { .. }
        ));
    }

    #[test]
    fn untrusted_content_cannot_read_secrets_on_its_own() {
        let mut tool = descriptor(Effect::Observe, Risk::Read);
        tool.zones_read.insert(DataZone::Secret);
        let scopes = ["mail.send".into()].into();
        let zones = [DataZone::UntrustedContent].into();
        let call = CallContext {
            goal_id: Uuid::now_v7(),
            task_id: None,
            tool: &tool,
            target: "keychain",
            active_input_zones: &zones,
            granted_scopes: &scopes,
            estimated_cost_usd: 0.0,
            background: false,
            user_present: true,
            checkpoint_available: true,
        };
        assert!(matches!(
            PolicyEngine.decide(&call, &[]),
            PolicyDecision::Ask { .. }
        ));
    }
}
