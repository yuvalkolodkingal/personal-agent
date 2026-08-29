//! Authenticated `OpenCode` skills/agents catalog and confirmed user-document edits.

#![allow(clippy::needless_pass_by_value)] // Tauri deserializes and owns IPC arguments.

use super::DesktopState;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use tauri::State;

const MAX_DOCUMENT_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DocumentKind {
    Agent,
    Command,
}

impl DocumentKind {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "agent" => Ok(Self::Agent),
            "command" => Ok(Self::Command),
            "skill" => Err(
                "skills are discovery-only; Personal Agent never installs agent-authored skills"
                    .into(),
            ),
            _ => Err("document kind must be agent or command".into()),
        }
    }

    const fn directory(self) -> &'static str {
        match self {
            Self::Agent => "agents",
            Self::Command => "commands",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Command => "command",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ManagedDocument {
    kind: &'static str,
    name: String,
    content: String,
    digest: String,
    enabled: bool,
    path_hint: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SkillsAgentsSnapshot {
    agents: Value,
    commands: Value,
    skills: Value,
    managed_documents: Vec<ManagedDocument>,
    default_agent: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct DocumentMutationResult {
    document: Option<ManagedDocument>,
    message: String,
}

fn managed_root(state: &DesktopState) -> PathBuf {
    state
        .app_data
        .join("runtime/opencode-profile/config/opencode")
}

#[tauri::command]
pub(crate) async fn skills_agents_snapshot(
    state: State<'_, DesktopState>,
) -> Result<SkillsAgentsSnapshot, String> {
    let (directory, default_agent) = {
        let config = state
            .config
            .read()
            .map_err(|_| "configuration lock is poisoned".to_owned())?;
        (
            config.runtime.working_directory.clone(),
            config.runtime.default_agent.clone(),
        )
    };
    let (agents, commands, skills) = match fs::canonicalize(&directory) {
        Ok(directory) if directory.is_dir() => {
            let client = {
                let runtime = state.runtime.lock().await;
                runtime.api_client().map_err(|error| error.to_string())
            };
            match client {
                Ok(client) => {
                    let query = [("directory", directory.display().to_string())];
                    tokio::join!(
                        catalog_resource(&client, "/agent", &query),
                        catalog_resource(&client, "/command", &query),
                        catalog_resource(&client, "/skill", &query),
                    )
                }
                Err(error) => unavailable_catalog(&error),
            }
        }
        Ok(_) => unavailable_catalog("the configured workspace is not a directory"),
        Err(error) => unavailable_catalog(&format!(
            "the configured workspace could not be resolved: {error}"
        )),
    };
    Ok(SkillsAgentsSnapshot {
        agents,
        commands,
        skills,
        managed_documents: discover_documents(&managed_root(&state))?,
        default_agent,
    })
}

fn unavailable_catalog(reason: &str) -> (Value, Value, Value) {
    let resource = json!({"available": false, "reason": reason});
    (resource.clone(), resource.clone(), resource)
}

async fn catalog_resource(
    client: &personal_agent_runtime::OpenCodeApiClient,
    route: &str,
    query: &[(&str, String)],
) -> Value {
    match client
        .request_json(reqwest::Method::GET, route, query, None)
        .await
    {
        Ok(data) => json!({"available": true, "data": data}),
        Err(error) => json!({"available": false, "reason": error.to_string()}),
    }
}

#[tauri::command]
pub(crate) fn skills_agents_write(
    kind: String,
    name: String,
    content: String,
    mode: String,
    expected_digest: Option<String>,
    confirmed: bool,
    state: State<'_, DesktopState>,
) -> Result<DocumentMutationResult, String> {
    if !confirmed {
        return Err("creating, editing, or importing a document requires confirmation".into());
    }
    if !matches!(mode.as_str(), "create" | "edit" | "import") {
        return Err("document write mode is invalid".into());
    }
    let kind = DocumentKind::parse(&kind)?;
    validate_name(&name)?;
    validate_document(kind, &content)?;
    let root = managed_root(&state);
    prepare_managed_directory(&root, kind, true)?;
    let path = document_path(&root, kind, &name)?;
    let existing = read_existing(&path)?;
    match mode.as_str() {
        "edit" => {
            let existing = existing.ok_or_else(|| "document no longer exists".to_owned())?;
            verify_digest(&existing, expected_digest.as_deref())?;
        }
        "create" | "import" => {
            if existing.is_some() {
                return Err("a document with this name already exists; open it in Edit".into());
            }
            if expected_digest.is_some() {
                return Err("new documents cannot include an existing digest".into());
            }
        }
        _ => unreachable!("validated write mode"),
    }
    atomic_write_document(&path, content.as_bytes())?;
    let document = managed_document(&root, kind, &path, content)?;
    Ok(DocumentMutationResult {
        message: format!("User-owned {} saved.", kind.label()),
        document: Some(document),
    })
}

#[tauri::command]
pub(crate) fn skills_agents_set_enabled(
    name: String,
    enabled: bool,
    expected_digest: String,
    confirmed: bool,
    state: State<'_, DesktopState>,
) -> Result<DocumentMutationResult, String> {
    if !confirmed {
        return Err("enabling or disabling an agent requires confirmation".into());
    }
    validate_name(&name)?;
    if !enabled {
        let default_agent = state
            .config
            .read()
            .map_err(|_| "configuration lock is poisoned".to_owned())?
            .runtime
            .default_agent
            .clone();
        if default_agent == name {
            return Err("choose another default agent before disabling this one".into());
        }
    }
    let root = managed_root(&state);
    prepare_managed_directory(&root, DocumentKind::Agent, false)?;
    let path = document_path(&root, DocumentKind::Agent, &name)?;
    let existing = read_existing(&path)?.ok_or_else(|| {
        "only user-owned agent documents can be enabled or disabled here".to_owned()
    })?;
    verify_digest(&existing, Some(&expected_digest))?;
    let content = set_agent_enabled(&existing, enabled)?;
    atomic_write_document(&path, content.as_bytes())?;
    Ok(DocumentMutationResult {
        message: format!("Agent {}.", if enabled { "enabled" } else { "disabled" }),
        document: Some(managed_document(
            &root,
            DocumentKind::Agent,
            &path,
            content,
        )?),
    })
}

#[tauri::command]
pub(crate) fn skills_agents_delete(
    kind: String,
    name: String,
    expected_digest: String,
    confirmed: bool,
    state: State<'_, DesktopState>,
) -> Result<DocumentMutationResult, String> {
    if !confirmed {
        return Err("deleting a user-owned document requires confirmation".into());
    }
    let kind = DocumentKind::parse(&kind)?;
    validate_name(&name)?;
    if kind == DocumentKind::Agent {
        let default_agent = state
            .config
            .read()
            .map_err(|_| "configuration lock is poisoned".to_owned())?
            .runtime
            .default_agent
            .clone();
        if default_agent == name {
            return Err("choose another default agent before deleting this one".into());
        }
    }
    let root = managed_root(&state);
    prepare_managed_directory(&root, kind, false)?;
    let path = document_path(&root, kind, &name)?;
    let existing = read_existing(&path)?.ok_or_else(|| "document no longer exists".to_owned())?;
    verify_digest(&existing, Some(&expected_digest))?;
    fs::remove_file(path).map_err(|error| error.to_string())?;
    Ok(DocumentMutationResult {
        document: None,
        message: format!("User-owned {} deleted.", kind.label()),
    })
}

fn discover_documents(root: &Path) -> Result<Vec<ManagedDocument>, String> {
    let mut documents = Vec::new();
    for kind in [DocumentKind::Agent, DocumentKind::Command] {
        let directory = root.join(kind.directory());
        if !directory.exists() {
            continue;
        }
        prepare_managed_directory(root, kind, false)?;
        for entry in fs::read_dir(&directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("md") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if validate_name(name).is_err() || metadata.len() > MAX_DOCUMENT_BYTES as u64 {
                continue;
            }
            let content = fs::read_to_string(&path).map_err(|error| error.to_string())?;
            if validate_document(kind, &content).is_err() {
                continue;
            }
            documents.push(managed_document(root, kind, &path, content)?);
        }
    }
    documents.sort_by(|left, right| {
        (left.kind, left.name.as_str()).cmp(&(right.kind, right.name.as_str()))
    });
    Ok(documents)
}

fn prepare_managed_directory(root: &Path, kind: DocumentKind, create: bool) -> Result<(), String> {
    if create {
        fs::create_dir_all(root).map_err(|error| error.to_string())?;
    }
    if root.exists() {
        let metadata = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err("managed OpenCode profile is not a regular directory".into());
        }
    }
    let directory = root.join(kind.directory());
    if create {
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    }
    if !directory.exists() {
        return Err(format!("managed {} directory does not exist", kind.label()));
    }
    let metadata = fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "managed {} directory must not be a symbolic link",
            kind.label()
        ));
    }
    Ok(())
}

