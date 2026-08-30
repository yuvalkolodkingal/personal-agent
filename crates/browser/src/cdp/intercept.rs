//! Network-layer policy enforcement and download quarantine.
//!
//! Every request the renderer makes is paused by `Fetch.enable` and judged
//! against [`BrowserPolicy`] before it is allowed to leave the machine. The page
//! cannot widen this: the decision happens in the browser process on behalf of
//! the agent, not in page script.

use super::transport::{CdpClient, CdpEvent};
use crate::cdp::launch::hex;
use crate::{
    BrowserError, BrowserPolicy, DownloadState, EgressDecision, EgressReceipt, QuarantinedDownload,
};
use chromiumoxide_cdp::cdp::browser_protocol::browser::{
    EventDownloadProgress, EventDownloadWillBegin,
};
use chromiumoxide_cdp::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EventRequestPaused, FailRequestParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::network::{ErrorReason, ResourceType};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::task::JoinHandle;
use url::Url;

/// Schemes that never reach the network and are therefore not policy-relevant.
const LOCAL_SCHEMES: &[&str] = &["about", "data", "blob"];

/// Shared, page-independent state for one browser session.
#[derive(Debug)]
pub(crate) struct NetworkGuard {
    policy: BrowserPolicy,
    page_url: Mutex<Option<Url>>,
    main_frame: Mutex<Option<String>>,
    receipts: Mutex<Vec<EgressReceipt>>,
    pending_downloads: Mutex<BTreeMap<String, Url>>,
    downloads: Mutex<Vec<QuarantinedDownload>>,
    quarantine: PathBuf,
    sequence: AtomicU64,
}

impl NetworkGuard {
    pub(crate) fn new(policy: BrowserPolicy, quarantine: PathBuf) -> Self {
        Self {
            policy,
            page_url: Mutex::new(None),
            main_frame: Mutex::new(None),
            receipts: Mutex::new(Vec::new()),
            pending_downloads: Mutex::new(BTreeMap::new()),
            downloads: Mutex::new(Vec::new()),
            quarantine,
            sequence: AtomicU64::new(0),
        }
    }

    pub(crate) fn policy(&self) -> &BrowserPolicy {
        &self.policy
    }

    pub(crate) fn quarantine(&self) -> &Path {
        &self.quarantine
    }

    pub(crate) fn set_page_url(&self, url: Option<Url>) {
        *self.page_url.lock().unwrap_or_else(PoisonError::into_inner) = url;
    }

    pub(crate) fn set_main_frame(&self, frame_id: Option<String>) {
        *self
            .main_frame
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = frame_id;
    }

    pub(crate) fn receipts(&self) -> Vec<EgressReceipt> {
        self.receipts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn downloads(&self) -> Vec<QuarantinedDownload> {
        self.downloads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn is_main_document(&self, event: &EventRequestPaused) -> bool {
        if event.resource_type != ResourceType::Document {
            return false;
        }
        let frame: String = event.frame_id.clone().into();
        self.main_frame
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_none_or(|main| main == &frame)
    }

    fn record(&self, receipt: EgressReceipt) {
        self.receipts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(receipt);
    }

    fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }
}

/// Judge one request without any I/O so the rule can be unit tested directly.
pub(crate) fn decide(
    policy: &BrowserPolicy,
    page: Option<&Url>,
    resource: &Url,
    main_document: bool,
) -> Result<(), BrowserError> {
    if LOCAL_SCHEMES.contains(&resource.scheme()) {
        return Ok(());
    }
    if main_document {
        return policy.allow_navigation(resource);
    }
    match page {
        Some(page) => policy.allow_subresource(page, resource),
        None => policy.allow_navigation(resource),
    }
}

/// Build the content-free receipt for one decision.
fn receipt(
    sequence: u64,
    method: &str,
    resource: &Url,
    resource_type: &ResourceType,
    outcome: &Result<(), BrowserError>,
) -> EgressReceipt {
    EgressReceipt {
        sequence,
        method: method.to_owned(),
        scheme: resource.scheme().to_owned(),
        host: resource.host_str().unwrap_or("").to_owned(),
        path_digest: hex(&Sha256::digest(resource.path().as_bytes())),
        resource_type: format!("{resource_type:?}"),
        decision: if outcome.is_ok() {
            EgressDecision::Allowed
        } else {
            EgressDecision::Blocked
        },
        reason: outcome.as_ref().err().map(ToString::to_string),
    }
}

/// Run the interception and download pump until the protocol channel closes.
pub(crate) fn spawn(client: CdpClient, guard: Arc<NetworkGuard>) -> JoinHandle<()> {
    let mut events = client.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event.method.as_str() {
                "Fetch.requestPaused" => {
                    handle_request(&client, &guard, &event).await;
                }
                "Browser.downloadWillBegin" => handle_download_start(&guard, &event),
                "Browser.downloadProgress" => handle_download_progress(&guard, &event),
                _ => {}
            }
        }
    })
}

