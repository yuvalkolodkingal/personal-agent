//! Versioned artifacts, safe report rendering, and whiteboard organization.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Text,
    Code,
    Diff,
    Table,
    Chart,
    Diagram,
    HtmlReport,
    Image,
    Audio,
    Video,
    Pdf,
    Document,
    Spreadsheet,
    Presentation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceLink {
    pub label: String,
    pub uri: String,
    pub content_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactVersion {
    pub version: u32,
    pub content_sha256: String,
    pub media_type: String,
    pub byte_length: usize,
    pub source_links: Vec<SourceLink>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: Uuid,
    pub title: String,
    pub kind: ArtifactKind,
    pub versions: Vec<ArtifactVersion>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ArtifactError {
    #[error("artifact title and media type must not be blank")]
    BlankMetadata,
    #[error("artifact does not exist: {0}")]
    Missing(Uuid),
    #[error("whiteboard card does not exist: {0}")]
    MissingCard(Uuid),
    #[error("whiteboard order must contain every card exactly once")]
    InvalidOrder,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRepository {
    artifacts: BTreeMap<Uuid, Artifact>,
}

impl ArtifactRepository {
    /// Create a logical artifact and its first immutable version.
    ///
    /// # Errors
    ///
    /// Rejects blank title/media type.
    pub fn create(
        &mut self,
        title: &str,
        kind: ArtifactKind,
        media_type: &str,
        bytes: &[u8],
        source_links: Vec<SourceLink>,
    ) -> Result<Artifact, ArtifactError> {
        if title.trim().is_empty() || media_type.trim().is_empty() {
            return Err(ArtifactError::BlankMetadata);
        }
        let artifact = Artifact {
            id: Uuid::now_v7(),
            title: title.trim().into(),
            kind,
            versions: vec![artifact_version(1, media_type, bytes, source_links)],
        };
        self.artifacts.insert(artifact.id, artifact.clone());
        Ok(artifact)
    }

    /// Add a new version without mutating earlier provenance.
    ///
    /// # Errors
    ///
    /// Returns `Missing` or rejects blank media type.
    pub fn add_version(
        &mut self,
        id: Uuid,
        media_type: &str,
        bytes: &[u8],
        source_links: Vec<SourceLink>,
    ) -> Result<ArtifactVersion, ArtifactError> {
        if media_type.trim().is_empty() {
            return Err(ArtifactError::BlankMetadata);
        }
        let artifact = self
            .artifacts
            .get_mut(&id)
            .ok_or(ArtifactError::Missing(id))?;
        let number = u32::try_from(artifact.versions.len())
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        let version = artifact_version(number, media_type, bytes, source_links);
        artifact.versions.push(version.clone());
        Ok(version)
    }

    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<&Artifact> {
        self.artifacts.get(&id)
    }

    /// Return artifacts in stable identifier order for deterministic projections.
    #[must_use]
    pub fn list(&self) -> Vec<Artifact> {
        self.artifacts.values().cloned().collect()
    }

    /// Rename an artifact without changing immutable content versions.
    ///
    /// # Errors
    /// Returns `BlankMetadata` or `Missing`.
    pub fn rename(&mut self, id: Uuid, title: &str) -> Result<(), ArtifactError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(ArtifactError::BlankMetadata);
        }
        self.artifacts
            .get_mut(&id)
            .ok_or(ArtifactError::Missing(id))?
            .title = title.into();
        Ok(())
    }

    /// Remove an artifact and return its immutable metadata.
    ///
    /// # Errors
    /// Returns `Missing` for an unknown artifact.
    pub fn remove(&mut self, id: Uuid) -> Result<Artifact, ArtifactError> {
        self.artifacts.remove(&id).ok_or(ArtifactError::Missing(id))
    }
}

fn artifact_version(
    version: u32,
    media_type: &str,
    bytes: &[u8],
    source_links: Vec<SourceLink>,
) -> ArtifactVersion {
    ArtifactVersion {
        version,
        content_sha256: hex(&Sha256::digest(bytes)),
        media_type: media_type.into(),
        byte_length: bytes.len(),
        source_links,
    }
}

/// Escape all supplied title/body markup into a self-contained safe HTML report.
#[must_use]
pub fn sanitized_html_report(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{}</title></head><body><main><h1>{}</h1><pre>{}</pre></main></body></html>",
        escape_html(title),
        escape_html(title),
        escape_html(body)
    )
}

fn escape_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for character in input.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            other => output.push(other),
        }
    }
    output
}

