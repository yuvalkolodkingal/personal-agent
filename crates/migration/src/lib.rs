//! Read-only legacy discovery and consented, idempotent migration.
//!
//! Discovery reads filesystem metadata only. Personal payloads are opened only
//! after [`MigrationConsent::copy_personal_data`] is true, and they are handed
//! to a caller-provided sink so production can persist them inside the
//! encrypted native boundary. Reports contain provenance and hashes, never the
//! imported content.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashSet},
    fmt::{self, Write as _},
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};
use thiserror::Error;
use uuid::Uuid;

/// Version of the migration plan, record, and report contract.
pub const MIGRATION_SCHEMA_VERSION: u32 = 1;
const MAX_DISCOVERED_FILES: usize = 100_000;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HISTORY_LINE_BYTES: usize = 1_048_576;
const MAX_EXTENSION_FILES: usize = 256;

/// The independently located legacy configuration, data, and optional auth roots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LegacyRoots {
    pub config_root: PathBuf,
    pub data_root: PathBuf,
    pub opencode_auth: Option<PathBuf>,
}

impl LegacyRoots {
    /// Treat one directory as both the configuration and data root.
    #[must_use]
    pub fn co_located(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config_root: root.clone(),
            data_root: root,
            opencode_auth: None,
        }
    }
}

/// What a dry run intends to do with a legacy input.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlannedAction {
    Import,
    Convert,
    QuarantineDisabled,
    MetadataOnly,
    SkipSecretBearing,
}

/// One legacy input found without opening or changing personal content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredInput {
    pub kind: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub entries: usize,
    pub contains_possible_secrets: bool,
    pub action: PlannedAction,
    pub modified_at: Option<String>,
}

/// Dry-run report that must precede any personal-data copy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationPlan {
    pub schema_version: u32,
    /// Kept for the original one-root API. It is the configuration root.
    pub source_root: PathBuf,
    pub roots: LegacyRoots,
    pub source_fingerprint: String,
    pub inputs: Vec<DiscoveredInput>,
    pub requires_confirmation: bool,
    pub remote_devices_require_repairing: bool,
    pub plaintext_secrets_will_be_skipped: bool,
}

impl MigrationPlan {
    /// Render the machine-readable dry run. It contains metadata, never payloads.
    ///
    /// # Errors
    ///
    /// Returns an error only if serialization fails.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Render a concise dry-run summary for a person reviewing the import.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut output = format!(
            "# Personal Agent legacy migration dry run\n\nSource fingerprint: `{}`\n\n",
            self.source_fingerprint
        );
        output.push_str("| Input | Items | Bytes | Planned action | Secret-bearing |\n");
        output.push_str("| --- | ---: | ---: | --- | --- |\n");
        for input in &self.inputs {
            let _ = writeln!(
                output,
                "| {} | {} | {} | {:?} | {} |",
                escape_table(&input.kind),
                input.entries,
                input.bytes,
                input.action,
                if input.contains_possible_secrets {
                    "yes"
                } else {
                    "no"
                }
            );
        }
        output.push_str(
            "\nNo personal payload has been copied. Plaintext secrets and traces are excluded; remote devices must pair again.\n",
        );
        output
    }
}

/// Explicit choices required before an import can read personal payloads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MigrationConsent {
    pub copy_personal_data: bool,
    /// Reserved for a future OS-keychain adoption flow. Auth is never put in a
    /// normal migration record even when this is true.
    pub adopt_opencode_auth: bool,
}

/// A validated record passed only to the encrypted persistence boundary.
///
/// The payload is deliberately omitted from `Debug` and from all serialized
/// reports so logs cannot accidentally become a second copy of personal data.
#[derive(Clone)]
pub struct PreparedRecord {
    pub id: String,
    pub kind: String,
    pub source_path: PathBuf,
    pub source_locator: String,
    pub source_modified_at: Option<String>,
    pub content_sha256: String,
    pub destination: String,
    pub enabled: bool,
    pub contains_personal_data: bool,
    payload: Vec<u8>,
}

impl PreparedRecord {
    /// Borrow the payload for immediate encrypted persistence.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

impl fmt::Debug for PreparedRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedRecord")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("source_path", &self.source_path)
            .field("source_locator", &self.source_locator)
            .field("source_modified_at", &self.source_modified_at)
            .field("content_sha256", &self.content_sha256)
            .field("destination", &self.destination)
            .field("enabled", &self.enabled)
            .field("contains_personal_data", &self.contains_personal_data)
            .field("payload", &"[REDACTED]")
            .finish()
    }
}

/// Persistence boundary used by the importer.
///
/// Production implementations must encrypt before returning from `store`.
pub trait MigrationSink {
    type Error;

    /// Whether this deterministic record was already committed.
    ///
    /// # Errors
    ///
    /// Returns the sink's native read error.
    fn contains(&mut self, record_id: &str) -> Result<bool, Self::Error>;

    /// Atomically persist one validated record.
    ///
    /// # Errors
    ///
    /// Returns the sink's native transaction error.
    fn store(&mut self, record: &PreparedRecord) -> Result<(), Self::Error>;
}

/// Terminal status for one report row.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImportStatus {
    Imported,
    AlreadyPresent,
    Skipped,
    Invalid,
}

/// Content-free provenance for one imported, duplicate, skipped, or invalid item.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportItemReport {
    pub id: String,
    pub kind: String,
    pub source_locator: String,
    pub source_modified_at: Option<String>,
    pub content_sha256: Option<String>,
    pub destination: Option<String>,
    pub status: ImportStatus,
    pub enabled: bool,
    pub bytes: u64,
    pub detail: String,
    pub secret_material_skipped: bool,
}

/// Aggregate counts for a completed run.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationSummary {
    pub imported: usize,
    pub already_present: usize,
    pub skipped: usize,
    pub invalid: usize,
    pub secrets_skipped: usize,
}

/// Machine- and human-readable result of a confirmed migration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationReport {
    pub schema_version: u32,
    pub run_id: String,
    pub source_fingerprint: String,
    pub started_at: String,
    pub completed_at: String,
    pub source_was_modified: bool,
    pub remote_devices_require_repairing: bool,
    pub summary: MigrationSummary,
    pub items: Vec<ImportItemReport>,
}

