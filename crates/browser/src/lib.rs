//! Replaceable browser engine with isolated profiles, untrusted-page policy, and invalidatable handles.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;
use url::Url;

mod webdriver;

pub use webdriver::{WebDriverBrowser, WebDriverConfig, WebDriverProcess};

/// Opaque node handle valid only for one page generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeHandle {
    pub page_id: String,
    pub generation: u64,
    pub opaque_id: String,
}

/// Structured page representation preferred over pixels.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageSnapshot {
    pub page_id: String,
    pub generation: u64,
    pub url: Url,
    pub title: String,
    pub text: String,
    pub handles: Vec<NodeHandle>,
}

/// Trust carried by browser task inputs. Page-derived instructions are always untrusted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserInputTrust {
    UserInstruction,
    UntrustedPage,
}

/// Browser effects evaluated before they can reach CDP.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserEffect {
    Observe,
    Navigate,
    Type,
    Download,
    Submit,
    ReadSecrets,
    ConnectorAccess,
}

/// Dedicated profile declaration. Personal profiles require an explicit opt-in.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BrowserProfile {
    pub id: String,
    pub isolated: bool,
    pub personal_profile_opt_in: bool,
}

impl BrowserProfile {
    /// Validate that normal agent sessions remain isolated.
    ///
    /// # Errors
    ///
    /// Rejects blank identifiers and implicit use of a personal browser profile.
    pub fn validate(&self) -> Result<(), BrowserError> {
        if self.id.trim().is_empty() {
            return Err(BrowserError::Operation("profile ID cannot be blank".into()));
        }
        if !self.isolated && !self.personal_profile_opt_in {
            return Err(BrowserError::PersonalProfileConsentRequired);
        }
        Ok(())
    }
}

/// Navigation, subresource, and data-zone policy applied outside page control.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BrowserPolicy {
    pub allowed_domains: BTreeSet<String>,
    pub blocked_domains: BTreeSet<String>,
    pub allow_third_party_subresources: bool,
}

impl BrowserPolicy {
    /// Validate a top-level navigation using exact or `*.example.com` domain rules.
    ///
    /// # Errors
    ///
    /// Rejects non-HTTP(S), blocked, or non-allowlisted destinations.
    pub fn allow_navigation(&self, url: &Url) -> Result<(), BrowserError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(BrowserError::SchemeBlocked(url.scheme().into()));
        }
        let host = url
            .host_str()
            .ok_or_else(|| BrowserError::DomainBlocked("missing host".into()))?;
        if self
            .blocked_domains
            .iter()
            .any(|rule| domain_matches(rule, host))
        {
            return Err(BrowserError::DomainBlocked(host.into()));
        }
        if !self.allowed_domains.is_empty()
            && !self
                .allowed_domains
                .iter()
                .any(|rule| domain_matches(rule, host))
        {
            return Err(BrowserError::DomainBlocked(host.into()));
        }
        Ok(())
    }

    /// Validate a subresource without letting pages widen their own network policy.
    ///
    /// # Errors
    ///
    /// Rejects blocked or third-party subresources unless explicitly allowed.
    pub fn allow_subresource(&self, page: &Url, resource: &Url) -> Result<(), BrowserError> {
        self.allow_navigation(resource)?;
        if !self.allow_third_party_subresources && page.host_str() != resource.host_str() {
            return Err(BrowserError::ThirdPartySubresourceBlocked(
                resource.host_str().unwrap_or("missing host").into(),
            ));
        }
        Ok(())
    }

    /// Page-controlled work may observe or navigate, but cannot cross into secrets,
    /// connectors, downloads, or real-world form submission without external approval.
    ///
    /// # Errors
    ///
    /// Returns `CrossZoneApprovalRequired` for a prohibited page-controlled effect.
    pub fn allow_effect(
        &self,
        trust: BrowserInputTrust,
        effect: BrowserEffect,
        cross_zone_approval: bool,
    ) -> Result<(), BrowserError> {
        let sensitive = matches!(
            effect,
            BrowserEffect::Download
                | BrowserEffect::Submit
                | BrowserEffect::ReadSecrets
                | BrowserEffect::ConnectorAccess
        );
        if trust == BrowserInputTrust::UntrustedPage && sensitive && !cross_zone_approval {
            return Err(BrowserError::CrossZoneApprovalRequired(effect));
        }
        Ok(())
    }
}

fn domain_matches(rule: &str, host: &str) -> bool {
    rule == host
        || rule
            .strip_prefix("*.")
            .is_some_and(|suffix| host != suffix && host.ends_with(&format!(".{suffix}")))
}

/// Download state before any file can enter a trusted workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Quarantined,
    ScanPassed,
    Rejected,
    Released,
}

/// Content-addressed download record; bytes remain in a quarantine-owned location.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct QuarantinedDownload {
    pub sha256: String,
    pub source: Url,
    pub quarantine_path: String,
    pub state: DownloadState,
}

impl QuarantinedDownload {
    /// A release is legal only after a scanner has passed the exact content hash.
    ///
    /// # Errors
    ///
    /// Returns `DownloadNotScanned` unless the state is `ScanPassed`.
    pub fn release(&mut self) -> Result<(), BrowserError> {
        if self.state != DownloadState::ScanPassed {
            return Err(BrowserError::DownloadNotScanned);
        }
        self.state = DownloadState::Released;
        Ok(())
    }
}

