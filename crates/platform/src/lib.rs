//! Honest cross-platform capability reporting.

pub mod desktop;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Parsed operating-system secret-store reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretReference {
    pub service: String,
    pub account: String,
}

impl SecretReference {
    /// Parse `keychain://service/account` without accepting ambiguous paths.
    ///
    /// # Errors
    ///
    /// Returns an error for other schemes, blank segments, or extra segments.
    pub fn parse(alias: &str) -> Result<Self, SecretStoreError> {
        let path = alias
            .strip_prefix("keychain://")
            .ok_or(SecretStoreError::InvalidAlias)?;
        let mut parts = path.split('/');
        let service = parts.next().filter(|part| !part.is_empty());
        let account = parts.next().filter(|part| !part.is_empty());
        if parts.next().is_some() {
            return Err(SecretStoreError::InvalidAlias);
        }
        match (service, account) {
            (Some(service), Some(account)) => Ok(Self {
                service: service.to_owned(),
                account: account.to_owned(),
            }),
            _ => Err(SecretStoreError::InvalidAlias),
        }
    }

    #[must_use]
    pub fn alias(&self) -> String {
        format!("keychain://{}/{}", self.service, self.account)
    }
}

/// Secret-store error that never includes secret material.
#[derive(Debug, Error)]
pub enum SecretStoreError {
    #[error("secret alias must be keychain://service/account")]
    InvalidAlias,
    #[error("secret store entry does not exist")]
    Missing,
    #[error("operating-system secret store is unavailable: {0}")]
    Unavailable(String),
}

/// Narrow secret-store boundary used by bootstrap and provider onboarding.
pub trait SecretStore: Send + Sync {
    /// Store or replace one secret.
    ///
    /// # Errors
    ///
    /// Returns an error when the OS store is unavailable or rejects the entry.
    fn put(
        &self,
        reference: &SecretReference,
        value: &SecretString,
    ) -> Result<(), SecretStoreError>;
    /// Retrieve one secret without making it serializable or printable.
    ///
    /// # Errors
    ///
    /// Returns `Missing` when absent, or `Unavailable` for backend failures.
    fn get(&self, reference: &SecretReference) -> Result<SecretString, SecretStoreError>;
    /// Delete one secret.
    ///
    /// # Errors
    ///
    /// Returns `Missing` when absent, or `Unavailable` for backend failures.
    fn delete(&self, reference: &SecretReference) -> Result<(), SecretStoreError>;
}

/// macOS Keychain, Windows Credential Manager, or Linux Secret Service.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsSecretStore;

impl OsSecretStore {
    fn entry(reference: &SecretReference) -> Result<keyring::Entry, SecretStoreError> {
        keyring::Entry::new(&reference.service, &reference.account)
            .map_err(|error| SecretStoreError::Unavailable(error.to_string()))
    }

    fn map_error(error: &keyring::Error) -> SecretStoreError {
        if matches!(error, keyring::Error::NoEntry) {
            SecretStoreError::Missing
        } else {
            SecretStoreError::Unavailable(error.to_string())
        }
    }
}

impl SecretStore for OsSecretStore {
    fn put(
        &self,
        reference: &SecretReference,
        value: &SecretString,
    ) -> Result<(), SecretStoreError> {
        Self::entry(reference)?
            .set_password(value.expose_secret())
            .map_err(|error| Self::map_error(&error))
    }

    fn get(&self, reference: &SecretReference) -> Result<SecretString, SecretStoreError> {
        Self::entry(reference)?
            .get_password()
            .map(SecretString::from)
            .map_err(|error| Self::map_error(&error))
    }

    fn delete(&self, reference: &SecretReference) -> Result<(), SecretStoreError> {
        Self::entry(reference)?
            .delete_credential()
            .map_err(|error| Self::map_error(&error))
    }
}

/// Platform support is never represented as a silent no-op.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CapabilityStatus {
    Supported,
    Degraded {
        reason: String,
        remediation: Option<String>,
    },
    Unsupported {
        reason: String,
        remediation: Option<String>,
    },
}

/// Named native capability shown in diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlatformCapability {
    pub id: String,
    pub backend: String,
    pub status: CapabilityStatus,
}

/// Runtime permission state returned by native probes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PermissionState {
    Granted,
    Denied { guidance: String },
    NotDetermined { guidance: String },
    Unavailable { reason: String },
}

/// Runtime permission observations used to refine compile-time support.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimePermissions {
    pub accessibility: PermissionState,
    pub screen_capture: PermissionState,
    pub audio_duplex: PermissionState,
}