impl MigrationReport {
    /// Render the report as JSON without imported payloads.
    ///
    /// # Errors
    ///
    /// Returns an error only if serialization fails.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Render the report for review without imported payloads.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut output = format!(
            "# Personal Agent legacy migration report\n\nRun: `{}`  \nSource fingerprint: `{}`  \nImported: {} · already present: {} · skipped: {} · invalid: {}\n\n",
            self.run_id,
            self.source_fingerprint,
            self.summary.imported,
            self.summary.already_present,
            self.summary.skipped,
            self.summary.invalid
        );
        output.push_str("| Kind | Source | Status | Destination | Detail |\n");
        output.push_str("| --- | --- | --- | --- | --- |\n");
        for item in &self.items {
            let _ = writeln!(
                output,
                "| {} | {} | {:?} | {} | {} |",
                escape_table(&item.kind),
                escape_table(&item.source_locator),
                item.status,
                escape_table(item.destination.as_deref().unwrap_or("—")),
                escape_table(&item.detail)
            );
        }
        output.push_str(
            "\nSecret-bearing environment, trace, connector-auth, and pairing key material was not copied. Imported extensions and automations remain disabled pending review.\n",
        );
        output
    }
}

/// Paths of the two persisted report formats.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WrittenReports {
    pub json: PathBuf,
    pub markdown: PathBuf,
}

/// Discovery or preparation failure.
#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("legacy source is not a directory: {0}")]
    InvalidSource(PathBuf),
    #[error("cannot inspect legacy input: {0}")]
    Io(#[from] std::io::Error),
    #[error("legacy input exceeds the safe discovery limit of {MAX_DISCOVERED_FILES} files")]
    TooManyFiles,
    #[error("legacy input is too large to import safely: {0}")]
    FileTooLarge(PathBuf),
    #[error("legacy input changed or became a link while it was being read: {0}")]
    SourceChanged(PathBuf),
    #[error("migration plan cannot be serialized: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Failure of a confirmed run, preserving the sink's native error.
#[derive(Debug, Error)]
pub enum MigrationRunError<E>
where
    E: fmt::Debug + fmt::Display,
{
    #[error("personal-data import requires explicit confirmation")]
    ConfirmationRequired,
    #[error(transparent)]
    Migration(#[from] MigrationError),
    #[error("migration sink failed: {0}")]
    Sink(E),
}

/// Discover the historical one-root layout.
///
/// # Errors
///
/// Returns an error when the source is absent/not a directory or metadata
/// cannot be inspected. Symlink targets are never followed.
pub fn discover(source_root: &Path) -> Result<MigrationPlan, MigrationError> {
    discover_profile(&LegacyRoots::co_located(source_root))
}

/// Discover configuration and data roots without opening personal payloads.
///
/// # Errors
///
/// Returns an error when either required root is absent/not a directory or
/// filesystem metadata cannot be inspected.
#[allow(clippy::too_many_lines)]
pub fn discover_profile(roots: &LegacyRoots) -> Result<MigrationPlan, MigrationError> {
    for root in [&roots.config_root, &roots.data_root] {
        let metadata =
            fs::symlink_metadata(root).map_err(|_| MigrationError::InvalidSource(root.clone()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(MigrationError::InvalidSource(root.clone()));
        }
    }

    let mut inputs = Vec::new();
    let mut seen = HashSet::new();
    let config_candidates = [
        ("config", "config.toml", true, PlannedAction::Convert),
        ("environment", "env", true, PlannedAction::SkipSecretBearing),
        ("schedules", "schedule.toml", false, PlannedAction::Convert),
        ("skills", "skills", false, PlannedAction::QuarantineDisabled),
        (
            "skills",
            ".opencode/skills",
            false,
            PlannedAction::QuarantineDisabled,
        ),
        (
            "skills",
            ".claude/skills",
            false,
            PlannedAction::QuarantineDisabled,
        ),
        (
            "skills",
            ".agents/skills",
            false,
            PlannedAction::QuarantineDisabled,
        ),
        (
            "experts",
            "experts",
            false,
            PlannedAction::QuarantineDisabled,
        ),
        (
            "experts",
            ".opencode/agent",
            false,
            PlannedAction::QuarantineDisabled,
        ),
        (
            "experts",
            ".claude/agents",
            false,
            PlannedAction::QuarantineDisabled,
        ),
        (
            "experts",
            ".agents/experts",
            false,
            PlannedAction::QuarantineDisabled,
        ),
        ("themes", "themes", false, PlannedAction::QuarantineDisabled),
        ("mcp", "mcp.json", true, PlannedAction::MetadataOnly),
    ];
    for (kind, relative, secrets, action) in config_candidates {
        push_candidate(
            &mut inputs,
            &mut seen,
            kind,
            roots.config_root.join(relative),
            secrets,
            action,
        )?;
    }

    let data_candidates = [
        ("state", "state.json", false, PlannedAction::Convert),
        ("history", "history", false, PlannedAction::Import),
        ("history", "history.jsonl", false, PlannedAction::Import),
        ("memory", "memory", false, PlannedAction::Import),
        ("traces", "traces", true, PlannedAction::SkipSecretBearing),
        ("projects", "projects.json", false, PlannedAction::Convert),
        (
            "remote-devices",
            "devices.json",
            true,
            PlannedAction::MetadataOnly,
        ),
    ];
    for (kind, relative, secrets, action) in data_candidates {
        push_candidate(
            &mut inputs,
            &mut seen,
            kind,
            roots.data_root.join(relative),
            secrets,
            action,
        )?;
    }
    if let Some(auth) = &roots.opencode_auth {
        push_candidate(
            &mut inputs,
            &mut seen,
            "opencode-auth",
            auth.clone(),
            true,
            PlannedAction::SkipSecretBearing,
        )?;
    }
    inputs.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.path.cmp(&right.path))
    });
    let source_fingerprint = plan_fingerprint(roots, &inputs);
    Ok(MigrationPlan {
        schema_version: MIGRATION_SCHEMA_VERSION,
        source_root: roots.config_root.clone(),
        roots: roots.clone(),
        source_fingerprint,
        inputs,
        requires_confirmation: true,
        remote_devices_require_repairing: true,
        plaintext_secrets_will_be_skipped: true,
    })
}