fn managed_document(
    root: &Path,
    kind: DocumentKind,
    path: &Path,
    content: String,
) -> Result<ManagedDocument, String> {
    let name = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "managed document name is invalid".to_owned())?
        .to_owned();
    let enabled = kind != DocumentKind::Agent || !frontmatter_has_disable(&content)?;
    Ok(ManagedDocument {
        kind: kind.label(),
        name,
        digest: content_digest(&content),
        enabled,
        path_hint: path
            .strip_prefix(root)
            .map_err(|_| "managed document escaped its root".to_owned())?
            .display()
            .to_string(),
        content,
    })
}

fn document_path(root: &Path, kind: DocumentKind, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    Ok(root.join(kind.directory()).join(format!("{name}.md")))
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        || !name.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_')
        })
    {
        return Err(
            "document name must be a lowercase slug containing only letters, digits, - or _".into(),
        );
    }
    Ok(())
}

fn validate_document(kind: DocumentKind, content: &str) -> Result<(), String> {
    if content.is_empty() || content.len() > MAX_DOCUMENT_BYTES || content.contains('\0') {
        return Err("document must be non-empty, valid text, and at most 128 KiB".into());
    }
    let (frontmatter, body) = split_frontmatter(content)?;
    if body.trim().is_empty() {
        return Err("document prompt body cannot be empty".into());
    }
    let allowed: BTreeSet<&str> = match kind {
        DocumentKind::Agent => [
            "description",
            "mode",
            "model",
            "variant",
            "temperature",
            "top_p",
            "color",
            "steps",
            "maxSteps",
            "disable",
            "hidden",
            "tools",
            "permission",
        ]
        .into_iter()
        .collect(),
        DocumentKind::Command => ["description", "agent", "model", "subtask"]
            .into_iter()
            .collect(),
    };
    let mut parent = "";
    let mut keys = BTreeSet::new();
    for line in frontmatter.lines().filter(|line| !line.trim().is_empty()) {
        if line.contains('\t') || line.contains("!!") || line.contains("<<:") {
            return Err("frontmatter contains unsupported YAML features".into());
        }
        let trimmed = line.trim_start_matches(' ');
        if line.len() == trimmed.len() {
            let (key, value) = trimmed
                .split_once(':')
                .ok_or_else(|| "frontmatter entries must use key: value".to_owned())?;
            if !allowed.contains(key) || !keys.insert(key) {
                return Err(format!(
                    "frontmatter key is unsupported or duplicated: {key}"
                ));
            }
            parent = key;
            validate_scalar(key, value.trim())?;
        } else if !matches!(parent, "tools" | "permission") {
            return Err("nested frontmatter is allowed only under tools or permission".into());
        } else {
            let (key, value) = trimmed
                .split_once(':')
                .ok_or_else(|| "nested frontmatter entries must use key: value".to_owned())?;
            if key.is_empty() || key.contains(char::is_whitespace) {
                return Err("nested frontmatter key is invalid".into());
            }
            validate_scalar(key, value.trim())?;
        }
    }
    if kind == DocumentKind::Agent && !keys.contains("description") {
        return Err("agent frontmatter requires a description".into());
    }
    Ok(())
}