/// Native resource generations are invalidated after suspend/resume or device change.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceGenerations {
    pub audio: u64,
    pub display: u64,
    pub browser: u64,
    pub network: u64,
    pub provider: u64,
}

impl ResourceGenerations {
    /// Invalidate every OS-bound handle after resume.
    pub fn resume(&mut self) {
        self.audio = self.audio.saturating_add(1);
        self.display = self.display.saturating_add(1);
        self.browser = self.browser.saturating_add(1);
        self.network = self.network.saturating_add(1);
        self.provider = self.provider.saturating_add(1);
    }

    pub fn audio_device_changed(&mut self) {
        self.audio = self.audio.saturating_add(1);
    }

    pub fn display_changed(&mut self) {
        self.display = self.display.saturating_add(1);
        self.browser = self.browser.saturating_add(1);
    }
}

/// Compile-target capability matrix. Runtime permission checks refine it.
#[must_use]
pub fn compile_time_capabilities() -> Vec<PlatformCapability> {
    let platform = std::env::consts::OS;
    let (desktop, capture, audio) = match platform {
        "macos" => (
            "Accessibility API",
            "ScreenCaptureKit",
            "CoreAudio/VoiceProcessingIO",
        ),
        "windows" => (
            "UI Automation/SendInput",
            "Windows Graphics Capture",
            "WASAPI",
        ),
        "linux" => ("AT-SPI/xdg-desktop-portal", "PipeWire portal", "PipeWire"),
        other => {
            return vec![PlatformCapability {
                id: "desktop".into(),
                backend: other.into(),
                status: CapabilityStatus::Unsupported {
                    reason: format!("{other} is not a supported desktop target"),
                    remediation: Some("Use Windows, macOS, or Linux".into()),
                },
            }];
        }
    };
    vec![
        PlatformCapability {
            id: "desktop.accessibility".into(),
            backend: desktop.into(),
            status: CapabilityStatus::Degraded {
                reason: "runtime permission has not been probed".into(),
                remediation: Some("Open Diagnostics to run the permission check".into()),
            },
        },
        PlatformCapability {
            id: "screen.capture".into(),
            backend: capture.into(),
            status: CapabilityStatus::Degraded {
                reason: "runtime permission has not been probed".into(),
                remediation: Some("Open Diagnostics to run the permission check".into()),
            },
        },
        PlatformCapability {
            id: "audio.duplex".into(),
            backend: audio.into(),
            status: CapabilityStatus::Degraded {
                reason: "runtime permission has not been probed".into(),
                remediation: Some("Open Diagnostics to run the permission check".into()),
            },
        },
    ]
}

/// Refine compiled backends with native permission results. This function never
/// upgrades an unknown or denied permission to nominal support.
#[must_use]
pub fn runtime_capabilities(permissions: &RuntimePermissions) -> Vec<PlatformCapability> {
    let mut capabilities = compile_time_capabilities();
    for capability in &mut capabilities {
        let permission = match capability.id.as_str() {
            "desktop.accessibility" => &permissions.accessibility,
            "screen.capture" => &permissions.screen_capture,
            "audio.duplex" => &permissions.audio_duplex,
            _ => continue,
        };
        capability.status = permission_status(permission);
    }
    capabilities
}

fn permission_status(permission: &PermissionState) -> CapabilityStatus {
    match permission {
        PermissionState::Granted => CapabilityStatus::Supported,
        PermissionState::Denied { guidance } => CapabilityStatus::Unsupported {
            reason: "operating-system permission was denied".into(),
            remediation: Some(guidance.clone()),
        },
        PermissionState::NotDetermined { guidance } => CapabilityStatus::Degraded {
            reason: "operating-system permission has not been requested".into(),
            remediation: Some(guidance.clone()),
        },
        PermissionState::Unavailable { reason } => CapabilityStatus::Unsupported {
            reason: reason.clone(),
            remediation: None,
        },
    }
}

/// Watchdog policy with bounded exponential retry and a stable reset window.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WatchdogBackoff {
    pub attempts: u8,
    pub maximum_attempts: u8,
    pub base_delay_ms: u64,
    pub maximum_delay_ms: u64,
}

impl WatchdogBackoff {
    /// Return the next delay or `None` after the retry budget is exhausted.
    pub fn next_delay(&mut self) -> Option<u64> {
        if self.attempts >= self.maximum_attempts {
            return None;
        }
        let exponent = u32::from(self.attempts.min(20));
        self.attempts = self.attempts.saturating_add(1);
        Some(
            self.base_delay_ms
                .saturating_mul(2_u64.saturating_pow(exponent))
                .min(self.maximum_delay_ms),
        )
    }