/// Execute a previously reviewed plan through an encrypted sink.
///
/// The source is never written. Record IDs are deterministic across reruns;
/// the sink's `contains` check makes a retry idempotent.
///
/// # Errors
///
/// Returns an error when confirmation is absent, a source cannot be safely
/// prepared, or the sink fails.
pub fn migrate<S>(
    plan: &MigrationPlan,
    consent: MigrationConsent,
    sink: &mut S,
) -> Result<MigrationReport, MigrationRunError<S::Error>>
where
    S: MigrationSink,
    S::Error: fmt::Debug + fmt::Display,
{
    if !consent.copy_personal_data {
        return Err(MigrationRunError::ConfirmationRequired);
    }
    let started_at = Utc::now().to_rfc3339();
    let mut items = Vec::new();
    for input in &plan.inputs {
        let batch = prepare_input(plan, input, consent)?;
        items.extend(batch.notices);
        for record in batch.records {
            let already_present = sink.contains(&record.id).map_err(MigrationRunError::Sink)?;
            let status = if already_present {
                ImportStatus::AlreadyPresent
            } else {
                sink.store(&record).map_err(MigrationRunError::Sink)?;
                ImportStatus::Imported
            };
            items.push(ImportItemReport {
                id: record.id,
                kind: record.kind,
                source_locator: record.source_locator,
                source_modified_at: record.source_modified_at,
                content_sha256: Some(record.content_sha256),
                destination: Some(record.destination),
                status,
                enabled: record.enabled,
                bytes: u64::try_from(record.payload.len()).unwrap_or(u64::MAX),
                detail: if already_present {
                    "deterministic record already committed".to_owned()
                } else if record.enabled {
                    "imported with legacy provenance".to_owned()
                } else {
                    "imported disabled pending user review".to_owned()
                },
                secret_material_skipped: false,
            });
        }
    }
    items.sort_by(|left, right| {
        left.source_locator
            .cmp(&right.source_locator)
            .then_with(|| left.id.cmp(&right.id))
    });
    let summary = summarize(&items);
    Ok(MigrationReport {
        schema_version: MIGRATION_SCHEMA_VERSION,
        run_id: Uuid::now_v7().to_string(),
        source_fingerprint: plan.source_fingerprint.clone(),
        started_at,
        completed_at: Utc::now().to_rfc3339(),
        source_was_modified: false,
        remote_devices_require_repairing: true,
        summary,
        items,
    })
}

/// Persist both report formats with private file permissions.
///
/// # Errors
///
/// Returns an error if the report directory or either unique report file
/// cannot be created and synchronized.
pub fn write_reports(
    report: &MigrationReport,
    directory: &Path,
) -> Result<WrittenReports, MigrationError> {
    fs::create_dir_all(directory)?;
    set_private_directory(directory)?;
    let stem = format!("migration-{}", report.run_id);
    let json_path = directory.join(format!("{stem}.json"));
    let markdown_path = directory.join(format!("{stem}.md"));
    write_private_new(&json_path, report.to_json_pretty()?.as_bytes())?;
    write_private_new(&markdown_path, report.to_markdown().as_bytes())?;
    Ok(WrittenReports {
        json: json_path,
        markdown: markdown_path,
    })
}

#[derive(Default)]
struct PreparedBatch {
    records: Vec<PreparedRecord>,
    notices: Vec<ImportItemReport>,
}

fn push_candidate(
    inputs: &mut Vec<DiscoveredInput>,
    seen: &mut HashSet<PathBuf>,
    kind: &str,
    path: PathBuf,
    contains_possible_secrets: bool,
    action: PlannedAction,
) -> Result<(), MigrationError> {
    if !seen.insert(path.clone()) {
        return Ok(());
    }
    let Some(stats) = inspect_path(&path)? else {
        return Ok(());
    };
    inputs.push(DiscoveredInput {
        kind: kind.to_owned(),
        path,
        bytes: stats.bytes,
        entries: stats.entries,
        contains_possible_secrets,
        action,
        modified_at: stats.modified_at,
    });
    Ok(())
}

struct PathStats {
    bytes: u64,
    entries: usize,
    modified_at: Option<String>,
}

fn inspect_path(path: &Path) -> Result<Option<PathStats>, MigrationError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() && !metadata.is_dir() {
        return Ok(None);
    }
    if metadata.is_file() {
        return Ok(Some(PathStats {
            bytes: metadata.len(),
            entries: 1,
            modified_at: modified_at(&metadata),
        }));
    }
    let files = regular_files(path, MAX_DISCOVERED_FILES)?;
    let mut bytes = 0_u64;
    let mut newest = None;
    for file in &files {
        let child = fs::symlink_metadata(file)?;
        bytes = bytes.saturating_add(child.len());
        let candidate = modified_at(&child);
        if candidate > newest {
            newest = candidate;
        }
    }
    Ok(Some(PathStats {
        bytes,
        entries: files.len(),
        modified_at: newest,
    }))
}

