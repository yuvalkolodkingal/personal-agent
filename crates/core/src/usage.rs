//! Provider-neutral usage accounting and content-free egress records.

use chrono::{DateTime, Utc};
use personal_agent_contracts::proto::EventEnvelope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use thiserror::Error;
use uuid::Uuid;

const LEDGER_SCHEMA_VERSION: u32 = 1;
const MAX_DETAIL_RECORDS: usize = 50_000;
const MAX_EGRESS_RECORDS: usize = 25_000;

/// Token categories reported by `OpenCode`. `total` is the provider-reported
/// total when present, otherwise the saturating sum of the named categories.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
    pub total_was_reported: bool,
}

/// Cost is never estimated. `microusd=None` means the provider did not report
/// a usable cost for a step that did report token usage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportedCost {
    pub microusd: Option<u64>,
    pub status: CostStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostStatus {
    ProviderReported,
    Unknown,
}

/// One provider step. It contains counters and identifiers, never prompts,
/// responses, credentials, tool arguments, or file contents.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsageRecord {
    pub id: String,
    pub at: DateTime<Utc>,
    pub day_utc: String,
    pub session_id: String,
    pub turn_id: String,
    pub scope_key: String,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub tokens: TokenUsage,
    pub cost: ReportedCost,
}

/// Content-free outbound transfer record. Unknown byte counts remain `None`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EgressRecord {
    pub id: Uuid,
    pub at: DateTime<Utc>,
    pub source: EgressSource,
    pub destination: String,
    pub operation: String,
    pub data_class: String,
    pub size_bytes: Option<u64>,
    pub purpose: String,
    pub session_id: Option<String>,
    pub scope_key: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressSource {
    Web,
    Mcp,
    Connector,
}

/// Additive aggregate persisted for every turn, session, UTC day, and durable
/// goal/automation scope.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UsageAggregate {
    pub provider_steps: u64,
    pub tokens: TokenUsage,
    pub reported_cost_microusd: u64,
    pub unknown_cost_steps: u64,
    pub tool_calls: u64,
    pub egress_events: u64,
    pub known_egress_bytes: u64,
    pub unknown_egress_sizes: u64,
    pub providers: BTreeSet<String>,
    pub models: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct TurnContext {
    turn_id: String,
    provider_id: Option<String>,
    model_id: Option<String>,
    scope_key: String,
}

/// Encrypted accounting state. Detail lists are bounded while aggregates are
/// retained for the lifetime of the profile.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UsageLedger {
    pub schema_version: u32,
    pub records: Vec<ProviderUsageRecord>,
    pub egress: Vec<EgressRecord>,
    pub turns: BTreeMap<String, UsageAggregate>,
    pub sessions: BTreeMap<String, UsageAggregate>,
    pub days: BTreeMap<String, UsageAggregate>,
    pub scopes: BTreeMap<String, UsageAggregate>,
    active_turns: BTreeMap<String, TurnContext>,
    seen_event_ids: BTreeSet<String>,
    seen_egress_ids: BTreeSet<Uuid>,
}

impl Default for UsageLedger {
    fn default() -> Self {
        Self {
            schema_version: LEDGER_SCHEMA_VERSION,
            records: Vec::new(),
            egress: Vec::new(),
            turns: BTreeMap::new(),
            sessions: BTreeMap::new(),
            days: BTreeMap::new(),
            scopes: BTreeMap::new(),
            active_turns: BTreeMap::new(),
            seen_event_ids: BTreeSet::new(),
            seen_egress_ids: BTreeSet::new(),
        }
    }
}