    pub fn healthy_window_completed(&mut self) {
        self.attempts = 0;
    }
}

/// Presence-only process marker used to distinguish a clean exit from a crash.
///
/// The marker contains only product version, process ID, and start time. It is
/// deliberately outside the log stream and never contains arguments or secrets.
#[derive(Debug)]
pub struct LifecycleMarker {
    path: PathBuf,
    previous_unclean_run: bool,
}

impl LifecycleMarker {
    /// Replace the previous marker and report whether it survived an earlier run.
    ///
    /// Callers must enforce single-instance startup before invoking this method.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the private application-state directory or marker
    /// cannot be created and durably flushed.
    pub fn begin(path: &Path, version: &str) -> Result<Self, std::io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let previous_unclean_run = path.exists();
        let started_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let body = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "product_version": version,
            "process_id": std::process::id(),
            "started_at_unix_ms": started_at_unix_ms,
        }))
        .map_err(std::io::Error::other)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(&body)?;
        file.sync_all()?;
        Ok(Self {
            path: path.to_owned(),
            previous_unclean_run,
        })
    }

    #[must_use]
    pub fn previous_unclean_run(&self) -> bool {
        self.previous_unclean_run
    }

    /// Remove the marker only during the application's normal exit path.
    ///
    /// # Errors
    ///
    /// Returns an I/O error other than an already-absent marker.
    pub fn finish(&self) -> Result<(), std::io::Error> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_references_are_unambiguous_and_redactable() {
        let reference = SecretReference::parse("keychain://dev.personal-agent/db-default")
            .expect("valid alias");
        assert_eq!(reference.service, "dev.personal-agent");
        assert_eq!(reference.account, "db-default");
        assert_eq!(
            reference.alias(),
            "keychain://dev.personal-agent/db-default"
        );
        assert!(SecretReference::parse("plaintext").is_err());
        assert!(SecretReference::parse("keychain://service/account/extra").is_err());
    }

    #[test]
    fn every_capability_has_a_backend_and_explicit_state() {
        for capability in compile_time_capabilities() {
            assert!(!capability.backend.is_empty());
            assert!(!capability.id.is_empty());
        }
    }

    #[test]
    fn lifecycle_marker_distinguishes_clean_and_unclean_restarts() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("run-state.json");
        let first = LifecycleMarker::begin(&path, "test").expect("first marker");
        assert!(!first.previous_unclean_run());
        let recovered = LifecycleMarker::begin(&path, "test").expect("replacement marker");
        assert!(recovered.previous_unclean_run());
        recovered.finish().expect("clean finish");
        let clean = LifecycleMarker::begin(&path, "test").expect("clean restart");
        assert!(!clean.previous_unclean_run());
    }

    #[test]
    fn runtime_permission_matrix_never_silently_noops() {
        let capabilities = runtime_capabilities(&RuntimePermissions {
            accessibility: PermissionState::Granted,
            screen_capture: PermissionState::Denied {
                guidance: "Enable screen recording in system settings".into(),
            },
            audio_duplex: PermissionState::NotDetermined {
                guidance: "Run microphone test".into(),
            },
        });
        assert!(matches!(
            capabilities[0].status,
            CapabilityStatus::Supported
        ));
        assert!(matches!(
            capabilities[1].status,
            CapabilityStatus::Unsupported { .. }
        ));
        assert!(matches!(
            capabilities[2].status,
            CapabilityStatus::Degraded { .. }
        ));
    }

    #[test]
    fn suspend_resume_invalidates_every_os_bound_handle() {
        let mut generations = ResourceGenerations::default();
        generations.resume();
        assert_eq!(
            generations,
            ResourceGenerations {
                audio: 1,
                display: 1,
                browser: 1,
                network: 1,
                provider: 1,
            }
        );
        generations.audio_device_changed();
        generations.display_changed();
        assert_eq!(generations.audio, 2);
        assert_eq!(generations.display, 2);
        assert_eq!(generations.browser, 2);
    }

    #[test]
    fn watchdog_retry_is_bounded_and_resets_after_health_window() {
        let mut backoff = WatchdogBackoff {
            attempts: 0,
            maximum_attempts: 4,
            base_delay_ms: 100,
            maximum_delay_ms: 500,
        };
        assert_eq!(
            (0..5).map(|_| backoff.next_delay()).collect::<Vec<_>>(),
            vec![Some(100), Some(200), Some(400), Some(500), None]
        );
        backoff.healthy_window_completed();
        assert_eq!(backoff.next_delay(), Some(100));
    }
}