fn regular_files(root: &Path, limit: usize) -> Result<Vec<PathBuf>, MigrationError> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() {
        return Ok(Vec::new());
    }
    if metadata.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = stack.pop() {
        let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                files.push(path);
                if files.len() > limit {
                    return Err(MigrationError::TooManyFiles);
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

fn plan_fingerprint(roots: &LegacyRoots, inputs: &[DiscoveredInput]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"personal-agent-migration-plan-v1\0");
    digest.update(roots.config_root.to_string_lossy().as_bytes());
    digest.update(b"\0");
    digest.update(roots.data_root.to_string_lossy().as_bytes());
    for input in inputs {
        digest.update(b"\0");
        digest.update(input.kind.as_bytes());
        digest.update(b"\0");
        digest.update(input.path.to_string_lossy().as_bytes());
        digest.update(input.bytes.to_le_bytes());
        digest.update(input.entries.to_le_bytes());
        if let Some(modified) = &input.modified_at {
            digest.update(modified.as_bytes());
        }
    }
    hex_digest(digest.finalize())
}

fn prepare_input(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
    consent: MigrationConsent,
) -> Result<PreparedBatch, MigrationError> {
    match input.kind.as_str() {
        "config" => prepare_config(plan, input),
        "state" => prepare_state(plan, input),
        "history" => prepare_history(plan, input),
        "memory" => prepare_memory(plan, input),
        "schedules" => prepare_schedule(plan, input),
        "skills" | "experts" => prepare_extensions(plan, input),
        "themes" => prepare_themes(plan, input),
        "mcp" => prepare_mcp_metadata(plan, input),
        "projects" => prepare_projects(plan, input),
        "remote-devices" => prepare_remote_metadata(plan, input),
        "opencode-auth" => Ok(skipped_secret(
            plan,
            input,
            if consent.adopt_opencode_auth {
                "OpenCode auth adoption requires the interactive OS-keychain flow; no plaintext auth was copied"
            } else {
                "OpenCode auth was not adopted because separate consent was absent"
            },
        )),
        "environment" => Ok(skipped_secret(
            plan,
            input,
            "legacy environment files may contain provider credentials and are never copied",
        )),
        "traces" => Ok(skipped_secret(
            plan,
            input,
            "legacy traces may contain prompts, tool arguments, results, and credentials and are never copied",
        )),
        _ => Ok(PreparedBatch {
            records: Vec::new(),
            notices: vec![notice(
                plan,
                input,
                ImportStatus::Skipped,
                "unrecognized legacy input",
                false,
            )],
        }),
    }
}

fn prepare_config(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
) -> Result<PreparedBatch, MigrationError> {
    let bytes = read_limited(&input.path)?;
    let text = String::from_utf8_lossy(&bytes);
    let parsed = match toml::from_str::<toml::Value>(&text) {
        Ok(value) => value,
        Err(_error) => {
            return Ok(invalid_batch(
                plan,
                input,
                "configuration is invalid TOML; parser excerpts are omitted from the content-free report",
            ));
        }
    };
    let persona = parsed.get("persona");
    let history = parsed.get("history");
    let trace = parsed.get("trace");
    let projects = parsed
        .get("integrations")
        .and_then(|value| value.get("projects"))
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mapped = json!({
        "mapping_version": 1,
        "source": "jarvis-config-v1",
        "persona": {
            "name": persona.and_then(|value| value.get("name")).and_then(toml::Value::as_str).unwrap_or("JARVIS"),
            "style": persona.and_then(|value| value.get("style")).and_then(toml::Value::as_str),
        },
        "privacy": {
            "record_transcripts": history.and_then(|value| value.get("enabled")).and_then(toml::Value::as_bool).unwrap_or(true),
            "record_tool_arguments": trace.and_then(|value| value.get("record_arguments")).and_then(toml::Value::as_bool).unwrap_or(false),
        },
        "projects": projects,
        "secret_values_imported": false,
    });
    let payload = serde_json::to_vec(&mapped)?;
    Ok(PreparedBatch {
        records: vec![make_record(
            plan,
            "settings",
            &input.path,
            None,
            "settings/legacy-v1",
            false,
            true,
            payload,
        )?],
        notices: vec![ImportItemReport {
            detail: "only persona, history privacy, trace privacy, and registered project paths use the explicit v1 mapping; all credential-like and unknown fields were omitted".to_owned(),
            secret_material_skipped: true,
            ..notice(plan, input, ImportStatus::Skipped, "secret-bearing configuration fields omitted", true)
        }],
    })
}

fn prepare_state(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
) -> Result<PreparedBatch, MigrationError> {
    let bytes = read_limited(&input.path)?;
    let parsed: JsonValue = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            return Ok(invalid_batch(
                plan,
                input,
                &format!("state is invalid JSON: {error}"),
            ));
        }
    };
    let mut mapped = JsonMap::new();
    for key in [
        "muted",
        "asleep",
        "asleep_since",
        "guest",
        "guest_since",
        "quiet_until",
        "project",
        "expert",
    ] {
        if let Some(value) = parsed.get(key) {
            mapped.insert(key.to_owned(), value.clone());
        }
    }
    mapped.insert("mapping_version".to_owned(), json!(1));
    mapped.insert("legacy_sessions_resumed".to_owned(), json!(false));
    let payload = serde_json::to_vec(&JsonValue::Object(mapped))?;
    Ok(single_record_batch(make_record(
        plan,
        "conversation-state",
        &input.path,
        None,
        "settings/legacy-conversation-state",
        false,
        true,
        payload,
    )?))
}

fn prepare_history(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
) -> Result<PreparedBatch, MigrationError> {
    let files = regular_files(&input.path, MAX_DISCOVERED_FILES)?;
    let mut batch = PreparedBatch::default();
    for path in files.into_iter().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension == "jsonl")
    }) {
        let bytes = read_limited(&path)?;
        let mut invalid = 0_usize;
        for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            if line.len() > MAX_HISTORY_LINE_BYTES {
                invalid += 1;
                continue;
            }
            let Ok(raw) = serde_json::from_slice::<JsonValue>(line) else {
                invalid += 1;
                continue;
            };
            let Some(text) = raw.get("text").and_then(JsonValue::as_str) else {
                invalid += 1;
                continue;
            };
            let Some(role) = raw.get("role").and_then(JsonValue::as_str) else {
                invalid += 1;
                continue;
            };
            if !matches!(role, "user" | "assistant" | "note") {
                invalid += 1;
                continue;
            }
            let payload = serde_json::to_vec(&json!({
                "schema_version": 1,
                "origin": "legacy-jarvis",
                "role": role,
                "text": text,
                "timestamp": raw.get("ts"),
                "context": raw.get("context"),
                "source": raw.get("source"),
                "turn": raw.get("turn"),
            }))?;
            batch.records.push(make_record(
                plan,
                "history-event",
                &path,
                Some(&format!("line {}", index + 1)),
                "events/legacy-history",
                true,
                true,
                payload,
            )?);
        }
        if invalid > 0 {
            batch.notices.push(file_notice(
                plan,
                "history",
                &path,
                ImportStatus::Invalid,
                &format!("{invalid} malformed or unsupported JSONL entries skipped"),
                false,
            ));
        }
    }
    Ok(batch)
}