impl UsageLedger {
    /// Consume one normalized runtime event. Non-accounting events are ignored.
    /// Replaying the same event ID is idempotent.
    ///
    /// # Errors
    /// Returns malformed-event or unsupported-ledger errors.
    pub fn ingest_runtime_event(&mut self, event: &EventEnvelope) -> Result<(), UsageError> {
        self.validate_schema()?;
        if self.seen_event_ids.contains(&event.event_id) {
            return Ok(());
        }
        let payload = event.payload()?;
        let session_id = event.session_id.clone().unwrap_or_else(|| "unknown".into());
        let at = event.timestamp()?;
        let scope_key = event_scope_key(event);
        if event.r#type == "response.admitted" {
            let turn_id = text_at(&payload, &["message_id", "messageID"])
                .unwrap_or(&event.event_id)
                .to_owned();
            self.active_turns.insert(
                session_id.clone(),
                TurnContext {
                    turn_id,
                    provider_id: text_at(&payload, &["provider_id", "providerID", "provider"])
                        .map(str::to_owned),
                    model_id: text_at(&payload, &["model_id", "modelID", "model"])
                        .map(str::to_owned),
                    scope_key,
                },
            );
            self.seen_event_ids.insert(event.event_id.clone());
            return Ok(());
        }
        if event.r#type == "tool.started" {
            let (turn_id, scope_key) = self.active_turns.get(&session_id).map_or_else(
                || {
                    let turn_id =
                        text_at(&payload, &["assistantMessageID", "messageID", "message_id"])
                            .unwrap_or(&event.event_id)
                            .to_owned();
                    (turn_id, scope_key)
                },
                |context| (context.turn_id.clone(), context.scope_key.clone()),
            );
            self.add_tool_call(&turn_id, &session_id, &day_string(at), &scope_key);
            if let Some(egress) = runtime_tool_egress(event, &payload, at, &scope_key) {
                self.record_egress(egress)?;
            }
            self.seen_event_ids.insert(event.event_id.clone());
            return Ok(());
        }
        if event.r#type != "response.step_completed" {
            return Ok(());
        }
        let context = self.active_turns.get(&session_id).cloned();
        let Some(tokens_value) = payload.get("tokens") else {
            if payload.get("cost").is_none() {
                return Ok(());
            }
            self.record_step(event, &payload, context.as_ref(), at, TokenUsage::default());
            return Ok(());
        };
        let tokens = normalize_tokens(tokens_value);
        self.record_step(event, &payload, context.as_ref(), at, tokens);
        Ok(())
    }

    fn record_step(
        &mut self,
        event: &EventEnvelope,
        payload: &Value,
        context: Option<&TurnContext>,
        at: DateTime<Utc>,
        tokens: TokenUsage,
    ) {
        let turn_id = context
            .map(|context| context.turn_id.clone())
            .or_else(|| {
                text_at(payload, &["assistantMessageID", "messageID", "message_id"])
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| event.event_id.clone());
        let scope_key =
            context.map_or_else(|| event_scope_key(event), |value| value.scope_key.clone());
        let provider_id = text_at(payload, &["provider_id", "providerID", "provider"])
            .map(str::to_owned)
            .or_else(|| context.and_then(|value| value.provider_id.clone()));
        let model_id = text_at(payload, &["model_id", "modelID", "model"])
            .map(str::to_owned)
            .or_else(|| context.and_then(|value| value.model_id.clone()));
        let cost = normalize_cost(payload.get("cost"));
        let day_utc = day_string(at);
        let session_id = event.session_id.clone().unwrap_or_else(|| "unknown".into());
        let record = ProviderUsageRecord {
            id: event.event_id.clone(),
            at,
            day_utc: day_utc.clone(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            scope_key: scope_key.clone(),
            provider_id,
            model_id,
            tokens,
            cost,
        };
        for aggregate in [
            self.turns.entry(turn_id).or_default(),
            self.sessions.entry(session_id).or_default(),
            self.days.entry(day_utc).or_default(),
            self.scopes.entry(scope_key).or_default(),
        ] {
            aggregate.add_usage(&record);
        }
        self.records.push(record);
        if self.records.len() > MAX_DETAIL_RECORDS {
            self.records.remove(0);
        }
        self.seen_event_ids.insert(event.event_id.clone());
    }

    /// Record a content-free outbound transfer idempotently.
    ///
    /// # Errors
    /// Rejects blank/oversized descriptive fields or unsupported ledger state.
    pub fn record_egress(&mut self, record: EgressRecord) -> Result<(), UsageError> {
        self.validate_schema()?;
        validate_egress(&record)?;
        if !self.seen_egress_ids.insert(record.id) {
            return Ok(());
        }
        let day = day_string(record.at);
        let session = record.session_id.clone();
        let scope = record.scope_key.clone();
        self.days.entry(day).or_default().add_egress(&record);
        if let Some(session) = session {
            self.sessions
                .entry(session)
                .or_default()
                .add_egress(&record);
        }
        if let Some(scope) = scope {
            self.scopes.entry(scope).or_default().add_egress(&record);
        }
        self.egress.push(record);
        if self.egress.len() > MAX_EGRESS_RECORDS {
            self.egress.remove(0);
        }
        Ok(())
    }

    /// Evaluate a durable scope after each normalized provider/tool event.
    #[must_use]
    pub fn check_budget(&self, scope_key: &str, limits: BudgetLimits) -> BudgetState {
        let usage = self.scopes.get(scope_key).cloned().unwrap_or_default();
        if let Some(limit) = limits.tokens
            && usage.tokens.total >= limit
        {
            return BudgetState::Exhausted {
                resource: BudgetResource::Tokens,
                used: usage.tokens.total,
                limit,
            };
        }
        if let Some(limit) = limits.tool_calls
            && usage.tool_calls >= u64::from(limit)
        {
            return BudgetState::Exhausted {
                resource: BudgetResource::ToolCalls,
                used: usage.tool_calls,
                limit: u64::from(limit),
            };
        }
        if let Some(limit) = limits.cost_microusd {
            if usage.unknown_cost_steps > 0 {
                return BudgetState::CostUnverifiable {
                    unknown_steps: usage.unknown_cost_steps,
                    limit_microusd: limit,
                };
            }
            if usage.reported_cost_microusd >= limit {
                return BudgetState::Exhausted {
                    resource: BudgetResource::CostMicrousd,
                    used: usage.reported_cost_microusd,
                    limit,
                };
            }
        }
        BudgetState::WithinLimits
    }

    fn validate_schema(&self) -> Result<(), UsageError> {
        if self.schema_version == LEDGER_SCHEMA_VERSION {
            Ok(())
        } else {
            Err(UsageError::UnsupportedSchema(self.schema_version))
        }
    }

    fn add_tool_call(&mut self, turn: &str, session: &str, day: &str, scope: &str) {
        for aggregate in [
            self.turns.entry(turn.to_owned()).or_default(),
            self.sessions.entry(session.to_owned()).or_default(),
            self.days.entry(day.to_owned()).or_default(),
            self.scopes.entry(scope.to_owned()).or_default(),
        ] {
            aggregate.tool_calls = aggregate.tool_calls.saturating_add(1);
        }
    }
}

impl UsageAggregate {
    fn add_usage(&mut self, record: &ProviderUsageRecord) {
        let first_step = self.provider_steps == 0;
        self.provider_steps = self.provider_steps.saturating_add(1);
        self.tokens.add(&record.tokens);
        self.tokens.total_was_reported = if first_step {
            record.tokens.total_was_reported
        } else {
            self.tokens.total_was_reported && record.tokens.total_was_reported
        };
        if let Some(cost) = record.cost.microusd {
            self.reported_cost_microusd = self.reported_cost_microusd.saturating_add(cost);
        } else {
            self.unknown_cost_steps = self.unknown_cost_steps.saturating_add(1);
        }
        if let Some(provider) = &record.provider_id {
            self.providers.insert(provider.clone());
        }
        if let Some(model) = &record.model_id {
            self.models.insert(model.clone());
        }
    }

    fn add_egress(&mut self, record: &EgressRecord) {
        self.egress_events = self.egress_events.saturating_add(1);
        if let Some(size) = record.size_bytes {
            self.known_egress_bytes = self.known_egress_bytes.saturating_add(size);
        } else {
            self.unknown_egress_sizes = self.unknown_egress_sizes.saturating_add(1);
        }
    }
}

impl TokenUsage {
    fn add(&mut self, value: &Self) {
        self.input = self.input.saturating_add(value.input);
        self.output = self.output.saturating_add(value.output);
        self.reasoning = self.reasoning.saturating_add(value.reasoning);
        self.cache_read = self.cache_read.saturating_add(value.cache_read);
        self.cache_write = self.cache_write.saturating_add(value.cache_write);
        self.total = self.total.saturating_add(value.total);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BudgetLimits {
    pub tokens: Option<u64>,
    pub cost_microusd: Option<u64>,
    pub tool_calls: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BudgetState {
    WithinLimits,
    Exhausted {
        resource: BudgetResource,
        used: u64,
        limit: u64,
    },
    CostUnverifiable {
        unknown_steps: u64,
        limit_microusd: u64,
    },
}

impl BudgetState {
    #[must_use]
    pub fn exceeded(self) -> bool {
        !matches!(self, Self::WithinLimits)
    }
}

impl fmt::Display for BudgetState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WithinLimits => formatter.write_str("within configured budgets"),
            Self::Exhausted {
                resource,
                used,
                limit,
            } => write!(formatter, "{resource} budget exhausted ({used}/{limit})"),
            Self::CostUnverifiable {
                unknown_steps,
                limit_microusd,
            } => write!(
                formatter,
                "cost budget cannot be verified: {unknown_steps} provider step(s) omitted cost; configured ceiling is {limit_microusd} micro-USD"
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetResource {
    Tokens,
    CostMicrousd,
    ToolCalls,
}

impl fmt::Display for BudgetResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Tokens => "token",
            Self::CostMicrousd => "cost",
            Self::ToolCalls => "tool-call",
        })
    }
}

#[derive(Debug, Error)]
pub enum UsageError {
    #[error(transparent)]
    Event(#[from] personal_agent_contracts::EventError),
    #[error("unsupported usage ledger schema {0}")]
    UnsupportedSchema(u32),
    #[error("egress record contains an invalid descriptive field")]
    InvalidEgress,
}

fn normalize_tokens(value: &Value) -> TokenUsage {
    let input = unsigned_at(value, &["input", "input_tokens", "inputTokens"]);
    let output = unsigned_at(value, &["output", "output_tokens", "outputTokens"]);
    let reasoning = unsigned_at(value, &["reasoning", "reasoning_tokens", "reasoningTokens"]);
    let cache_read = unsigned_at(value, &["cache_read", "cacheRead"])
        .max(unsigned_at_path(value, &["cache", "read"]));
    let cache_write = unsigned_at(value, &["cache_write", "cacheWrite"])
        .max(unsigned_at_path(value, &["cache", "write"]));
    let sum = input
        .saturating_add(output)
        .saturating_add(reasoning)
        .saturating_add(cache_read)
        .saturating_add(cache_write);
    let explicit_total = value.get("total").and_then(unsigned);
    TokenUsage {
        input,
        output,
        reasoning,
        cache_read,
        cache_write,
        total: explicit_total.unwrap_or(sum),
        total_was_reported: explicit_total.is_some(),
    }
}

fn normalize_cost(value: Option<&Value>) -> ReportedCost {
    let microusd = value
        .and_then(Value::as_f64)
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .and_then(usd_to_microusd);
    ReportedCost {
        status: if microusd.is_some() {
            CostStatus::ProviderReported
        } else {
            CostStatus::Unknown
        },
        microusd,
    }
}

fn usd_to_microusd(cost: f64) -> Option<u64> {
    let decimal = format!("{cost:.6}");
    let (whole, fractional) = decimal.split_once('.')?;
    whole
        .parse::<u64>()
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(fractional.parse::<u64>().ok()?)
}

fn unsigned_at(value: &Value, names: &[&str]) -> u64 {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(unsigned))
        .unwrap_or(0)
}

fn unsigned_at_path(value: &Value, path: &[&str]) -> u64 {
    path.iter()
        .try_fold(value, |cursor, name| cursor.get(*name))
        .and_then(unsigned)
        .unwrap_or(0)
}

fn unsigned(value: &Value) -> Option<u64> {
    value.as_u64()
}

fn text_at<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
}

