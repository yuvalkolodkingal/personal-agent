//! Encrypted, versioned artifact library and whiteboard IPC.

#![allow(clippy::needless_pass_by_value)] // Tauri owns deserialized IPC arguments.

use super::DesktopState;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use personal_agent_contracts::proto::EventEnvelope;
use personal_agent_core::{
    Artifact, ArtifactKind, ArtifactVersion, ArtifactWorkspace, SourceLink, WhiteboardCard,
    sanitized_html_report, terminal_safe_text,
};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::io::Write as _;
use std::path::Path;
use uuid::Uuid;

const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_LINKS: usize = 64;

#[derive(Debug, Serialize)]
pub(crate) struct ArtifactWorkspaceSnapshot {
    artifacts: Vec<Artifact>,
    cards: Vec<WhiteboardCard>,
    order: Vec<Uuid>,
    focused: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ArtifactContent {
    artifact_id: Uuid,
    title: String,
    kind: ArtifactKind,
    version: u32,
    media_type: String,
    byte_length: usize,
    content_base64: String,
    text: Option<String>,
    terminal_safe_text: Option<String>,
    source_links: Vec<SourceLink>,
}

fn snapshot(workspace: &ArtifactWorkspace) -> ArtifactWorkspaceSnapshot {
    let mut cards = Vec::with_capacity(workspace.whiteboard.cards.len());
    for id in &workspace.whiteboard.order {
        if let Some(card) = workspace.whiteboard.cards.get(id) {
            cards.push(card.clone());
        }
    }
    ArtifactWorkspaceSnapshot {
        artifacts: workspace.repository.list(),
        cards,
        order: workspace.whiteboard.order.clone(),
        focused: workspace.whiteboard.focused,
    }
}

fn load_workspace(
    profile: &personal_agent_core::ProfileState,
) -> Result<ArtifactWorkspace, String> {
    profile
        .artifact_workspace_snapshot()
        .map(Option::unwrap_or_default)
        .map_err(|error| error.to_string())
}

fn persist_event(
    profile: &mut personal_agent_core::ProfileState,
    event_type: &str,
    payload: &impl Serialize,
) -> Result<(), String> {
    let payload = serde_json::to_value(payload).map_err(|error| error.to_string())?;
    let event = EventEnvelope::new(1, "desktop-ui", "default", event_type, &payload)
        .map_err(|error| error.to_string())?;
    profile
        .record_runtime_event(event)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn persist_workspace_event(
    profile: &mut personal_agent_core::ProfileState,
    workspace: &ArtifactWorkspace,
    event_type: &str,
    payload: &impl Serialize,
) -> Result<(), String> {
    let payload = serde_json::to_value(payload).map_err(|error| error.to_string())?;
    profile
        .record_artifact_workspace_event(workspace, event_type, &payload)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn parse_kind(kind: &str) -> Result<ArtifactKind, String> {
    serde_json::from_value(serde_json::Value::String(kind.trim().to_ascii_lowercase()))
        .map_err(|_| format!("unsupported artifact kind: {kind}"))
}

fn default_media_type(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Text | ArtifactKind::Diff | ArtifactKind::Chart | ArtifactKind::Diagram => {
            "text/plain"
        }
        ArtifactKind::Code => "text/plain; charset=utf-8",
        ArtifactKind::Table => "text/csv",
        ArtifactKind::HtmlReport => "text/html",
        ArtifactKind::Image => "image/png",
        ArtifactKind::Audio => "audio/wav",
        ArtifactKind::Video => "video/mp4",
        ArtifactKind::Pdf => "application/pdf",
        ArtifactKind::Document => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        }
        ArtifactKind::Spreadsheet => {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        }
        ArtifactKind::Presentation => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        }
    }
}

fn text_kind(kind: ArtifactKind) -> bool {
    matches!(
        kind,
        ArtifactKind::Text
            | ArtifactKind::Code
            | ArtifactKind::Diff
            | ArtifactKind::Table
            | ArtifactKind::Chart
            | ArtifactKind::Diagram
            | ArtifactKind::HtmlReport
    )
}