fn validate_scalar(key: &str, value: &str) -> Result<(), String> {
    if value.len() > 1024
        || value.starts_with('&')
        || value.starts_with('*')
        || value.contains("file://")
    {
        return Err(format!("frontmatter value is unsafe: {key}"));
    }
    match key {
        "mode" if !matches!(value, "primary" | "subagent" | "all") => {
            Err("agent mode must be primary, subagent, or all".into())
        }
        "disable" | "hidden" | "subtask" if !matches!(value, "true" | "false") => {
            Err(format!("{key} must be true or false"))
        }
        "steps" | "maxSteps"
            if value
                .parse::<u32>()
                .map_or(true, |number| number == 0 || number > 10_000) =>
        {
            Err(format!("{key} must be between 1 and 10000"))
        }
        "temperature" | "top_p"
            if value
                .parse::<f64>()
                .map_or(true, |number| !number.is_finite()) =>
        {
            Err(format!("{key} must be a finite number"))
        }
        "description" if value.is_empty() => Err("description cannot be empty".into()),
        _ => Ok(()),
    }
}

fn split_frontmatter(content: &str) -> Result<(&str, &str), String> {
    let content = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or_else(|| "document must begin with YAML frontmatter".to_owned())?;
    let marker = content
        .find("\n---\n")
        .or_else(|| content.find("\r\n---\r\n"))
        .ok_or_else(|| "document frontmatter is not closed".to_owned())?;
    let marker_len = if content[marker..].starts_with("\r\n") {
        "\r\n---\r\n".len()
    } else {
        "\n---\n".len()
    };
    Ok((&content[..marker], &content[marker + marker_len..]))
}