/// Remove terminal control/escape bytes while preserving newline and tab.
#[must_use]
pub fn terminal_safe_text(input: &str) -> String {
    input
        .chars()
        .filter(|character| {
            matches!(character, '\n' | '\t')
                || (!character.is_control() && *character != '\u{001b}')
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhiteboardCard {
    pub id: Uuid,
    pub artifact_id: Uuid,
    pub pinned: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Whiteboard {
    pub cards: BTreeMap<Uuid, WhiteboardCard>,
    pub order: Vec<Uuid>,
    pub focused: Option<Uuid>,
}

impl Whiteboard {
    pub fn add(&mut self, artifact_id: Uuid) -> Uuid {
        let id = Uuid::now_v7();
        self.cards.insert(
            id,
            WhiteboardCard {
                id,
                artifact_id,
                pinned: false,
            },
        );
        self.order.push(id);
        id
    }
    /// Pin or unpin a card.
    ///
    /// # Errors
    /// Returns `MissingCard` for an unknown ID.
    pub fn set_pinned(&mut self, id: Uuid, pinned: bool) -> Result<(), ArtifactError> {
        self.cards
            .get_mut(&id)
            .ok_or(ArtifactError::MissingCard(id))?
            .pinned = pinned;
        Ok(())
    }
    /// Focus a card or clear focus.
    ///
    /// # Errors
    /// Returns `MissingCard` for an unknown ID.
    pub fn focus(&mut self, id: Option<Uuid>) -> Result<(), ArtifactError> {
        if let Some(id) = id
            && !self.cards.contains_key(&id)
        {
            return Err(ArtifactError::MissingCard(id));
        }
        self.focused = id;
        Ok(())
    }
    /// Replace order with an exact permutation.
    ///
    /// # Errors
    /// Returns `InvalidOrder` for missing, duplicate, or foreign IDs.
    pub fn reorder(&mut self, order: Vec<Uuid>) -> Result<(), ArtifactError> {
        let expected = self.cards.keys().copied().collect::<BTreeSet<_>>();
        let actual = order.iter().copied().collect::<BTreeSet<_>>();
        if expected != actual || order.len() != self.cards.len() {
            return Err(ArtifactError::InvalidOrder);
        }
        self.order = order;
        Ok(())
    }
    /// Copy a card reference with independent pin state.
    ///
    /// # Errors
    /// Returns `MissingCard` for an unknown ID.
    pub fn copy(&mut self, id: Uuid) -> Result<Uuid, ArtifactError> {
        let artifact_id = self
            .cards
            .get(&id)
            .ok_or(ArtifactError::MissingCard(id))?
            .artifact_id;
        Ok(self.add(artifact_id))
    }

    /// Remove one card while preserving its underlying artifact.
    ///
    /// # Errors
    /// Returns `MissingCard` for an unknown ID.
    pub fn remove(&mut self, id: Uuid) -> Result<WhiteboardCard, ArtifactError> {
        let card = self
            .cards
            .remove(&id)
            .ok_or(ArtifactError::MissingCard(id))?;
        self.order.retain(|candidate| *candidate != id);
        if self.focused == Some(id) {
            self.focused = None;
        }
        Ok(card)
    }

    /// Remove every card that points at a deleted artifact.
    pub fn remove_artifact(&mut self, artifact_id: Uuid) {
        let removed = self
            .cards
            .values()
            .filter(|card| card.artifact_id == artifact_id)
            .map(|card| card.id)
            .collect::<BTreeSet<_>>();
        self.cards.retain(|id, _| !removed.contains(id));
        self.order.retain(|id| !removed.contains(id));
        if self.focused.is_some_and(|id| removed.contains(&id)) {
            self.focused = None;
        }
    }
}

/// Encrypted snapshot metadata for the artifact library and whiteboard.
/// Content bytes remain in the `SQLCipher` content-addressed blob store.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactWorkspace {
    pub repository: ArtifactRepository,
    pub whiteboard: Whiteboard,
}

impl ArtifactWorkspace {
    /// Delete an artifact and all whiteboard references to it.
    ///
    /// # Errors
    /// Returns `Missing` for an unknown artifact.
    pub fn remove_artifact(&mut self, id: Uuid) -> Result<Artifact, ArtifactError> {
        let artifact = self.repository.remove(id)?;
        self.whiteboard.remove_artifact(id);
        Ok(artifact)
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn artifacts_are_versioned_with_sources_and_safe_rendering() {
        let mut repository = ArtifactRepository::default();
        let first = repository
            .create(
                "Report <one>",
                ArtifactKind::HtmlReport,
                "text/html",
                b"one",
                vec![SourceLink {
                    label: "source".into(),
                    uri: "https://example.test".into(),
                    content_hash: None,
                }],
            )
            .expect("artifact");
        let second = repository
            .add_version(first.id, "text/html", b"two", vec![])
            .expect("version");
        assert_eq!(second.version, 2);
        assert_ne!(first.versions[0].content_sha256, second.content_sha256);
        let html = sanitized_html_report("<script>alert(1)</script>", "\u{1b}[31m<body>");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<body>\u{1b}"));
        assert_eq!(terminal_safe_text("safe\u{1b}[31m\nnext"), "safe[31m\nnext");
    }
    #[test]
    fn whiteboard_operations_are_explicit() {
        let mut board = Whiteboard::default();
        let first = board.add(Uuid::now_v7());
        let second = board.copy(first).expect("copy");
        board.set_pinned(first, true).expect("pin");
        board.focus(Some(second)).expect("focus");
        board.reorder(vec![second, first]).expect("reorder");
        assert!(board.cards[&first].pinned);
        assert_eq!(board.focused, Some(second));
        assert_eq!(board.order, vec![second, first]);
        board.remove(second).expect("remove");
        assert_eq!(board.order, vec![first]);
        assert_eq!(board.focused, None);
    }

    #[test]
    fn deleting_an_artifact_removes_all_of_its_cards() {
        let mut workspace = ArtifactWorkspace::default();
        let artifact = workspace
            .repository
            .create("Draft", ArtifactKind::Text, "text/plain", b"one", vec![])
            .expect("artifact");
        workspace.whiteboard.add(artifact.id);
        workspace.whiteboard.add(artifact.id);
        workspace.remove_artifact(artifact.id).expect("delete");
        assert!(workspace.repository.list().is_empty());
        assert!(workspace.whiteboard.cards.is_empty());
        assert!(workspace.whiteboard.order.is_empty());
    }
}