fn day_string(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d").to_string()
}

fn event_scope_key(event: &EventEnvelope) -> String {
    if let Some(goal_id) = &event.goal_id {
        return format!("goal:{goal_id}");
    }
    if let Some(agent_id) = event
        .agent_id
        .as_deref()
        .and_then(|value| value.strip_prefix("automation:"))
    {
        return format!("automation:{agent_id}");
    }
    format!(
        "session:{}",
        event.session_id.as_deref().unwrap_or("unknown")
    )
}

fn runtime_tool_egress(
    event: &EventEnvelope,
    payload: &Value,
    at: DateTime<Utc>,
    scope_key: &str,
) -> Option<EgressRecord> {
    let tool = text_at(payload, &["tool"])?;
    let provider = text_at(payload, &["provider"]);
    let lowercase = tool.to_ascii_lowercase();
    let source = if lowercase.contains("web") || lowercase.contains("browser") {
        EgressSource::Web
    } else if provider.is_some() {
        EgressSource::Mcp
    } else {
        return None;
    };
    Some(EgressRecord {
        id: Uuid::now_v7(),
        at,
        source,
        destination: bounded_label(provider.unwrap_or("runtime-managed web"), "unknown"),
        operation: bounded_label(tool, "tool call"),
        data_class: "tool arguments".into(),
        size_bytes: None,
        purpose: "agent tool invocation".into(),
        session_id: event.session_id.clone(),
        scope_key: Some(scope_key.to_owned()),
    })
}