fn frontmatter_has_disable(content: &str) -> Result<bool, String> {
    let (frontmatter, _) = split_frontmatter(content)?;
    Ok(frontmatter.lines().any(|line| {
        line.len() == line.trim_start_matches(' ').len() && line.trim() == "disable: true"
    }))
}

fn set_agent_enabled(content: &str, enabled: bool) -> Result<String, String> {
    validate_document(DocumentKind::Agent, content)?;
    let (frontmatter, body) = split_frontmatter(content)?;
    let mut lines = frontmatter
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start_matches(' ');
            line.len() != trimmed.len() || !trimmed.starts_with("disable:")
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if !enabled {
        lines.push("disable: true".into());
    }
    let rendered = format!("---\n{}\n---\n{}", lines.join("\n"), body);
    validate_document(DocumentKind::Agent, &rendered)?;
    Ok(rendered)
}

fn read_existing(path: &Path) -> Result<Option<String>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("managed document path is not a regular file".into());
    }
    if metadata.len() > MAX_DOCUMENT_BYTES as u64 {
        return Err("managed document exceeds 128 KiB".into());
    }
    fs::read_to_string(path)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn verify_digest(content: &str, expected: Option<&str>) -> Result<(), String> {
    let digest = content_digest(content);
    if expected != Some(digest.as_str()) {
        return Err("document changed since it was opened; refresh before overwriting it".into());
    }
    Ok(())
}

fn content_digest(content: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(content.as_bytes()) {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn atomic_write_document(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "managed document has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    fs::set_permissions(parent, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".document-")
        .suffix(".md.tmp")
        .tempfile_in(parent)
        .map_err(|error| error.to_string())?;
    temporary
        .write_all(content)
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    temporary
        .persist(path)
        .map_err(|error| error.error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: &str = "---\ndescription: Reviews changes\nmode: subagent\ntools:\n  read: true\n  edit: true\n  bash: true\npermission:\n  edit: allow\n  bash: ask\n---\nReview and edit only inside the active workspace.\n";
    const COMMAND: &str = "---\ndescription: Run focused tests\nagent: build\nsubtask: false\n---\nRun the focused tests for $ARGUMENTS.\n";

    #[test]
    fn safe_frontmatter_and_names_are_enforced() {
        assert!(validate_document(DocumentKind::Agent, AGENT).is_ok());
        assert!(validate_document(DocumentKind::Command, COMMAND).is_ok());
        assert!(validate_name("review_changes").is_ok());
        assert!(validate_name("../escape").is_err());
        assert!(
            validate_document(
                DocumentKind::Agent,
                "---\ndescription: bad\nplugin: file:///tmp/evil\n---\nPrompt\n"
            )
            .is_err()
        );
        assert!(DocumentKind::parse("skill").is_err());
    }

    #[test]
    fn enabling_preserves_content_and_removes_only_top_level_disable() {
        let disabled = set_agent_enabled(AGENT, false).unwrap();
        assert!(frontmatter_has_disable(&disabled).unwrap());
        assert!(disabled.contains("Review and edit only inside the active workspace."));
        let enabled = set_agent_enabled(&disabled, true).unwrap();
        assert!(!frontmatter_has_disable(&enabled).unwrap());
        assert!(validate_document(DocumentKind::Agent, &enabled).is_ok());
    }

    #[test]
    fn atomic_user_documents_never_escape_managed_directories() {
        let root = tempfile::tempdir().unwrap();
        let path = document_path(root.path(), DocumentKind::Command, "focused_test").unwrap();
        atomic_write_document(&path, COMMAND.as_bytes()).unwrap();
        let content = read_existing(&path).unwrap().unwrap();
        assert_eq!(content, COMMAND);
        assert!(path.starts_with(root.path().join("commands")));
        verify_digest(&content, Some(&content_digest(&content))).unwrap();
        assert!(verify_digest(&content, Some("stale")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn managed_document_directories_reject_symbolic_links() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("agents")).unwrap();
        assert!(prepare_managed_directory(root.path(), DocumentKind::Agent, true).is_err());
        assert!(!outside.path().join("escaped.md").exists());
    }
}