async fn handle_request(client: &CdpClient, guard: &NetworkGuard, event: &CdpEvent) {
    let Ok(paused) = event.parse::<EventRequestPaused>() else {
        return;
    };
    let session = event.session_id.as_deref();
    let Ok(resource) = Url::parse(&paused.request.url) else {
        let _ = client
            .send(
                session,
                &FailRequestParams::new(paused.request_id, ErrorReason::BlockedByClient),
            )
            .await;
        return;
    };
    let page = guard
        .page_url
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let outcome = decide(
        guard.policy(),
        page.as_ref(),
        &resource,
        guard.is_main_document(&paused),
    );
    guard.record(receipt(
        guard.next_sequence(),
        &paused.request.method,
        &resource,
        &paused.resource_type,
        &outcome,
    ));
    let request_id = paused.request_id;
    if outcome.is_ok() {
        let _ = client
            .send(session, &ContinueRequestParams::new(request_id))
            .await;
    } else {
        let _ = client
            .send(
                session,
                &FailRequestParams::new(request_id, ErrorReason::BlockedByClient),
            )
            .await;
    }
}

fn handle_download_start(guard: &NetworkGuard, event: &CdpEvent) {
    let Ok(started) = event.parse::<EventDownloadWillBegin>() else {
        return;
    };
    let Ok(source) = Url::parse(&started.url) else {
        return;
    };
    guard
        .pending_downloads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(started.guid, source);
}

fn handle_download_progress(guard: &NetworkGuard, event: &CdpEvent) {
    use chromiumoxide_cdp::cdp::browser_protocol::browser::DownloadProgressState;
    let Ok(progress) = event.parse::<EventDownloadProgress>() else {
        return;
    };
    if progress.state != DownloadProgressState::Completed {
        if progress.state == DownloadProgressState::Canceled {
            guard
                .pending_downloads
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&progress.guid);
        }
        return;
    }
    let Some(source) = guard
        .pending_downloads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&progress.guid)
    else {
        return;
    };
    let path = progress
        .file_path
        .map_or_else(|| guard.quarantine().join(&progress.guid), PathBuf::from);
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    guard
        .downloads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(QuarantinedDownload {
            sha256: hex(&Sha256::digest(&bytes)),
            source,
            quarantine_path: path.display().to_string(),
            // Bytes stay in the quarantine directory in the `Quarantined` state;
            // only the existing scanner may promote them.
            state: DownloadState::Quarantined,
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn policy() -> BrowserPolicy {
        BrowserPolicy {
            allowed_domains: ["fixture.test".into()].into(),
            blocked_domains: ["tracker.test".into()].into(),
            allow_third_party_subresources: false,
        }
    }

    fn url(text: &str) -> Url {
        Url::parse(text).expect("fixture url")
    }

    #[test]
    fn main_document_navigation_uses_the_navigation_rule() {
        assert!(decide(&policy(), None, &url("https://fixture.test/a"), true).is_ok());
        assert!(matches!(
            decide(&policy(), None, &url("https://tracker.test/a"), true),
            Err(BrowserError::DomainBlocked(_))
        ));
    }

    #[test]
    fn subresources_from_a_third_party_are_refused_at_the_network_layer() {
        let page = url("https://fixture.test/page");
        assert!(
            decide(
                &policy(),
                Some(&page),
                &url("https://fixture.test/app.js"),
                false
            )
            .is_ok()
        );
        assert!(matches!(
            decide(
                &policy(),
                Some(&page),
                &url("https://tracker.test/pixel.gif"),
                false
            ),
            Err(BrowserError::DomainBlocked(_))
        ));
    }

    #[test]
    fn non_network_schemes_are_not_policy_relevant_but_file_urls_still_are() {
        assert!(decide(&policy(), None, &url("about:blank"), true).is_ok());
        assert!(decide(&policy(), None, &url("data:text/html,x"), false).is_ok());
        assert!(matches!(
            decide(&policy(), None, &url("file:///etc/passwd"), true),
            Err(BrowserError::SchemeBlocked(_))
        ));
    }

    #[test]
    fn receipts_carry_no_path_query_or_body_content() {
        let resource = url("https://fixture.test/secret/path?token=abcd1234#fragment");
        let allowed = receipt(
            7,
            "GET",
            &resource,
            &ResourceType::Document,
            &Ok::<(), BrowserError>(()),
        );
        let serialized = serde_json::to_string(&allowed).expect("serialize receipt");
        assert!(!serialized.contains("abcd1234"), "{serialized}");
        assert!(!serialized.contains("secret"), "{serialized}");
        assert!(!serialized.contains("fragment"), "{serialized}");
        assert_eq!(allowed.host, "fixture.test");
        assert_eq!(allowed.decision, EgressDecision::Allowed);
        assert_eq!(allowed.path_digest.len(), 64);

        let blocked = receipt(
            8,
            "GET",
            &resource,
            &ResourceType::Image,
            &Err(BrowserError::DomainBlocked("fixture.test".into())),
        );
        assert_eq!(blocked.decision, EgressDecision::Blocked);
        assert!(blocked.reason.is_some());
    }

    #[test]
    fn a_guard_starts_with_no_receipts_and_no_downloads() {
        let guard = NetworkGuard::new(
            BrowserPolicy {
                allowed_domains: BTreeSet::new(),
                blocked_domains: BTreeSet::new(),
                allow_third_party_subresources: false,
            },
            PathBuf::from("/tmp/quarantine"),
        );
        assert!(guard.receipts().is_empty());
        assert!(guard.downloads().is_empty());
        assert_eq!(guard.quarantine(), Path::new("/tmp/quarantine"));
    }
}