/// Browser operation failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum BrowserError {
    #[error("domain is blocked by policy: {0}")]
    DomainBlocked(String),
    #[error("URL scheme is blocked by browser policy: {0}")]
    SchemeBlocked(String),
    #[error("third-party subresource is blocked by browser policy: {0}")]
    ThirdPartySubresourceBlocked(String),
    #[error("using a personal browser profile requires explicit opt-in")]
    PersonalProfileConsentRequired,
    #[error("untrusted page content requires cross-zone approval for {0:?}")]
    CrossZoneApprovalRequired(BrowserEffect),
    #[error("download cannot leave quarantine before a successful scan")]
    DownloadNotScanned,
    #[error("page handle is stale and must be reacquired")]
    StaleHandle,
    #[error("browser capability unavailable: {0}")]
    Unavailable(String),
    #[error("browser operation failed: {0}")]
    Operation(String),
}

/// CDP is an implementation detail behind this engine boundary.
#[async_trait]
pub trait BrowserEngine: Send {
    async fn open_isolated_profile(&mut self, profile_id: &str) -> Result<(), BrowserError>;
    async fn navigate(&mut self, url: &Url) -> Result<PageSnapshot, BrowserError>;
    async fn snapshot(&mut self) -> Result<PageSnapshot, BrowserError>;
    async fn click(&mut self, handle: &NodeHandle) -> Result<PageSnapshot, BrowserError>;
    async fn type_text(
        &mut self,
        handle: &NodeHandle,
        text: &str,
    ) -> Result<PageSnapshot, BrowserError>;
    async fn takeover(&mut self) -> Result<(), BrowserError>;
    async fn close(&mut self) -> Result<(), BrowserError>;
}

/// Reject a handle after navigation or any page-changing action.
///
/// # Errors
///
/// Returns `StaleHandle` when page identity or generation differs.
pub fn validate_handle(snapshot: &PageSnapshot, handle: &NodeHandle) -> Result<(), BrowserError> {
    if handle.page_id == snapshot.page_id && handle.generation == snapshot.generation {
        Ok(())
    } else {
        Err(BrowserError::StaleHandle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changing_generation_invalidates_handles() {
        let handle = NodeHandle {
            page_id: "p".into(),
            generation: 1,
            opaque_id: "n1".into(),
        };
        let page = PageSnapshot {
            page_id: "p".into(),
            generation: 2,
            url: Url::parse("https://example.com").unwrap(),
            title: String::new(),
            text: String::new(),
            handles: vec![],
        };
        assert_eq!(
            validate_handle(&page, &handle),
            Err(BrowserError::StaleHandle)
        );
    }

    #[test]
    fn isolated_profile_and_domain_policy_fail_closed() {
        let profile = BrowserProfile {
            id: "goal-1".into(),
            isolated: true,
            personal_profile_opt_in: false,
        };
        profile.validate().expect("isolated profile");
        assert_eq!(
            BrowserProfile {
                isolated: false,
                ..profile
            }
            .validate(),
            Err(BrowserError::PersonalProfileConsentRequired)
        );
        let policy = BrowserPolicy {
            allowed_domains: ["example.test".into(), "*.assets.test".into()].into(),
            blocked_domains: ["blocked.assets.test".into()].into(),
            allow_third_party_subresources: false,
        };
        policy
            .allow_navigation(&Url::parse("https://example.test/form").unwrap())
            .expect("fixture allowed");
        assert!(
            policy
                .allow_navigation(&Url::parse("file:///etc/passwd").unwrap())
                .is_err()
        );
        assert!(
            policy
                .allow_navigation(&Url::parse("https://blocked.assets.test/x").unwrap())
                .is_err()
        );
    }

    #[test]
    fn malicious_page_instructions_cannot_cross_data_zones() {
        let policy = BrowserPolicy {
            allowed_domains: BTreeSet::new(),
            blocked_domains: BTreeSet::new(),
            allow_third_party_subresources: false,
        };
        for effect in [
            BrowserEffect::ReadSecrets,
            BrowserEffect::ConnectorAccess,
            BrowserEffect::Submit,
            BrowserEffect::Download,
        ] {
            assert_eq!(
                policy.allow_effect(BrowserInputTrust::UntrustedPage, effect, false),
                Err(BrowserError::CrossZoneApprovalRequired(effect))
            );
        }
        policy
            .allow_effect(
                BrowserInputTrust::UntrustedPage,
                BrowserEffect::Observe,
                false,
            )
            .expect("structured observation remains safe");
    }

    #[test]
    fn download_stays_quarantined_until_exact_hash_passes_scan() {
        let mut download = QuarantinedDownload {
            sha256: "a".repeat(64),
            source: Url::parse("https://example.test/file").unwrap(),
            quarantine_path: "quarantine/a".into(),
            state: DownloadState::Quarantined,
        };
        assert_eq!(download.release(), Err(BrowserError::DownloadNotScanned));
        download.state = DownloadState::ScanPassed;
        download.release().expect("release");
        assert_eq!(download.state, DownloadState::Released);
    }
}