fn prepare_memory(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
) -> Result<PreparedBatch, MigrationError> {
    let mut batch = PreparedBatch::default();
    for path in regular_files(&input.path, MAX_DISCOVERED_FILES)? {
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let relative = path
            .strip_prefix(&input.path)
            .unwrap_or(&path)
            .to_string_lossy();
        batch.records.push(make_record(
            plan,
            "memory",
            &path,
            None,
            &format!("memories/legacy/{relative}"),
            true,
            true,
            read_limited(&path)?,
        )?);
    }
    Ok(batch)
}

fn prepare_schedule(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
) -> Result<PreparedBatch, MigrationError> {
    let bytes = read_limited(&input.path)?;
    let parsed = match toml::from_str::<toml::Value>(&String::from_utf8_lossy(&bytes)) {
        Ok(value) => value,
        Err(_error) => {
            return Ok(invalid_batch(
                plan,
                input,
                "schedule is invalid TOML; parser excerpts are omitted from the content-free report",
            ));
        }
    };
    let Some(tasks) = parsed.get("task").and_then(toml::Value::as_array) else {
        return Ok(invalid_batch(
            plan,
            input,
            "schedule has no [[task]] entries",
        ));
    };
    let mut batch = PreparedBatch::default();
    for (index, task) in tasks.iter().enumerate() {
        let Some(table) = task.as_table() else {
            batch.notices.push(file_notice(
                plan,
                "automation",
                &input.path,
                ImportStatus::Invalid,
                &format!("task {} is not a table", index + 1),
                false,
            ));
            continue;
        };
        let name = table.get("name").and_then(toml::Value::as_str);
        let cron = table.get("cron").and_then(toml::Value::as_str);
        let prompt = table.get("prompt").and_then(toml::Value::as_str);
        let deliver = table
            .get("deliver")
            .and_then(toml::Value::as_str)
            .unwrap_or("notify");
        let (Some(name), Some(cron), Some(prompt)) = (name, cron, prompt) else {
            batch.notices.push(file_notice(
                plan,
                "automation",
                &input.path,
                ImportStatus::Invalid,
                &format!("task {} is missing name, cron, or prompt", index + 1),
                false,
            ));
            continue;
        };
        if !matches!(deliver, "speak" | "notify" | "silent") {
            batch.notices.push(file_notice(
                plan,
                "automation",
                &input.path,
                ImportStatus::Invalid,
                &format!("task {} has unsupported delivery mode", index + 1),
                false,
            ));
            continue;
        }
        let payload = serde_json::to_vec(&json!({
            "mapping_version": 1,
            "name": name,
            "cron": cron,
            "prompt": prompt,
            "deliver": deliver,
            "enabled": false,
        }))?;
        batch.records.push(make_record(
            plan,
            "automation",
            &input.path,
            Some(&format!("task {}", index + 1)),
            "automations/legacy",
            false,
            true,
            payload,
        )?);
    }
    Ok(batch)
}

fn prepare_extensions(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
) -> Result<PreparedBatch, MigrationError> {
    let all_files = regular_files(&input.path, MAX_DISCOVERED_FILES)?;
    let manifests = all_files
        .iter()
        .filter(|path| is_manifest(&input.kind, path))
        .cloned()
        .collect::<Vec<_>>();
    let mut batch = PreparedBatch::default();
    for manifest in manifests {
        let bytes = read_limited(&manifest)?;
        let text = String::from_utf8_lossy(&bytes);
        let Ok(name) = validate_extension_manifest(&input.kind, &manifest, &text) else {
            batch.notices.push(file_notice(
                plan,
                &input.kind,
                &manifest,
                ImportStatus::Invalid,
                "manifest failed conservative name, description, body, or folder validation",
                false,
            ));
            continue;
        };
        let folder_manifest = manifest.file_name().is_some_and(|file| {
            file.eq_ignore_ascii_case("SKILL.md") || file.eq_ignore_ascii_case("EXPERT.md")
        });
        let bundle_root = if folder_manifest {
            manifest.parent().unwrap_or(&input.path)
        } else {
            manifest.as_path()
        };
        let files = if bundle_root.is_file() {
            vec![bundle_root.to_path_buf()]
        } else {
            regular_files(bundle_root, MAX_EXTENSION_FILES)?
        };
        for path in files {
            let relative = if bundle_root.is_file() {
                path.file_name()
                    .map_or_else(|| "manifest.md".into(), |file| file.to_string_lossy())
            } else {
                path.strip_prefix(bundle_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
            };
            batch.records.push(make_record(
                plan,
                &format!("{}-file", input.kind.trim_end_matches('s')),
                &path,
                None,
                &format!("extensions/quarantine/{}/{}/{}", input.kind, name, relative),
                false,
                true,
                read_limited(&path)?,
            )?);
        }
    }
    Ok(batch)
}

fn prepare_themes(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
) -> Result<PreparedBatch, MigrationError> {
    let mut batch = PreparedBatch::default();
    for path in regular_files(&input.path, MAX_EXTENSION_FILES)? {
        let relative = path
            .strip_prefix(&input.path)
            .unwrap_or(&path)
            .to_string_lossy();
        batch.records.push(make_record(
            plan,
            "theme",
            &path,
            None,
            &format!("themes/quarantine/{relative}"),
            false,
            false,
            read_limited(&path)?,
        )?);
    }
    Ok(batch)
}

fn prepare_mcp_metadata(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
) -> Result<PreparedBatch, MigrationError> {
    let bytes = read_limited(&input.path)?;
    let parsed: JsonValue = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            return Ok(invalid_batch(
                plan,
                input,
                &format!("MCP configuration is invalid JSON: {error}"),
            ));
        }
    };
    let servers = parsed
        .get("mcpServers")
        .or_else(|| parsed.get("mcp"))
        .and_then(JsonValue::as_object)
        .or_else(|| parsed.as_object())
        .map(|object| object.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let payload = serde_json::to_vec(&json!({
        "mapping_version": 1,
        "server_names": servers,
        "enabled": false,
        "arguments_imported": false,
        "environment_imported": false,
        "headers_imported": false,
    }))?;
    Ok(PreparedBatch {
        records: vec![make_record(
            plan,
            "connector-metadata",
            &input.path,
            None,
            "connectors/legacy-mcp",
            false,
            false,
            payload,
        )?],
        notices: vec![notice(
            plan,
            input,
            ImportStatus::Skipped,
            "MCP arguments, environment, URLs, headers, and auth were omitted",
            true,
        )],
    })
}