fn bounded_label(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    let value = if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    };
    value.chars().take(256).collect()
}

fn validate_egress(record: &EgressRecord) -> Result<(), UsageError> {
    for value in [
        &record.destination,
        &record.operation,
        &record.data_class,
        &record.purpose,
    ] {
        if value.trim().is_empty() || value.len() > 512 || value.contains(['\n', '\r']) {
            return Err(UsageError::InvalidEgress);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(sequence: u64, kind: &str, payload: &Value) -> EventEnvelope {
        let mut event =
            EventEnvelope::new(sequence, "opencode", "default", kind, payload).expect("event");
        event.session_id = Some("ses_1".into());
        event
    }

    #[test]
    fn provider_metrics_roll_up_by_turn_session_day_and_scope() {
        let mut ledger = UsageLedger::default();
        let mut admitted = event(
            1,
            "response.admitted",
            &json!({"message_id":"msg_1", "provider_id":"openai", "model_id":"gpt-5"}),
        );
        admitted.goal_id = Some("goal-1".into());
        ledger.ingest_runtime_event(&admitted).expect("admitted");
        let mut step = event(
            2,
            "response.step_completed",
            &json!({
                "tokens":{"input":100,"output":20,"reasoning":5,"cache":{"read":10,"write":2}},
                "cost":0.012_345,
            }),
        );
        step.goal_id = Some("goal-1".into());
        ledger.ingest_runtime_event(&step).expect("step");
        ledger.ingest_runtime_event(&step).expect("idempotent");

        let turn = &ledger.turns["msg_1"];
        assert_eq!(turn.provider_steps, 1);
        assert_eq!(turn.tokens.total, 137);
        assert_eq!(turn.reported_cost_microusd, 12_345);
        assert_eq!(ledger.sessions["ses_1"], *turn);
        assert_eq!(ledger.scopes["goal:goal-1"], *turn);
        assert_eq!(ledger.days.len(), 1);
        assert_eq!(ledger.records[0].provider_id.as_deref(), Some("openai"));
    }

    #[test]
    fn missing_cost_is_unknown_and_fails_closed_for_cost_budgets() {
        let mut ledger = UsageLedger::default();
        let mut step = event(
            1,
            "response.step_completed",
            &json!({"tokens":{"input":4,"output":2}}),
        );
        step.agent_id = Some("automation:nightly".into());
        ledger.ingest_runtime_event(&step).expect("step");
        assert_eq!(ledger.records[0].cost.status, CostStatus::Unknown);
        assert_eq!(ledger.records[0].cost.microusd, None);
        assert_eq!(
            ledger.check_budget(
                "automation:nightly",
                BudgetLimits {
                    cost_microusd: Some(50_000),
                    ..BudgetLimits::default()
                }
            ),
            BudgetState::CostUnverifiable {
                unknown_steps: 1,
                limit_microusd: 50_000,
            }
        );
    }

    #[test]
    fn content_free_egress_preserves_unknown_sizes_and_rejects_multiline_fields() {
        let mut ledger = UsageLedger::default();
        let record = EgressRecord {
            id: Uuid::now_v7(),
            at: Utc::now(),
            source: EgressSource::Mcp,
            destination: "github".into(),
            operation: "issues.list".into(),
            data_class: "tool arguments".into(),
            size_bytes: None,
            purpose: "user-requested MCP tool".into(),
            session_id: Some("ses_1".into()),
            scope_key: None,
        };
        ledger.record_egress(record.clone()).expect("egress");
        ledger.record_egress(record).expect("idempotent");
        assert_eq!(ledger.egress.len(), 1);
        assert_eq!(ledger.sessions["ses_1"].unknown_egress_sizes, 1);

        let mut invalid = ledger.egress[0].clone();
        invalid.id = Uuid::now_v7();
        invalid.destination = "secret\ncontent".into();
        assert!(matches!(
            ledger.record_egress(invalid),
            Err(UsageError::InvalidEgress)
        ));
    }
}