fn artifact_bytes(
    kind: ArtifactKind,
    title: &str,
    content: &str,
    content_base64: Option<&str>,
) -> Result<Vec<u8>, String> {
    let bytes = if let Some(encoded) = content_base64 {
        if encoded.len() > (MAX_ARTIFACT_BYTES * 4 / 3).saturating_add(16) {
            return Err("encoded artifact exceeds the desktop size limit".into());
        }
        STANDARD
            .decode(encoded)
            .map_err(|_| "artifact file encoding is invalid".to_owned())?
    } else if !text_kind(kind) {
        return Err("binary artifact kinds require a selected file".into());
    } else if kind == ArtifactKind::HtmlReport {
        sanitized_html_report(title, content).into_bytes()
    } else {
        content.as_bytes().to_vec()
    };
    if bytes.is_empty() {
        return Err("artifact content is required".into());
    }
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "artifact content exceeds the {} MiB desktop limit",
            MAX_ARTIFACT_BYTES / 1024 / 1024
        ));
    }
    Ok(bytes)
}

fn validate_sources(sources: &[SourceLink]) -> Result<(), String> {
    if sources.len() > MAX_SOURCE_LINKS {
        return Err(format!(
            "at most {MAX_SOURCE_LINKS} source links are allowed"
        ));
    }
    for source in sources {
        if source.label.trim().is_empty() || source.label.len() > 256 || source.uri.len() > 8_192 {
            return Err("source labels and URIs must be non-empty and bounded".into());
        }
        let parsed = url::Url::parse(&source.uri)
            .map_err(|_| format!("source URI is invalid: {}", source.uri))?;
        if !matches!(parsed.scheme(), "https" | "http" | "file") {
            return Err("artifact sources must use HTTPS, HTTP, or file URIs".into());
        }
        if source.content_hash.as_ref().is_some_and(|hash| {
            hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err("source content hashes must be 64 hexadecimal characters".into());
        }
    }
    Ok(())
}

fn selected_version(artifact: &Artifact, version: Option<u32>) -> Result<ArtifactVersion, String> {
    let requested = version.unwrap_or_else(|| {
        artifact
            .versions
            .last()
            .map_or(1, |candidate| candidate.version)
    });
    artifact
        .versions
        .iter()
        .find(|candidate| candidate.version == requested)
        .cloned()
        .ok_or_else(|| format!("artifact version {requested} does not exist"))
}

fn verify_blob(version: &ArtifactVersion, bytes: &[u8]) -> Result<(), String> {
    let digest = sha256_hex(bytes);
    if bytes.len() != version.byte_length || digest != version.content_sha256 {
        return Err("artifact blob failed content-address verification".into());
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[tauri::command]
pub(crate) fn artifact_snapshot(
    state: tauri::State<'_, DesktopState>,
) -> Result<ArtifactWorkspaceSnapshot, String> {
    let profile = state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?;
    Ok(snapshot(&load_workspace(&profile)?))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) fn artifact_create(
    title: String,
    kind: String,
    media_type: Option<String>,
    content: String,
    content_base64: Option<String>,
    source_links: Vec<SourceLink>,
    pin: bool,
    state: tauri::State<'_, DesktopState>,
) -> Result<ArtifactWorkspaceSnapshot, String> {
    let title = title.trim();
    if title.is_empty() || title.len() > 512 {
        return Err("artifact title must contain 1 to 512 characters".into());
    }
    validate_sources(&source_links)?;
    let kind = parse_kind(&kind)?;
    let bytes = artifact_bytes(kind, title, &content, content_base64.as_deref())?;
    let media_type = media_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_media_type(kind));
    if media_type.len() > 256 {
        return Err("artifact media type is too long".into());
    }
    let mut profile = state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?;
    let mut workspace = load_workspace(&profile)?;
    let stored_hash = profile
        .store_artifact_blob(&bytes)
        .map_err(|error| error.to_string())?;
    let artifact = workspace
        .repository
        .create(title, kind, media_type, &bytes, source_links)
        .map_err(|error| error.to_string())?;
    if artifact.versions[0].content_sha256 != stored_hash {
        return Err("artifact content address failed verification".into());
    }
    let card_id = workspace.whiteboard.add(artifact.id);
    if pin {
        workspace
            .whiteboard
            .set_pinned(card_id, true)
            .map_err(|error| error.to_string())?;
    }
    persist_workspace_event(
        &mut profile,
        &workspace,
        "artifact.created",
        &serde_json::json!({
            "id": artifact.id,
            "title": artifact.title,
            "kind": artifact.kind,
            "version": 1,
            "content_sha256": stored_hash,
            "card_id": card_id,
        }),
    )?;
    Ok(snapshot(&workspace))
}

#[tauri::command]
pub(crate) fn artifact_add_version(
    artifact_id: Uuid,
    media_type: Option<String>,
    content: String,
    content_base64: Option<String>,
    source_links: Vec<SourceLink>,
    state: tauri::State<'_, DesktopState>,
) -> Result<ArtifactWorkspaceSnapshot, String> {
    validate_sources(&source_links)?;
    let mut profile = state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?;
    let mut workspace = load_workspace(&profile)?;
    let artifact = workspace
        .repository
        .get(artifact_id)
        .cloned()
        .ok_or_else(|| "artifact does not exist".to_owned())?;
    let bytes = artifact_bytes(
        artifact.kind,
        &artifact.title,
        &content,
        content_base64.as_deref(),
    )?;
    let media_type = media_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_media_type(artifact.kind));
    let stored_hash = profile
        .store_artifact_blob(&bytes)
        .map_err(|error| error.to_string())?;
    let version = workspace
        .repository
        .add_version(artifact_id, media_type, &bytes, source_links)
        .map_err(|error| error.to_string())?;
    if version.content_sha256 != stored_hash {
        return Err("artifact content address failed verification".into());
    }
    persist_workspace_event(
        &mut profile,
        &workspace,
        "artifact.version_created",
        &serde_json::json!({"id": artifact_id, "version": version.version, "content_sha256": stored_hash}),
    )?;
    Ok(snapshot(&workspace))
}

#[tauri::command]
pub(crate) fn artifact_restore_version(
    artifact_id: Uuid,
    version: u32,
    state: tauri::State<'_, DesktopState>,
) -> Result<ArtifactWorkspaceSnapshot, String> {
    let mut profile = state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?;
    let mut workspace = load_workspace(&profile)?;
    let artifact = workspace
        .repository
        .get(artifact_id)
        .cloned()
        .ok_or_else(|| "artifact does not exist".to_owned())?;
    let restored = selected_version(&artifact, Some(version))?;
    let bytes = profile
        .artifact_blob(&restored.content_sha256)
        .map_err(|error| error.to_string())?;
    verify_blob(&restored, &bytes)?;
    let created = workspace
        .repository
        .add_version(
            artifact_id,
            &restored.media_type,
            &bytes,
            restored.source_links.clone(),
        )
        .map_err(|error| error.to_string())?;
    persist_workspace_event(
        &mut profile,
        &workspace,
        "artifact.version_restored",
        &serde_json::json!({"id": artifact_id, "source_version": version, "new_version": created.version}),
    )?;
    Ok(snapshot(&workspace))
}

#[tauri::command]
pub(crate) fn artifact_content(
    artifact_id: Uuid,
    version: Option<u32>,
    state: tauri::State<'_, DesktopState>,
) -> Result<ArtifactContent, String> {
    let profile = state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?;
    let workspace = load_workspace(&profile)?;
    let artifact = workspace
        .repository
        .get(artifact_id)
        .ok_or_else(|| "artifact does not exist".to_owned())?;
    let selected = selected_version(artifact, version)?;
    let bytes = profile
        .artifact_blob(&selected.content_sha256)
        .map_err(|error| error.to_string())?;
    verify_blob(&selected, &bytes)?;
    let text = String::from_utf8(bytes.clone()).ok();
    Ok(ArtifactContent {
        artifact_id,
        title: artifact.title.clone(),
        kind: artifact.kind,
        version: selected.version,
        media_type: selected.media_type,
        byte_length: selected.byte_length,
        content_base64: STANDARD.encode(&bytes),
        terminal_safe_text: text.as_deref().map(terminal_safe_text),
        text,
        source_links: selected.source_links,
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) fn artifact_action(
    action: String,
    artifact_id: Option<Uuid>,
    card_id: Option<Uuid>,
    title: Option<String>,
    pinned: Option<bool>,
    order: Option<Vec<Uuid>>,
    confirmed: Option<bool>,
    state: tauri::State<'_, DesktopState>,
) -> Result<ArtifactWorkspaceSnapshot, String> {
    let mut profile = state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?;
    let mut workspace = load_workspace(&profile)?;
    match action.as_str() {
        "rename" => workspace
            .repository
            .rename(
                artifact_id.ok_or_else(|| "artifact ID is required".to_owned())?,
                title.as_deref().unwrap_or_default(),
            )
            .map_err(|error| error.to_string())?,
        "delete" => {
            if confirmed != Some(true) {
                return Err("artifact deletion requires explicit confirmation".into());
            }
            workspace
                .remove_artifact(artifact_id.ok_or_else(|| "artifact ID is required".to_owned())?)
                .map_err(|error| error.to_string())?;
        }
        "add_to_board" => {
            let id = artifact_id.ok_or_else(|| "artifact ID is required".to_owned())?;
            if workspace.repository.get(id).is_none() {
                return Err("artifact does not exist".into());
            }
            workspace.whiteboard.add(id);
        }
        "pin" => workspace
            .whiteboard
            .set_pinned(
                card_id.ok_or_else(|| "card ID is required".to_owned())?,
                pinned.unwrap_or(false),
            )
            .map_err(|error| error.to_string())?,
        "focus" => workspace
            .whiteboard
            .focus(Some(
                card_id.ok_or_else(|| "card ID is required".to_owned())?,
            ))
            .map_err(|error| error.to_string())?,
        "clear_focus" => workspace
            .whiteboard
            .focus(None)
            .map_err(|error| error.to_string())?,
        "copy_card" => {
            workspace
                .whiteboard
                .copy(card_id.ok_or_else(|| "card ID is required".to_owned())?)
                .map_err(|error| error.to_string())?;
        }
        "remove_card" => {
            workspace
                .whiteboard
                .remove(card_id.ok_or_else(|| "card ID is required".to_owned())?)
                .map_err(|error| error.to_string())?;
        }
        "reorder" => workspace
            .whiteboard
            .reorder(order.ok_or_else(|| "complete card order is required".to_owned())?)
            .map_err(|error| error.to_string())?,
        _ => return Err("unknown artifact action".into()),
    }
    persist_workspace_event(
        &mut profile,
        &workspace,
        &format!("artifact.{action}"),
        &serde_json::json!({"artifact_id": artifact_id, "card_id": card_id}),
    )?;
    Ok(snapshot(&workspace))
}

#[tauri::command]
pub(crate) fn artifact_export(
    artifact_id: Uuid,
    version: Option<u32>,
    path: String,
    confirmed: bool,
    state: tauri::State<'_, DesktopState>,
) -> Result<String, String> {
    if !confirmed {
        return Err("export requires confirmation of the exact destination".into());
    }
    if path.len() > 4_096 {
        return Err("export path is too long".into());
    }
    let destination = Path::new(&path);
    if !destination.is_absolute() {
        return Err("export destination must be an absolute path".into());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "export destination has no parent directory".to_owned())?;
    if !parent.is_dir() {
        return Err("export destination directory does not exist".into());
    }
    let mut profile = state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?;
    let workspace = load_workspace(&profile)?;
    let artifact = workspace
        .repository
        .get(artifact_id)
        .ok_or_else(|| "artifact does not exist".to_owned())?;
    let selected = selected_version(artifact, version)?;
    let bytes = profile
        .artifact_blob(&selected.content_sha256)
        .map_err(|error| error.to_string())?;
    verify_blob(&selected, &bytes)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".personal-agent-artifact-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temporary
        .persist_noclobber(destination)
        .map_err(|error| error.error.to_string())?;
    persist_event(
        &mut profile,
        "artifact.exported",
        &serde_json::json!({
            "id": artifact_id,
            "version": selected.version,
            "destination": destination,
            "byte_length": bytes.len(),
        }),
    )?;
    Ok(destination.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_reports_are_escaped_and_binary_limits_are_enforced() {
        let bytes = artifact_bytes(
            ArtifactKind::HtmlReport,
            "<unsafe>",
            "<script>x</script>",
            None,
        )
        .expect("report");
        let html = String::from_utf8(bytes).expect("utf8");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(artifact_bytes(ArtifactKind::Text, "x", "", None).is_err());
        assert!(artifact_bytes(ArtifactKind::Image, "x", "not an image", None).is_err());
        assert_eq!(
            artifact_bytes(ArtifactKind::Image, "x", "", Some("aW1hZ2U=")).expect("binary"),
            b"image"
        );
    }

    #[test]
    fn sources_require_bounded_safe_uris_and_hashes() {
        assert!(
            validate_sources(&[SourceLink {
                label: "source".into(),
                uri: "https://example.test/report".into(),
                content_hash: Some("a".repeat(64)),
            }])
            .is_ok()
        );
        assert!(
            validate_sources(&[SourceLink {
                label: "bad".into(),
                uri: "javascript:alert(1)".into(),
                content_hash: None,
            }])
            .is_err()
        );
    }

    #[test]
    fn blob_verification_checks_hash_and_length() {
        let bytes = b"content";
        let version = ArtifactVersion {
            version: 1,
            content_sha256: sha256_hex(bytes),
            media_type: "text/plain".into(),
            byte_length: bytes.len(),
            source_links: vec![],
        };
        assert!(verify_blob(&version, bytes).is_ok());
        assert!(verify_blob(&version, b"tampered").is_err());
    }
}
