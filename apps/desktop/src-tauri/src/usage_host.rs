//! Usage and content-free egress dashboard/export boundary.

use super::DesktopState;
use chrono::Utc;
use personal_agent_core::{EgressRecord, ProviderUsageRecord, UsageAggregate, UsagePageRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use tauri::Manager as _;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct UsageSnapshotView {
    records: Vec<ProviderUsageRecord>,
    egress: Vec<EgressRecord>,
    turns: BTreeMap<String, UsageAggregate>,
    sessions: BTreeMap<String, UsageAggregate>,
    days: BTreeMap<String, UsageAggregate>,
    scopes: BTreeMap<String, UsageAggregate>,
    usage_total: u64,
    egress_total: u64,
    limit: usize,
    offset: usize,
    pricing_policy: &'static str,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct UsageFilter {
    #[serde(default)]
    from_day: Option<String>,
    #[serde(default)]
    to_day: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct UsageExportResult {
    path: String,
    usage_records: usize,
    egress_records: usize,
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands deserialize and inject owned IPC arguments.
#[allow(clippy::too_many_arguments)] // Flat optional IPC filters remain backward-compatible.
pub(crate) fn usage_snapshot(
    limit: Option<usize>,
    offset: Option<usize>,
    from: Option<String>,
    to: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    session: Option<String>,
    source: Option<String>,
    desktop: tauri::State<'_, DesktopState>,
) -> Result<UsageSnapshotView, String> {
    let limit = limit.unwrap_or(50).clamp(1, 200);
    let offset = offset.unwrap_or_default();
    let (from, to) = normalize_range(from, to)?;
    let provider = normalize_optional(provider);
    let model = normalize_optional(model);
    let session = normalize_optional(session);
    let source = normalize_optional(source);
    let profile = desktop
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?;
    let page = profile
        .usage_page(UsagePageRequest {
            limit,
            offset,
            from: from.as_deref(),
            to: to.as_deref(),
            provider: provider.as_deref(),
            model: model.as_deref(),
            session: session.as_deref(),
            source: source.as_deref(),
        })
        .map_err(|error| error.to_string())?;
    let ledger = page.ledger;
    Ok(UsageSnapshotView {
        records: ledger.records,
        egress: ledger.egress,
        turns: ledger.turns,
        sessions: ledger.sessions,
        days: ledger.days,
        scopes: ledger.scopes,
        usage_total: page.usage_total,
        egress_total: page.egress_total,
        limit: page.limit,
        offset: page.offset,
        pricing_policy: "Only provider-reported cost is totaled. Missing or invalid cost remains explicitly unknown.",
    })
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands deserialize and inject owned IPC arguments.
pub(crate) fn usage_export(
    filter: UsageFilter,
    desktop: tauri::State<'_, DesktopState>,
    app: tauri::AppHandle,
) -> Result<UsageExportResult, String> {
    let profile = desktop
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?;
    let (from, to) = normalize_range(filter.from_day.clone(), filter.to_day.clone())?;
    let provider = normalize_optional(filter.provider.clone());
    let model = normalize_optional(filter.model.clone());
    let session = normalize_optional(filter.session.clone());
    let source = normalize_optional(filter.source.clone());
    let page = profile
        .usage_page(UsagePageRequest {
            limit: usize::MAX,
            offset: 0,
            from: from.as_deref(),
            to: to.as_deref(),
            provider: provider.as_deref(),
            model: model.as_deref(),
            session: session.as_deref(),
            source: source.as_deref(),
        })
        .map_err(|error| error.to_string())?;
    drop(profile);
    let body = filtered_export(&page.ledger.records, &page.ledger.egress, &filter);
    let directory = app
        .path()
        .download_dir()
        .or_else(|_| app.path().app_data_dir())
        .map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let filename = format!(
        "personal-agent-usage-egress-{}.json",
        Utc::now().format("%Y%m%dT%H%M%SZ")
    );
    let path = directory.join(filename);
    let encoded = serde_json::to_vec_pretty(&body).map_err(|error| error.to_string())?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| error.to_string())?;
    file.write_all(&encoded)
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(UsageExportResult {
        path: path.display().to_string(),
        usage_records: body["usage"].as_array().map_or(0, Vec::len),
        egress_records: body["egress"].as_array().map_or(0, Vec::len),
    })
}

fn filtered_export(
    usage: &[ProviderUsageRecord],
    egress: &[EgressRecord],
    filter: &UsageFilter,
) -> Value {
    let usage = usage
        .iter()
        .filter(|record| usage_matches(record, filter))
        .collect::<Vec<_>>();
    let egress = egress
        .iter()
        .filter(|record| egress_matches(record, filter))
        .collect::<Vec<_>>();
    json!({
        "schema_version": 1,
        "product": "Personal Agent",
        "exported_at": Utc::now(),
        "pricing_policy": "provider_reported_only",
        "content_policy": "No prompts, responses, credentials, tool arguments, file contents, or connector payloads are included.",
        "filter": filter,
        "usage": usage,
        "egress": egress,
    })
}

fn usage_matches(record: &ProviderUsageRecord, filter: &UsageFilter) -> bool {
    in_day_range(&record.day_utc, filter)
        && optional_contains(record.provider_id.as_deref(), filter.provider.as_deref())
        && optional_contains(record.model_id.as_deref(), filter.model.as_deref())
        && optional_contains(Some(&record.session_id), filter.session.as_deref())
}

fn egress_matches(record: &EgressRecord, filter: &UsageFilter) -> bool {
    in_day_range(&record.at.format("%Y-%m-%d").to_string(), filter)
        && optional_contains(record.session_id.as_deref(), filter.session.as_deref())
        && filter
            .source
            .as_deref()
            .is_none_or(|needle| format!("{:?}", record.source).eq_ignore_ascii_case(needle.trim()))
}

fn in_day_range(day: &str, filter: &UsageFilter) -> bool {
    filter
        .from_day
        .as_deref()
        .is_none_or(|from| day >= from.trim())
        && filter.to_day.as_deref().is_none_or(|to| day <= to.trim())
}

fn optional_contains(value: Option<&str>, needle: Option<&str>) -> bool {
    let Some(needle) = needle.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    value.is_some_and(|value| {
        value
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    })
}

fn normalize_range(
    from: Option<String>,
    to: Option<String>,
) -> Result<(Option<String>, Option<String>), String> {
    let normalize = |value: Option<String>| -> Result<Option<String>, String> {
        let Some(value) = value.map(|value| value.trim().to_owned()) else {
            return Ok(None);
        };
        if value.is_empty() {
            return Ok(None);
        }
        chrono::NaiveDate::parse_from_str(&value, "%Y-%m-%d")
            .map_err(|_| "usage date filters must use YYYY-MM-DD".to_owned())?;
        Ok(Some(value))
    };
    let from = normalize(from)?;
    let to = normalize(to)?;
    if from
        .as_deref()
        .zip(to.as_deref())
        .is_some_and(|(from, to)| from > to)
    {
        return Err("usage date filter start must not be after its end".into());
    }
    Ok((from, to))
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_agent_core::{CostStatus, EgressSource, ReportedCost, TokenUsage};
    use uuid::Uuid;

    #[test]
    fn export_filters_metrics_without_content_fields() {
        let at = "2026-08-28T10:00:00Z".parse().expect("timestamp");
        let usage = vec![ProviderUsageRecord {
            id: "event-1".into(),
            at,
            day_utc: "2026-08-28".into(),
            session_id: "ses_1".into(),
            turn_id: "msg_1".into(),
            scope_key: "session:ses_1".into(),
            provider_id: Some("openai".into()),
            model_id: Some("gpt-5".into()),
            tokens: TokenUsage {
                input: 10,
                output: 2,
                total: 12,
                ..TokenUsage::default()
            },
            cost: ReportedCost {
                microusd: Some(42),
                status: CostStatus::ProviderReported,
            },
        }];
        let egress = vec![EgressRecord {
            id: Uuid::now_v7(),
            at,
            source: EgressSource::Mcp,
            destination: "github".into(),
            operation: "issues.list".into(),
            data_class: "tool arguments".into(),
            size_bytes: None,
            purpose: "user-requested MCP tool".into(),
            session_id: Some("ses_1".into()),
            scope_key: None,
        }];
        let value = filtered_export(
            &usage,
            &egress,
            &UsageFilter {
                provider: Some("OpenAI".into()),
                source: Some("mcp".into()),
                ..UsageFilter::default()
            },
        );
        assert_eq!(value["usage"].as_array().unwrap().len(), 1);
        assert_eq!(value["egress"].as_array().unwrap().len(), 1);
        let encoded = serde_json::to_string(&json!({
            "usage": value["usage"],
            "egress": value["egress"],
        }))
        .expect("encode records");
        for forbidden in ["prompt", "response", "credential", "tool_arguments"] {
            assert!(!encoded.contains(forbidden));
        }
    }
}
