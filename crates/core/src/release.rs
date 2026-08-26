//! Signed update metadata, health rollback state, and export-aware uninstall planning.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use thiserror::Error;
use url::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChannel {
    Stable,
    Beta,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseArtifact {
    pub target: String,
    pub installer_kind: String,
    pub url: Url,
    pub sha256: String,
    pub byte_length: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SignedReleaseManifest {
    pub schema_version: u32,
    pub product: String,
    pub version: String,
    pub channel: ReleaseChannel,
    pub artifacts: Vec<ReleaseArtifact>,
    pub signature_hex: String,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ReleaseError {
    #[error("release manifest metadata is invalid: {0}")]
    InvalidManifest(String),
    #[error("release manifest signature is invalid")]
    InvalidSignature,
    #[error("release artifact is unavailable for target: {0}")]
    MissingTarget(String),
    #[error("release artifact hash or length does not match signed metadata")]
    ArtifactMismatch,
    #[error("update transition is invalid from {0:?}")]
    InvalidTransition(UpdateState),
    #[error("personal-data deletion requires explicit confirmation")]
    DeletionConfirmationRequired,
    #[error("requested export must complete before personal-data deletion")]
    ExportRequired,
}

impl SignedReleaseManifest {
    /// Canonical bytes signed by the offline release key. The signature field is blanked.
    ///
    /// # Errors
    /// Returns serialization errors as stable manifest failures.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, ReleaseError> {
        let mut unsigned = self.clone();
        unsigned.signature_hex.clear();
        serde_json::to_vec(&unsigned)
            .map_err(|error| ReleaseError::InvalidManifest(error.to_string()))
    }
    /// Verify structure, HTTPS artifact URLs, and the `Ed25519` signature.
    ///
    /// # Errors
    /// Rejects malformed metadata, key/signature bytes, or invalid signatures.
    pub fn verify(&self, public_key: &[u8; 32]) -> Result<(), ReleaseError> {
        if self.schema_version != 1
            || self.product != "Personal Agent"
            || self.version.trim().is_empty()
            || self.artifacts.is_empty()
        {
            return Err(ReleaseError::InvalidManifest(
                "identity fields are missing".into(),
            ));
        }
        for artifact in &self.artifacts {
            if artifact.target.trim().is_empty()
                || artifact.installer_kind.trim().is_empty()
                || artifact.url.scheme() != "https"
                || artifact.byte_length == 0
                || decode_hex::<32>(&artifact.sha256).is_none()
            {
                return Err(ReleaseError::InvalidManifest(format!(
                    "invalid artifact for {}",
                    artifact.target
                )));
            }
        }
        let key =
            VerifyingKey::from_bytes(public_key).map_err(|_| ReleaseError::InvalidSignature)?;
        let signature_bytes =
            decode_hex::<64>(&self.signature_hex).ok_or(ReleaseError::InvalidSignature)?;
        key.verify_strict(
            &self.signing_bytes()?,
            &Signature::from_bytes(&signature_bytes),
        )
        .map_err(|_| ReleaseError::InvalidSignature)
    }
    /// Select and verify downloaded bytes for one compile target.
    ///
    /// # Errors
    /// Returns missing-target or hash/length mismatch.
    pub fn verify_artifact(
        &self,
        target: &str,
        bytes: &[u8],
    ) -> Result<&ReleaseArtifact, ReleaseError> {
        let artifact = self
            .artifacts
            .iter()
            .find(|artifact| artifact.target == target)
            .ok_or_else(|| ReleaseError::MissingTarget(target.into()))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != artifact.byte_length
            || hex(&Sha256::digest(bytes)) != artifact.sha256
        {
            return Err(ReleaseError::ArtifactMismatch);
        }
        Ok(artifact)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    Available,
    Downloaded,
    Verified,
    BackupCreated,
    InstalledPendingHealth,
    Healthy,
    RolledBack,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateTransaction {
    pub from_version: String,
    pub to_version: String,
    pub target: String,
    pub state: UpdateState,
    pub backup_id: Option<String>,
}

impl UpdateTransaction {
    #[must_use]
    pub fn new(from_version: &str, to_version: &str, target: &str) -> Self {
        Self {
            from_version: from_version.into(),
            to_version: to_version.into(),
            target: target.into(),
            state: UpdateState::Available,
            backup_id: None,
        }
    }
    /// Record downloaded bytes only after matching signed artifact metadata.
    ///
    /// # Errors
    /// Rejects invalid state or artifact bytes.
    pub fn downloaded(
        &mut self,
        manifest: &SignedReleaseManifest,
        bytes: &[u8],
    ) -> Result<(), ReleaseError> {
        if self.state != UpdateState::Available {
            return Err(ReleaseError::InvalidTransition(self.state));
        }
        manifest.verify_artifact(&self.target, bytes)?;
        self.state = UpdateState::Downloaded;
        Ok(())
    }
    /// Record signature verification.
    ///
    /// # Errors
    /// Rejects invalid state or signature.
    pub fn verified(
        &mut self,
        manifest: &SignedReleaseManifest,
        public_key: &[u8; 32],
    ) -> Result<(), ReleaseError> {
        if self.state != UpdateState::Downloaded {
            return Err(ReleaseError::InvalidTransition(self.state));
        }
        manifest.verify(public_key)?;
        self.state = UpdateState::Verified;
        Ok(())
    }
    /// Record the encrypted database backup required before install.
    ///
    /// # Errors
    /// Rejects invalid state or blank backup identity.
    pub fn backup_created(&mut self, backup_id: &str) -> Result<(), ReleaseError> {
        if self.state != UpdateState::Verified || backup_id.trim().is_empty() {
            return Err(ReleaseError::InvalidTransition(self.state));
        }
        self.backup_id = Some(backup_id.into());
        self.state = UpdateState::BackupCreated;
        Ok(())
    }
    /// Enter health-check state only after backup.
    ///
    /// # Errors
    /// Rejects any transition that could install without rollback coverage.
    pub fn installed(&mut self) -> Result<(), ReleaseError> {
        if self.state != UpdateState::BackupCreated || self.backup_id.is_none() {
            return Err(ReleaseError::InvalidTransition(self.state));
        }
        self.state = UpdateState::InstalledPendingHealth;
        Ok(())
    }
    /// Finish health check; failure deterministically enters rolled-back state.
    ///
    /// # Errors
    /// Rejects calls outside pending-health state.
    pub fn health_result(&mut self, healthy: bool) -> Result<(), ReleaseError> {
        if self.state != UpdateState::InstalledPendingHealth {
            return Err(ReleaseError::InvalidTransition(self.state));
        }
        self.state = if healthy {
            UpdateState::Healthy
        } else {
            UpdateState::RolledBack
        };
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportDisposition {
    NotRequested,
    Pending,
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalDataDisposition {
    Keep,
    DeletePendingConfirmation,
    DeleteConfirmed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UninstallPlan {
    pub data_paths: Vec<PathBuf>,
    pub export: ExportDisposition,
    pub personal_data: PersonalDataDisposition,
}

impl UninstallPlan {
    /// Validate the plan without deleting anything.
    ///
    /// # Errors
    /// Requires explicit deletion confirmation and completion of requested export.
    pub fn validate(&self) -> Result<(), ReleaseError> {
        if self.personal_data == PersonalDataDisposition::DeletePendingConfirmation {
            return Err(ReleaseError::DeletionConfirmationRequired);
        }
        if self.personal_data == PersonalDataDisposition::DeleteConfirmed
            && self.export == ExportDisposition::Pending
        {
            return Err(ReleaseError::ExportRequired);
        }
        Ok(())
    }
}

fn decode_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N * 2 {
        return None;
    }
    let mut output = [0_u8; N];
    for (index, byte) in output.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = (nibble(value.as_bytes()[offset])? << 4) | nibble(value.as_bytes()[offset + 1])?;
    }
    Some(output)
}
fn nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    fn signed_manifest(bytes: &[u8]) -> (SignedReleaseManifest, [u8; 32]) {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let mut manifest = SignedReleaseManifest {
            schema_version: 1,
            product: "Personal Agent".into(),
            version: "0.2.0".into(),
            channel: ReleaseChannel::Beta,
            artifacts: vec![ReleaseArtifact {
                target: "x86_64-unknown-linux-gnu".into(),
                installer_kind: "deb".into(),
                url: Url::parse("https://releases.example.test/app.deb").unwrap(),
                sha256: hex(&Sha256::digest(bytes)),
                byte_length: u64::try_from(bytes.len()).unwrap(),
            }],
            signature_hex: String::new(),
        };
        manifest.signature_hex = hex(&signing.sign(&manifest.signing_bytes().unwrap()).to_bytes());
        (manifest, signing.verifying_key().to_bytes())
    }
    #[test]
    fn signed_update_backs_up_and_rolls_back_on_failed_health() {
        let bytes = b"fixture installer";
        let (manifest, public_key) = signed_manifest(bytes);
        let mut update = UpdateTransaction::new("0.1.0", "0.2.0", "x86_64-unknown-linux-gnu");
        update.downloaded(&manifest, bytes).expect("download");
        update.verified(&manifest, &public_key).expect("signature");
        assert!(update.installed().is_err());
        update.backup_created("backup-sha256").expect("backup");
        update.installed().expect("install");
        update.health_result(false).expect("health");
        assert_eq!(update.state, UpdateState::RolledBack);
    }
    #[test]
    fn tampering_is_rejected() {
        let (mut manifest, public_key) = signed_manifest(b"fixture installer");
        assert!(
            manifest
                .verify_artifact("x86_64-unknown-linux-gnu", b"tampered")
                .is_err()
        );
        manifest.version = "9.9.9".into();
        assert_eq!(
            manifest.verify(&public_key),
            Err(ReleaseError::InvalidSignature)
        );
    }
    #[test]
    fn uninstall_deletion_requires_confirmation_and_export() {
        let mut plan = UninstallPlan {
            data_paths: vec![PathBuf::from("profile")],
            export: ExportDisposition::Pending,
            personal_data: PersonalDataDisposition::DeletePendingConfirmation,
        };
        assert_eq!(
            plan.validate(),
            Err(ReleaseError::DeletionConfirmationRequired)
        );
        plan.personal_data = PersonalDataDisposition::DeleteConfirmed;
        assert_eq!(plan.validate(), Err(ReleaseError::ExportRequired));
        plan.export = ExportDisposition::Completed;
        plan.validate().expect("safe plan");
    }
}
