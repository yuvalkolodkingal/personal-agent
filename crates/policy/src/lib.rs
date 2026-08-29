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
    use chrono::Duration;

    const EFFECTS: [Effect; 7] = [
        Effect::Observe,
        Effect::LocalWrite,
        Effect::ExternalWrite,
        Effect::Communication,
        Effect::Commerce,
        Effect::Security,
        Effect::Power,
    ];
    const RISKS: [Risk; 4] = [
        Risk::Read,
        Risk::Reversible,
        Risk::Consequential,
        Risk::Irreversible,
    ];
    const ZONES: [DataZone; 7] = [
        DataZone::UserInstruction,
        DataZone::TrustedLocalState,
        DataZone::PrivateMemory,
        DataZone::Secret,
        DataZone::ConnectorData,
        DataZone::UntrustedContent,
        DataZone::AgentGenerated,
    ];

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

    fn context<'a>(
        goal_id: Uuid,
        task_id: Option<Uuid>,
        tool: &'a ToolDescriptor,
        target: &'a str,
        active_input_zones: &'a BTreeSet<DataZone>,
        granted_scopes: &'a BTreeSet<String>,
    ) -> CallContext<'a> {
        CallContext {
            goal_id,
            task_id,
            tool,
            target,
            active_input_zones,
            granted_scopes,
            estimated_cost_usd: 0.0,
            background: false,
            user_present: true,
            checkpoint_available: true,
        }
    }

    #[test]
    fn communications_always_ask_without_consent() {
        let tool = descriptor(Effect::Communication, Risk::Consequential);
        let scopes = ["mail.send".into()].into();
        let zones = [DataZone::UserInstruction].into();
        let call = context(
            Uuid::now_v7(),
            None,
            &tool,
            "alice@example.com",
            &zones,
            &scopes,
        );
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
        let call = context(Uuid::now_v7(), None, &tool, "keychain", &zones, &scopes);
        assert!(matches!(
            PolicyEngine.decide(&call, &[]),
            PolicyDecision::Ask { .. }
        ));
    }

    #[test]
    fn every_effect_risk_and_zone_combination_matches_the_documented_decision() {
        let scopes = ["mail.send".into()].into();
        let goal_id = Uuid::now_v7();
        let mut checked = 0;

        for effect in EFFECTS {
            for risk in RISKS {
                for zone_mask in 0..(1_usize << ZONES.len()) {
                    let zones = ZONES
                        .iter()
                        .enumerate()
                        .filter_map(|(index, zone)| {
                            (zone_mask & (1 << index) != 0).then_some(*zone)
                        })
                        .collect::<BTreeSet<_>>();
                    let mut tool = descriptor(effect, risk);
                    tool.reversible = risk == Risk::Reversible;
                    tool.zones_read.clone_from(&zones);
                    let call = context(goal_id, None, &tool, "fixture", &zones, &scopes);

                    let is_cross_zone = zones.contains(&DataZone::UntrustedContent)
                        && (zones.contains(&DataZone::Secret)
                            || zones.contains(&DataZone::PrivateMemory)
                            || matches!(
                                effect,
                                Effect::Communication | Effect::Commerce | Effect::Security
                            ));
                    let is_always_confirm = matches!(
                        effect,
                        Effect::Communication | Effect::Commerce | Effect::Security | Effect::Power
                    ) || matches!(
                        risk,
                        Risk::Consequential | Risk::Irreversible
                    ) || (effect == Effect::ExternalWrite
                        && !tool.reversible);
                    let expected = if is_cross_zone {
                        PolicyDecision::Ask {
                            reason: "untrusted content is controlling a cross-zone action".into(),
                        }
                    } else if is_always_confirm {
                        PolicyDecision::Ask {
                            reason: "consequential effects require explicit scoped consent".into(),
                        }
                    } else {
                        PolicyDecision::Allow {
                            reason: "bounded policy permits read-only or reversible local work"
                                .into(),
                            consent_id: None,
                        }
                    };

                    assert_eq!(
                        PolicyEngine.decide(&call, &[]),
                        expected,
                        "effect={effect:?} risk={risk:?} zones={zones:?}"
                    );
                    checked += 1;
                }
            }
        }

        assert_eq!(checked, EFFECTS.len() * RISKS.len() * (1 << ZONES.len()));
    }

    #[test]
    fn policy_gate_precedence_and_consent_cover_every_decide_branch() {
        let goal_id = Uuid::now_v7();
        let task_id = Uuid::now_v7();
        let zones = [DataZone::UserInstruction].into();
        let scopes = ["mail.send".into()].into();
        let no_scopes = BTreeSet::new();
        let mut tool = descriptor(Effect::LocalWrite, Risk::Reversible);
        tool.reversible = true;
        tool.user_presence = true;

        let missing_scope = context(goal_id, Some(task_id), &tool, "draft", &zones, &no_scopes);
        assert_eq!(
            PolicyEngine.decide(&missing_scope, &[]),
            PolicyDecision::Deny {
                reason: "tool requires capability scopes not granted to this task".into(),
            }
        );

        let mut missing_presence = context(goal_id, Some(task_id), &tool, "draft", &zones, &scopes);
        missing_presence.user_present = false;
        assert_eq!(
            PolicyEngine.decide(&missing_presence, &[]),
            PolicyDecision::Deny {
                reason: "this tool requires the user to be present".into(),
            }
        );

        let mut missing_checkpoint =
            context(goal_id, Some(task_id), &tool, "draft", &zones, &scopes);
        missing_checkpoint.checkpoint_available = false;
        assert_eq!(
            PolicyEngine.decide(&missing_checkpoint, &[]),
            PolicyDecision::Deny {
                reason: "a checkpoint is required before the first reversible mutation".into(),
            }
        );

        let consent_id = Uuid::now_v7();
        let mut consequential_tool = descriptor(Effect::Communication, Risk::Consequential);
        consequential_tool.user_presence = false;
        let consequential_call = context(
            goal_id,
            Some(task_id),
            &consequential_tool,
            "alice@example.com",
            &zones,
            &scopes,
        );
        let grant = ConsentGrant {
            id: consent_id,
            goal_id,
            task_id: Some(task_id),
            tool_ids: [consequential_tool.id.clone()].into(),
            effects: [consequential_tool.effect].into(),
            target_patterns: ["alice@example.com".into()].into(),
            expires_at: Utc::now() + Duration::hours(1),
            maximum_calls: 1,
            calls_used: 0,
            cost_ceiling_usd: Some(1.0),
            background: false,
            revoked: false,
        };
        assert_eq!(
            PolicyEngine.decide(&consequential_call, &[grant]),
            PolicyDecision::Allow {
                reason: "covered by scoped consent".into(),
                consent_id: Some(consent_id),
            }
        );
    }

    #[test]
    fn consent_grant_expiry_count_and_cost_boundaries_are_closed() {
        let goal_id = Uuid::now_v7();
        let task_id = Uuid::now_v7();
        let tool = descriptor(Effect::Commerce, Risk::Consequential);
        let zones = [DataZone::UserInstruction].into();
        let scopes = ["mail.send".into()].into();
        let mut call = context(goal_id, Some(task_id), &tool, "orders/42", &zones, &scopes);
        call.background = true;
        call.estimated_cost_usd = 10.0;
        let base = ConsentGrant {
            id: Uuid::now_v7(),
            goal_id,
            task_id: Some(task_id),
            tool_ids: [tool.id.clone()].into(),
            effects: [tool.effect].into(),
            target_patterns: ["orders/*".into()].into(),
            expires_at: Utc::now() + Duration::hours(1),
            maximum_calls: 2,
            calls_used: 1,
            cost_ceiling_usd: Some(10.0),
            background: true,
            revoked: false,
        };

        assert!(base.covers(&call), "the inclusive cost ceiling must cover");

        let mut expired = base.clone();
        expired.expires_at = Utc::now() - Duration::nanoseconds(1);
        assert!(!expired.covers(&call));

        let mut at_call_limit = base.clone();
        at_call_limit.calls_used = at_call_limit.maximum_calls;
        assert!(!at_call_limit.covers(&call));

        call.estimated_cost_usd = 10.000_001;
        assert!(!base.covers(&call));
        call.estimated_cost_usd = f64::NAN;
        assert!(!base.covers(&call));

        let mut unlimited_cost = base.clone();
        unlimited_cost.cost_ceiling_usd = None;
        call.estimated_cost_usd = f64::MAX;
        assert!(unlimited_cost.covers(&call));
    }

    #[test]
    fn consent_grant_rejects_every_mismatched_dimension() {
        let goal_id = Uuid::now_v7();
        let task_id = Uuid::now_v7();
        let tool = descriptor(Effect::Communication, Risk::Consequential);
        let zones = [DataZone::UserInstruction].into();
        let scopes = ["mail.send".into()].into();
        let mut call = context(
            goal_id,
            Some(task_id),
            &tool,
            "alice@example.com",
            &zones,
            &scopes,
        );
        let base = ConsentGrant {
            id: Uuid::now_v7(),
            goal_id,
            task_id: Some(task_id),
            tool_ids: [tool.id.clone()].into(),
            effects: [tool.effect].into(),
            target_patterns: ["alice@example.com".into()].into(),
            expires_at: Utc::now() + Duration::hours(1),
            maximum_calls: 1,
            calls_used: 0,
            cost_ceiling_usd: Some(1.0),
            background: false,
            revoked: false,
        };

        let mut variants = Vec::new();
        let mut revoked = base.clone();
        revoked.revoked = true;
        variants.push(revoked);
        let mut wrong_goal = base.clone();
        wrong_goal.goal_id = Uuid::now_v7();
        variants.push(wrong_goal);
        let mut wrong_task = base.clone();
        wrong_task.task_id = Some(Uuid::now_v7());
        variants.push(wrong_task);
        let mut wrong_tool = base.clone();
        wrong_tool.tool_ids = ["other.tool".into()].into();
        variants.push(wrong_tool);
        let mut wrong_effect = base.clone();
        wrong_effect.effects = [Effect::Observe].into();
        variants.push(wrong_effect);
        let mut wrong_target = base.clone();
        wrong_target.target_patterns = ["bob@example.com".into()].into();
        variants.push(wrong_target);
        for variant in &variants {
            assert!(!variant.covers(&call));
        }

        call.background = true;
        assert!(!base.covers(&call));
        call.background = false;
        assert!(base.covers(&call));

        let mut any_task = base;
        any_task.task_id = None;
        assert!(any_task.covers(&call));
    }
}