fn prepare_projects(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
) -> Result<PreparedBatch, MigrationError> {
    let bytes = read_limited(&input.path)?;
    let parsed: JsonValue = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            return Ok(invalid_batch(
                plan,
                input,
                &format!("project registry is invalid JSON: {error}"),
            ));
        }
    };
    let paths = json_string_values(&parsed);
    let payload = serde_json::to_vec(&json!({
        "mapping_version": 1,
        "paths": paths,
        "verified": false,
    }))?;
    Ok(single_record_batch(make_record(
        plan,
        "projects",
        &input.path,
        None,
        "settings/legacy-projects",
        false,
        true,
        payload,
    )?))
}

fn prepare_remote_metadata(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
) -> Result<PreparedBatch, MigrationError> {
    let bytes = read_limited(&input.path)?;
    let parsed: JsonValue = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            return Ok(invalid_batch(
                plan,
                input,
                &format!("device registry is invalid JSON: {error}"),
            ));
        }
    };
    let mut devices = Vec::new();
    collect_device_metadata(&parsed, &mut devices);
    let payload = serde_json::to_vec(&json!({
        "mapping_version": 1,
        "devices": devices,
        "pairing_keys_imported": false,
        "repair_required": true,
    }))?;
    Ok(PreparedBatch {
        records: vec![make_record(
            plan,
            "remote-device-metadata",
            &input.path,
            None,
            "settings/legacy-remote-devices",
            false,
            false,
            payload,
        )?],
        notices: vec![notice(
            plan,
            input,
            ImportStatus::Skipped,
            "cryptographic pairing material and remote grants were omitted; every device must pair again",
            true,
        )],
    })
}

fn skipped_secret(plan: &MigrationPlan, input: &DiscoveredInput, detail: &str) -> PreparedBatch {
    PreparedBatch {
        records: Vec::new(),
        notices: vec![notice(plan, input, ImportStatus::Skipped, detail, true)],
    }
}

fn invalid_batch(plan: &MigrationPlan, input: &DiscoveredInput, detail: &str) -> PreparedBatch {
    PreparedBatch {
        records: Vec::new(),
        notices: vec![notice(plan, input, ImportStatus::Invalid, detail, false)],
    }
}

fn single_record_batch(record: PreparedRecord) -> PreparedBatch {
    PreparedBatch {
        records: vec![record],
        notices: Vec::new(),
    }
}

fn notice(
    plan: &MigrationPlan,
    input: &DiscoveredInput,
    status: ImportStatus,
    detail: &str,
    secret_material_skipped: bool,
) -> ImportItemReport {
    file_notice(
        plan,
        &input.kind,
        &input.path,
        status,
        detail,
        secret_material_skipped,
    )
}

fn file_notice(
    plan: &MigrationPlan,
    kind: &str,
    path: &Path,
    status: ImportStatus,
    detail: &str,
    secret_material_skipped: bool,
) -> ImportItemReport {
    let locator = source_locator(plan, path, None);
    let id = hash_text(&format!("notice\0{kind}\0{locator}\0{detail}"));
    ImportItemReport {
        id,
        kind: kind.to_owned(),
        source_locator: locator,
        source_modified_at: fs::symlink_metadata(path)
            .ok()
            .and_then(|metadata| modified_at(&metadata)),
        content_sha256: None,
        destination: None,
        status,
        enabled: false,
        bytes: 0,
        detail: detail.to_owned(),
        secret_material_skipped,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_record(
    plan: &MigrationPlan,
    kind: &str,
    source_path: &Path,
    suffix: Option<&str>,
    destination: &str,
    enabled: bool,
    contains_personal_data: bool,
    payload: Vec<u8>,
) -> Result<PreparedRecord, MigrationError> {
    if u64::try_from(payload.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
        return Err(MigrationError::FileTooLarge(source_path.to_path_buf()));
    }
    let content_sha256 = hash_bytes(&payload);
    let source_locator = source_locator(plan, source_path, suffix);
    let id = hash_text(&format!(
        "personal-agent-migration-record-v1\0{kind}\0{source_locator}\0{content_sha256}"
    ));
    let source_modified_at = fs::symlink_metadata(source_path)
        .ok()
        .and_then(|metadata| modified_at(&metadata));
    Ok(PreparedRecord {
        id,
        kind: kind.to_owned(),
        source_path: source_path.to_path_buf(),
        source_locator,
        source_modified_at,
        content_sha256,
        destination: destination.to_owned(),
        enabled,
        contains_personal_data,
        payload,
    })
}

fn source_locator(plan: &MigrationPlan, path: &Path, suffix: Option<&str>) -> String {
    let (label, relative) = if let Ok(relative) = path.strip_prefix(&plan.roots.config_root) {
        ("config", relative)
    } else if let Ok(relative) = path.strip_prefix(&plan.roots.data_root) {
        ("data", relative)
    } else if let Some(auth) = &plan.roots.opencode_auth {
        if path == auth {
            ("opencode-auth", Path::new("auth"))
        } else {
            ("external", path)
        }
    } else {
        ("external", path)
    };
    let mut locator = format!("{label}/{}", relative.to_string_lossy());
    if let Some(suffix) = suffix {
        locator.push('#');
        locator.push_str(suffix);
    }
    locator
}

fn read_limited(path: &Path) -> Result<Vec<u8>, MigrationError> {
    let before = fs::symlink_metadata(path)?;
    if before.file_type().is_symlink() || !before.is_file() || before.len() > MAX_FILE_BYTES {
        return Err(MigrationError::FileTooLarge(path.to_path_buf()));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options.open(path)?;
    let opened = file.metadata()?;
    if !same_file(&before, &opened) {
        return Err(MigrationError::SourceChanged(path.to_path_buf()));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() > usize::try_from(MAX_FILE_BYTES).unwrap_or(usize::MAX)
        || !same_file(&opened, &after)
    {
        return Err(MigrationError::SourceChanged(path.to_path_buf()));
    }
    Ok(bytes)
}

fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    if left.file_type() != right.file_type()
        || left.len() != right.len()
        || left.modified().ok() != right.modified().ok()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if left.dev() != right.dev() || left.ino() != right.ino() {
            return false;
        }
    }
    true
}

fn is_manifest(kind: &str, path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if kind == "skills" {
        name == "skill.md"
    } else {
        name == "expert.md"
            || Path::new(&name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                && name != "readme.md"
                && name != "skill.md"
    }
}

fn validate_extension_manifest(kind: &str, path: &Path, text: &str) -> Result<String, ()> {
    if text.len() > usize::try_from(MAX_FILE_BYTES).unwrap_or(usize::MAX)
        || !text.starts_with("---")
    {
        return Err(());
    }
    let mut lines = text.lines();
    let _opening = lines.next();
    let mut name = String::new();
    let mut description = String::new();
    let mut found_end = false;
    let mut body = String::new();
    for line in lines.by_ref() {
        if matches!(line.trim(), "---" | "...") {
            found_end = true;
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches(['\'', '"']);
            match key.trim() {
                "name" => value.clone_into(&mut name),
                "description" => value.clone_into(&mut description),
                _ => {}
            }
        }
    }
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    if !found_end
        || name.is_empty()
        || description.is_empty()
        || description.len() > 1024
        || body.trim().is_empty()
        || name.len() > 64
        || !valid_extension_name(&name)
    {
        return Err(());
    }
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(file_name.as_str(), "skill.md" | "expert.md") {
        if path.parent().and_then(Path::file_name) != Some(name.as_ref()) {
            return Err(());
        }
    } else if kind == "experts"
        && path
            .file_stem()
            .map(|value| value.to_string_lossy().into_owned())
            != Some(name.clone())
    {
        return Err(());
    }
    Ok(name)
}

fn valid_extension_name(name: &str) -> bool {
    !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn json_string_values(value: &JsonValue) -> Vec<String> {
    match value {
        JsonValue::Array(values) => values
            .iter()
            .filter_map(JsonValue::as_str)
            .map(str::to_owned)
            .collect(),
        JsonValue::Object(object) => object
            .values()
            .filter_map(|value| {
                value
                    .as_str()
                    .or_else(|| value.get("path").and_then(JsonValue::as_str))
            })
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn collect_device_metadata(value: &JsonValue, output: &mut Vec<JsonValue>) {
    match value {
        JsonValue::Array(values) => {
            for value in values {
                collect_device_metadata(value, output);
            }
        }
        JsonValue::Object(object) => {
            if object.contains_key("name") || object.contains_key("platform") {
                output.push(json!({
                    "name": object.get("name").and_then(JsonValue::as_str),
                    "platform": object.get("platform").and_then(JsonValue::as_str),
                }));
            } else {
                for (name, value) in object {
                    if let Some(child) = value.as_object() {
                        output.push(json!({
                            "name": child.get("name").and_then(JsonValue::as_str).unwrap_or(name),
                            "platform": child.get("platform").and_then(JsonValue::as_str),
                        }));
                    }
                }
            }
        }
        _ => {}
    }
}

fn summarize(items: &[ImportItemReport]) -> MigrationSummary {
    let mut summary = MigrationSummary::default();
    for item in items {
        match item.status {
            ImportStatus::Imported => summary.imported += 1,
            ImportStatus::AlreadyPresent => summary.already_present += 1,
            ImportStatus::Skipped => summary.skipped += 1,
            ImportStatus::Invalid => summary.invalid += 1,
        }
        if item.secret_material_skipped {
            summary.secrets_skipped += 1;
        }
    }
    summary
}

fn modified_at(metadata: &fs::Metadata) -> Option<String> {
    metadata
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
        .map(|timestamp| timestamp.to_rfc3339())
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex_digest(digest.finalize())
}

fn hash_text(text: &str) -> String {
    hash_bytes(text.as_bytes())
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .fold(String::new(), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}

fn escape_table(value: &str) -> String {
    value.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn set_private_directory(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private_new(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, convert::Infallible};

    #[derive(Default)]
    struct MemorySink {
        records: BTreeMap<String, PreparedRecord>,
    }

    impl MigrationSink for MemorySink {
        type Error = Infallible;

        fn contains(&mut self, record_id: &str) -> Result<bool, Self::Error> {
            Ok(self.records.contains_key(record_id))
        }

        fn store(&mut self, record: &PreparedRecord) -> Result<(), Self::Error> {
            self.records.insert(record.id.clone(), record.clone());
            Ok(())
        }
    }

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/legacy/synthetic-v1")
    }

    fn source_hash(root: &Path) -> String {
        let mut digest = Sha256::new();
        for path in regular_files(root, MAX_DISCOVERED_FILES).expect("fixture walk") {
            digest.update(
                path.strip_prefix(root)
                    .expect("relative")
                    .as_os_str()
                    .as_encoded_bytes(),
            );
            digest.update(fs::read(path).expect("fixture read"));
        }
        hex_digest(digest.finalize())
    }

    #[test]
    fn discovery_is_metadata_only_and_marks_secret_sources() {
        let root = fixture();
        let before = source_hash(&root);
        let plan = discover(&root).expect("plan");
        assert_eq!(before, source_hash(&root));
        assert!(plan.requires_confirmation);
        assert!(plan.inputs.iter().any(|input| {
            input.kind == "environment"
                && input.contains_possible_secrets
                && input.action == PlannedAction::SkipSecretBearing
        }));
        let rendered = plan.to_json_pretty().expect("json dry run");
        assert!(!rendered.contains("fixture-provider-value-must-never-migrate"));
        assert!(
            plan.to_markdown()
                .contains("No personal payload has been copied")
        );
    }

    #[test]
    fn import_requires_confirmation_and_preserves_source() {
        let root = fixture();
        let plan = discover(&root).expect("plan");
        let before = source_hash(&root);
        let mut sink = MemorySink::default();
        assert!(matches!(
            migrate(&plan, MigrationConsent::default(), &mut sink),
            Err(MigrationRunError::ConfirmationRequired)
        ));
        let report = migrate(
            &plan,
            MigrationConsent {
                copy_personal_data: true,
                adopt_opencode_auth: false,
            },
            &mut sink,
        )
        .expect("confirmed import");
        assert_eq!(before, source_hash(&root));
        assert!(!report.source_was_modified);
        assert!(report.summary.imported > 5);
        assert!(report.summary.secrets_skipped >= 3);
        assert!(sink.records.values().all(|record| {
            !record
                .payload()
                .windows("fixture-provider-value-must-never-migrate".len())
                .any(|window| window == b"fixture-provider-value-must-never-migrate")
        }));
        assert!(
            sink.records
                .values()
                .filter(|record| {
                    record.kind.contains("skill")
                        || record.kind.contains("expert")
                        || record.kind == "automation"
                })
                .all(|record| !record.enabled)
        );
        assert!(
            sink.records
                .values()
                .any(|record| record.kind == "automation"),
            "{:#?}",
            report.items
        );
        assert!(
            report
                .items
                .iter()
                .any(|item| item.detail.contains("pair again"))
        );
    }

    #[test]
    fn rerun_is_idempotent_and_reports_are_content_free() {
        let plan = discover(&fixture()).expect("plan");
        let consent = MigrationConsent {
            copy_personal_data: true,
            adopt_opencode_auth: false,
        };
        let mut sink = MemorySink::default();
        let first = migrate(&plan, consent, &mut sink).expect("first");
        let count = sink.records.len();
        let second = migrate(&plan, consent, &mut sink).expect("second");
        assert_eq!(sink.records.len(), count);
        assert_eq!(second.summary.imported, 0);
        assert_eq!(second.summary.already_present, first.summary.imported);
        let json = second.to_json_pretty().expect("report json");
        let markdown = second.to_markdown();
        for report in [&json, &markdown] {
            assert!(!report.contains("synthetic private memory"));
            assert!(!report.contains("fixture-provider-value-must-never-migrate"));
        }
    }

    #[test]
    fn reports_are_written_privately_in_both_formats() {
        let plan = discover(&fixture()).expect("plan");
        let mut sink = MemorySink::default();
        let report = migrate(
            &plan,
            MigrationConsent {
                copy_personal_data: true,
                adopt_opencode_auth: false,
            },
            &mut sink,
        )
        .expect("import");
        let output = tempfile::tempdir().expect("reports");
        let written = write_reports(&report, output.path()).expect("write reports");
        assert!(written.json.is_file());
        assert!(written.markdown.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(written.json).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn parser_failures_never_echo_source_lines_into_reports() {
        let temp = tempfile::tempdir().expect("temp");
        fs::write(
            temp.path().join("config.toml"),
            "[provider]\ncredential = 'parser-canary-must-not-enter-report\n",
        )
        .expect("invalid config");
        let plan = discover(temp.path()).expect("plan");
        let mut sink = MemorySink::default();
        let report = migrate(
            &plan,
            MigrationConsent {
                copy_personal_data: true,
                adopt_opencode_auth: false,
            },
            &mut sink,
        )
        .expect("reported invalid input");

        assert_eq!(report.summary.invalid, 1);
        assert!(
            !report
                .to_json_pretty()
                .expect("json")
                .contains("parser-canary-must-not-enter-report")
        );
        assert!(
            !report
                .to_markdown()
                .contains("parser-canary-must-not-enter-report")
        );
    }

    #[test]
    fn symlink_targets_are_never_discovered_or_imported() {
        let temp = tempfile::tempdir().expect("temp");
        fs::write(temp.path().join("outside"), "do not read").expect("outside");
        fs::create_dir(temp.path().join("memory")).expect("memory");
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            temp.path().join("outside"),
            temp.path().join("memory/linked.md"),
        )
        .expect("symlink");
        let plan = discover(temp.path()).expect("plan");
        assert!(plan.inputs.iter().all(|input| input.entries == 0));
        #[cfg(unix)]
        {
            let root_link = temp.path().join("root-link");
            std::os::unix::fs::symlink(temp.path(), &root_link).expect("root symlink");
            assert!(matches!(
                discover(&root_link),
                Err(MigrationError::InvalidSource(_))
            ));
        }
    }

    #[test]
    fn split_anonymized_profile_imports_and_auth_stays_outside_normal_sink() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/legacy/anonymized-split");
        let roots = LegacyRoots {
            config_root: root.join("config"),
            data_root: root.join("data"),
            opencode_auth: Some(root.join("opencode-auth.json")),
        };
        let before = source_hash(&root);
        let plan = discover_profile(&roots).expect("split plan");
        let mut sink = MemorySink::default();
        let consent = MigrationConsent {
            copy_personal_data: true,
            adopt_opencode_auth: true,
        };
        let first = migrate(&plan, consent, &mut sink).expect("first");
        let second = migrate(&plan, consent, &mut sink).expect("second");

        assert_eq!(before, source_hash(&root));
        assert_eq!(second.summary.imported, 0);
        assert_eq!(second.summary.already_present, first.summary.imported);
        assert!(first.items.iter().any(|item| {
            item.kind == "opencode-auth"
                && item.status == ImportStatus::Skipped
                && item.secret_material_skipped
        }));
        assert!(sink.records.values().all(|record| {
            !record
                .payload()
                .windows("anonymized-auth-material-must-not-migrate".len())
                .any(|window| window == b"anonymized-auth-material-must-not-migrate")
        }));
    }
}
