//! Chrome `DevTools` Protocol implementation of [`BrowserEngine`].
//!
//! Design constraints that are load bearing rather than stylistic:
//!
//! * The protocol travels over `--remote-debugging-pipe`, never a TCP debugging
//!   port, so no other local process can drive the agent's browser.
//! * Every session gets its own `--user-data-dir`, ephemeral by default, so two
//!   tasks cannot see each other's cookies, storage, or logins.
//! * Reads are structured protocol calls (`Accessibility.getFullAXTree` plus
//!   `DOMSnapshot`). The engine never evaluates JavaScript, so page text can
//!   never become code.
//! * Requests are judged by [`BrowserPolicy`] inside `Fetch.requestPaused`,
//!   which is below anything the page can influence, and each decision leaves a
//!   content-free [`EgressReceipt`].

mod actions;
mod intercept;
mod launch;
mod snapshot;
#[cfg(test)]
mod tests;
mod transport;

use crate::{
    BrowserEngine, BrowserError, BrowserPolicy, BrowserProfile, EgressReceipt, NodeHandle,
    PageSnapshot, QuarantinedDownload, SnapshotNode,
};
use async_trait::async_trait;
use chromiumoxide_cdp::cdp::browser_protocol::accessibility::{
    EnableParams as AccessibilityEnable, GetFullAxTreeParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::browser::{
    SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::dom::EnableParams as DomEnable;
use chromiumoxide_cdp::cdp::browser_protocol::dom_snapshot::CaptureSnapshotParams;
use chromiumoxide_cdp::cdp::browser_protocol::fetch::{
    EnableParams as FetchEnable, RequestPattern,
};
use chromiumoxide_cdp::cdp::browser_protocol::network::{
    EnableParams as NetworkEnable, GetCookiesParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::{
    EnableParams as PageEnable, GetFrameTreeParams, NavigateParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::target::{
    AttachToTargetParams, CloseTargetParams, CreateTargetParams, GetTargetInfoParams,
    GetTargetsParams,
};
use intercept::NetworkGuard;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Child;
use tokio::task::JoinHandle;
use transport::{CdpClient, CdpEvent};
use url::Url;
use uuid::Uuid;

pub use actions::is_within;
pub use launch::{
    CHROME_PATH_ENV, ChromiumBinary, ChromiumSource, discover_system_chromium,
    fetch_managed_chromium, is_chromium_executable, no_browser_remediation,
};

/// How long to wait for a navigation's load event before snapshotting anyway.
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// `DOMSnapshot.captureSnapshot` requires a non-empty computed-style whitelist.
/// The engine only consumes layout boxes, so this asks for the cheapest property
/// that is always present rather than a real style budget.
const SNAPSHOT_STYLES: [&str; 1] = ["display"];

/// Which `--user-data-dir` a session should use.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "name")]
pub enum ProfileKind {
    /// A fresh directory per task, removed when the session closes. This is what
    /// makes two concurrent goals genuinely unable to observe each other.
    #[default]
    Ephemeral,
    /// A named directory under the profile root that survives across sessions,
    /// for sites the user deliberately wants the agent to stay signed in to.
    Persistent(String),
}

/// Configuration for a managed Chromium session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CdpConfig {
    /// Navigation and subresource policy enforced at the network layer.
    pub policy: BrowserPolicy,
    /// Ephemeral or named-persistent profile selection.
    pub profile: ProfileKind,
    /// Parent directory for all agent-owned browser profiles.
    pub profile_root: PathBuf,
    /// Directory downloads land in; they never enter a trusted workspace.
    pub quarantine_dir: PathBuf,
    /// Cache directory for the hash-verified managed Chromium.
    pub managed_root: PathBuf,
    /// Headful is the default so takeover is possible.
    pub headless: bool,
    /// The browser's own sandbox. Only disable inside an already-sandboxed CI.
    pub sandbox: bool,
    /// Explicit browser binary, taking precedence over discovery.
    pub executable: Option<PathBuf>,
    /// Whether the pinned managed Chromium may be downloaded when nothing is
    /// installed.
    pub allow_managed_download: bool,
    /// Additional switches appended after the managed ones.
    pub extra_arguments: Vec<String>,
}

impl Default for CdpConfig {
    fn default() -> Self {
        let root = std::env::temp_dir().join("personal-agent-browser");
        Self {
            policy: BrowserPolicy {
                allowed_domains: std::collections::BTreeSet::new(),
                blocked_domains: std::collections::BTreeSet::new(),
                allow_third_party_subresources: false,
            },
            profile: ProfileKind::Ephemeral,
            profile_root: root.join("profiles"),
            quarantine_dir: root.join("quarantine"),
            managed_root: root.join("managed"),
            headless: false,
            sandbox: true,
            executable: None,
            allow_managed_download: false,
            extra_arguments: Vec::new(),
        }
    }
}

/// One open browser tab.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub target_id: String,
    pub url: String,
    pub title: String,
    /// Whether this is the tab the agent's actions currently address.
    pub active: bool,
}

/// What an opaque handle resolves to inside the current generation.
#[derive(Clone, Debug)]
struct NodeRecord {
    backend_node_id: i64,
    options: Vec<String>,
}

/// A live browser process and its attached page session.
struct Session {
    child: Child,
    client: CdpClient,
    pump: JoinHandle<()>,
    guard: Arc<NetworkGuard>,
    profile_dir: PathBuf,
    ephemeral: bool,
    target_id: String,
    /// Flattened `Target.attachToTarget` session addressing the active tab.
    attachment_id: String,
    executable: ChromiumBinary,
}

/// Browser engine driving a managed Chromium over a devtools pipe.
pub struct CdpBrowser {
    config: CdpConfig,
    session: Option<Session>,
    page_id: String,
    generation: u64,
    nodes: BTreeMap<String, NodeRecord>,
    current: Option<PageSnapshot>,
    takeover: bool,
    approved_uploads: Vec<PathBuf>,
}

impl CdpBrowser {
    /// Create an engine that has not launched a browser yet.
    #[must_use]
    pub fn new(config: CdpConfig) -> Self {
        Self {
            config,
            session: None,
            page_id: String::new(),
            generation: 0,
            nodes: BTreeMap::new(),
            current: None,
            takeover: false,
            approved_uploads: Vec::new(),
        }
    }

    /// Which browser binary the session is driving.
    #[must_use]
    pub fn binary(&self) -> Option<&ChromiumBinary> {
        self.session.as_ref().map(|session| &session.executable)
    }

    /// The `--user-data-dir` in use, which is the isolation boundary.
    #[must_use]
    pub fn profile_dir(&self) -> Option<&Path> {
        self.session
            .as_ref()
            .map(|session| session.profile_dir.as_path())
    }

    /// Content-free receipts for every request the browser attempted.
    #[must_use]
    pub fn receipts(&self) -> Vec<EgressReceipt> {
        self.session
            .as_ref()
            .map(|session| session.guard.receipts())
            .unwrap_or_default()
    }

    /// Downloads captured into the quarantine directory, still unscanned.
    #[must_use]
    pub fn downloads(&self) -> Vec<QuarantinedDownload> {
        self.session
            .as_ref()
            .map(|session| session.guard.downloads())
            .unwrap_or_default()
    }

    /// Record a filesystem path the user approved for upload.
    ///
    /// `DOM.setFileInputFiles` will refuse anything absent from this list, so a
    /// page can never talk the agent into attaching an arbitrary file.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserError::UploadPathNotApproved`] when the path does not
    /// resolve to an existing file.
    pub fn approve_upload(&mut self, path: &Path) -> Result<(), BrowserError> {
        let resolved = path.canonicalize().map_err(|error| {
            BrowserError::UploadPathNotApproved(format!("{}: {error}", path.display()))
        })?;
        if !resolved.is_file() {
            return Err(BrowserError::UploadPathNotApproved(
                resolved.display().to_string(),
            ));
        }
        if !self.approved_uploads.contains(&resolved) {
            self.approved_uploads.push(resolved);
        }
        Ok(())
    }

    fn session(&self) -> Result<&Session, BrowserError> {
        self.session
            .as_ref()
            .ok_or_else(|| BrowserError::Unavailable("no browser session is open".into()))
    }

    /// Reject agent-driven work while the human is holding the window.
    fn guard_takeover(&self) -> Result<(), BrowserError> {
        if self.takeover {
            return Err(BrowserError::TakeoverActive);
        }
        Ok(())
    }

    async fn resolve_binary(&self) -> Result<ChromiumBinary, BrowserError> {
        if let Some(found) = discover_system_chromium(self.config.executable.as_deref()) {
            return Ok(found);
        }
        if self.config.allow_managed_download {
            return fetch_managed_chromium(&self.config.managed_root).await;
        }
        Err(BrowserError::Unavailable(format!(
            "no Chromium-family browser found: {}",
            no_browser_remediation()
        )))
    }

    fn profile_dir_for(&self, profile_id: &str) -> Result<(PathBuf, bool), BrowserError> {
        match &self.config.profile {
            ProfileKind::Ephemeral => Ok((
                self.config.profile_root.join(format!(
                    "task-{}-{}",
                    sanitize(profile_id)?,
                    Uuid::new_v4()
                )),
                true,
            )),
            ProfileKind::Persistent(name) => Ok((
                self.config
                    .profile_root
                    .join("persistent")
                    .join(sanitize(name)?),
                false,
            )),
        }
    }

    /// Launch the browser, attach to a page, and arm policy enforcement.
    async fn start(&mut self, profile_id: &str) -> Result<(), BrowserError> {
        let binary = self.resolve_binary().await?;
        let (profile_dir, ephemeral) = self.profile_dir_for(profile_id)?;
        create_dir(&profile_dir)?;
        create_dir(&self.config.quarantine_dir)?;

        let launched = launch::launch(
            &binary.path,
            &profile_dir,
            self.config.headless,
            self.config.sandbox,
            &self.config.extra_arguments,
        )?;
        let client = CdpClient::start(launched.reader, launched.writer);
        let guard = Arc::new(NetworkGuard::new(
            self.config.policy.clone(),
            self.config.quarantine_dir.clone(),
        ));
        // Armed before any domain is enabled so no request can slip past policy.
        let pump = intercept::spawn(client.clone(), Arc::clone(&guard));

        let target_id: String = client
            .send(None, &CreateTargetParams::new("about:blank"))
            .await?
            .target_id
            .into();
        let session_id: String = client
            .send(
                None,
                &AttachToTargetParams::builder()
                    .target_id(target_id.clone())
                    .flatten(true)
                    .build()
                    .map_err(BrowserError::Operation)?,
            )
            .await?
            .session_id
            .into();

        arm_session(&client, &session_id).await?;
        set_download_behavior(&client, &self.config.quarantine_dir).await?;
        let frame_tree = client
            .send(Some(&session_id), &GetFrameTreeParams::default())
            .await?;
        guard.set_main_frame(Some(frame_tree.frame_tree.frame.id.inner().clone()));

        self.page_id.clone_from(&target_id);
        self.generation = 0;
        self.session = Some(Session {
            child: launched.child,
            client,
            pump,
            guard,
            profile_dir,
            ephemeral,
            target_id,
            attachment_id: session_id,
            executable: binary,
        });
        Ok(())
    }

    /// Wait for the load event, or fall through when the page never loads.
    async fn await_load(
        &self,
        mut events: tokio::sync::broadcast::Receiver<CdpEvent>,
    ) -> Result<(), BrowserError> {
        let session_id = self.session()?.attachment_id.clone();
        let wait = async {
            while let Ok(event) = events.recv().await {
                let matches_session = event.session_id.as_deref() == Some(session_id.as_str());
                if matches_session
                    && matches!(
                        event.method.as_str(),
                        "Page.loadEventFired" | "Page.frameStoppedLoading"
                    )
                {
                    return;
                }
            }
        };
        let _ = tokio::time::timeout(LOAD_TIMEOUT, wait).await;
        Ok(())
    }

    /// Read the page through the accessibility tree and the DOM snapshot.
    async fn read_page(&mut self) -> Result<PageSnapshot, BrowserError> {
        let session = self.session()?;
        let client = session.client.clone();
        let session_id = session.attachment_id.clone();
        let target_id = session.target_id.clone();

        let info = client
            .send(
                None,
                &GetTargetInfoParams::builder().target_id(target_id).build(),
            )
            .await?
            .target_info;
        let ax = client
            .send(Some(&session_id), &GetFullAxTreeParams::default())
            .await?;
        let dom = client
            .send(
                Some(&session_id),
                &CaptureSnapshotParams::new(SNAPSHOT_STYLES.map(String::from).to_vec()),
            )
            .await?;

        let merged = snapshot::merge(&ax.nodes, &dom);
        let url = Url::parse(&info.url)
            .unwrap_or_else(|_| Url::parse("about:blank").expect("constant URL"));
        self.session()?.guard.set_page_url(Some(url.clone()));
        Ok(self.bind_generation(merged, url, info.title))
    }

    /// Bind a generation-independent read to a fresh generation, invalidating
    /// every handle the caller was still holding.
    fn bind_generation(
        &mut self,
        merged: snapshot::MergedSnapshot,
        url: Url,
        title: String,
    ) -> PageSnapshot {
        self.generation = self.generation.saturating_add(1);
        self.nodes.clear();
        let mut handles = Vec::with_capacity(merged.nodes.len());
        let mut nodes = Vec::with_capacity(merged.nodes.len());
        for node in merged.nodes {
            let handle = NodeHandle {
                page_id: self.page_id.clone(),
                generation: self.generation,
                opaque_id: node.backend_node_id.to_string(),
            };
            self.nodes.insert(
                handle.opaque_id.clone(),
                NodeRecord {
                    backend_node_id: node.backend_node_id,
                    options: node.options.clone(),
                },
            );
            handles.push(handle.clone());
            nodes.push(SnapshotNode {
                handle,
                role: node.role,
                name: node.name,
                value: node.value,
                editable: node.editable,
                bounds: node.bounds,
                options: node.options,
            });
        }
        let snapshot = PageSnapshot {
            page_id: self.page_id.clone(),
            generation: self.generation,
            url,
            title,
            text: merged.text,
            handles,
            nodes,
        };
        self.current = Some(snapshot.clone());
        snapshot
    }

    /// Every cookie visible to this profile, used to prove profile isolation.
    ///
    /// # Errors
    ///
    /// Returns a [`BrowserError`] when no session is open or the protocol call
    /// fails.
    pub async fn cookie_names(&self) -> Result<Vec<String>, BrowserError> {
        let session = self.session()?;
        let cookies = session
            .client
            .send(Some(&session.attachment_id), &GetCookiesParams::default())
            .await?;
        Ok(cookies
            .cookies
            .into_iter()
            .map(|cookie| cookie.name)
            .collect())
    }

    /// List the browser's open page targets.
    ///
    /// # Errors
    ///
    /// Returns a [`BrowserError`] when no session is open or the protocol call
    /// fails.
    pub async fn tabs(&self) -> Result<Vec<TabInfo>, BrowserError> {
        let session = self.session()?;
        let targets = session
            .client
            .send(None, &GetTargetsParams::default())
            .await?;
        Ok(targets
            .target_infos
            .into_iter()
            .filter(|info| info.r#type == "page")
            .map(|info| {
                let target_id: String = info.target_id.into();
                TabInfo {
                    active: target_id == session.target_id,
                    target_id,
                    url: info.url,
                    title: info.title,
                }
            })
            .collect())
    }

    /// Open a new tab and make it the tab the agent acts on.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserError::DomainBlocked`] when policy refuses the URL, or a
    /// protocol error.
    pub async fn open_tab(&mut self, url: &Url) -> Result<PageSnapshot, BrowserError> {
        self.guard_takeover()?;
        self.config.policy.allow_navigation(url)?;
        let session = self.session()?;
        let client = session.client.clone();
        let target_id: String = client
            .send(None, &CreateTargetParams::new("about:blank"))
            .await?
            .target_id
            .into();
        let session_id: String = client
            .send(
                None,
                &AttachToTargetParams::builder()
                    .target_id(target_id.clone())
                    .flatten(true)
                    .build()
                    .map_err(BrowserError::Operation)?,
            )
            .await?
            .session_id
            .into();
        arm_session(&client, &session_id).await?;
        if let Some(session) = self.session.as_mut() {
            session.target_id.clone_from(&target_id);
            session.attachment_id = session_id;
        }
        self.page_id = target_id;
        self.navigate(url).await
    }

    /// Close a tab. Closing the active tab requires opening another one next.
    ///
    /// # Errors
    ///
    /// Returns a [`BrowserError`] when no session is open or the protocol call
    /// fails.
    pub async fn close_tab(&mut self, target_id: &str) -> Result<(), BrowserError> {
        self.guard_takeover()?;
        let session = self.session()?;
        session
            .client
            .send(None, &CloseTargetParams::new(target_id.to_owned()))
            .await?;
        Ok(())
    }

    /// Take the browser back from the user and re-read the page.
    ///
    /// The user may have navigated or typed anywhere while holding the window,
    /// so resuming always produces a fresh generation rather than trusting any
    /// handle the agent held before the takeover.
    ///
    /// # Errors
    ///
    /// Returns a [`BrowserError`] when no session is open.
    pub async fn resume_from_takeover(&mut self) -> Result<PageSnapshot, BrowserError> {
        self.session()?;
        self.takeover = false;
        // The user may have navigated anywhere; every prior handle is void.
        self.snapshot().await
    }
}

fn sanitize(name: &str) -> Result<String, BrowserError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        || trimmed.starts_with('.')
    {
        return Err(BrowserError::Operation(format!(
            "profile name {name:?} must be non-empty ASCII alphanumeric with '-', '_', or '.'"
        )));
    }
    Ok(trimmed.to_owned())
}

fn create_dir(path: &Path) -> Result<(), BrowserError> {
    std::fs::create_dir_all(path).map_err(|error| {
        BrowserError::Operation(format!("cannot create {}: {error}", path.display()))
    })
}

/// Enable exactly the domains the engine needs, with `Fetch` last so policy is
/// live before the page can request anything.
async fn arm_session(client: &CdpClient, session_id: &str) -> Result<(), BrowserError> {
    let session = Some(session_id);
    client.send(session, &PageEnable::default()).await?;
    client.send(session, &DomEnable::default()).await?;
    client
        .send(session, &AccessibilityEnable::default())
        .await?;
    client.send(session, &NetworkEnable::default()).await?;
    client
        .send(
            session,
            &FetchEnable::builder()
                .pattern(RequestPattern::builder().url_pattern("*").build())
                .build(),
        )
        .await?;
    Ok(())
}

async fn set_download_behavior(client: &CdpClient, quarantine: &Path) -> Result<(), BrowserError> {
    client
        .send(
            None,
            &SetDownloadBehaviorParams::builder()
                .behavior(SetDownloadBehaviorBehavior::AllowAndName)
                .download_path(quarantine.display().to_string())
                .events_enabled(true)
                .build()
                .map_err(BrowserError::Operation)?,
        )
        .await?;
    Ok(())
}

#[async_trait]
impl BrowserEngine for CdpBrowser {
    async fn open_isolated_profile(&mut self, profile_id: &str) -> Result<(), BrowserError> {
        if self.session.is_some() {
            return Err(BrowserError::Operation(
                "browser session is already open".into(),
            ));
        }
        BrowserProfile {
            id: profile_id.into(),
            isolated: true,
            personal_profile_opt_in: false,
        }
        .validate()?;
        self.start(profile_id).await
    }

    async fn navigate(&mut self, url: &Url) -> Result<PageSnapshot, BrowserError> {
        self.guard_takeover()?;
        self.config.policy.allow_navigation(url)?;
        let session = self.session()?;
        let client = session.client.clone();
        let session_id = session.attachment_id.clone();
        session.guard.set_page_url(Some(url.clone()));

        let events = client.subscribe();
        let outcome = client
            .send(Some(&session_id), &NavigateParams::new(url.to_string()))
            .await?;
        if let Some(error) = outcome.error_text {
            return Err(BrowserError::Operation(format!(
                "navigation to {url} failed: {error}"
            )));
        }
        self.await_load(events).await?;
        self.snapshot().await
    }

    async fn snapshot(&mut self) -> Result<PageSnapshot, BrowserError> {
        self.read_page().await
    }

    async fn click(&mut self, handle: &NodeHandle) -> Result<PageSnapshot, BrowserError> {
        self.click_node(handle).await
    }

    async fn type_text(
        &mut self,
        handle: &NodeHandle,
        text: &str,
    ) -> Result<PageSnapshot, BrowserError> {
        self.type_into(handle, text).await
    }

    async fn takeover(&mut self) -> Result<(), BrowserError> {
        self.session()?;
        self.takeover = true;
        // Handles taken before a takeover must never be replayed afterwards.
        self.nodes.clear();
        self.current = None;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), BrowserError> {
        let Some(mut session) = self.session.take() else {
            return Ok(());
        };
        session.pump.abort();
        let _ = session.child.kill().await;
        let _ = session.child.wait().await;
        self.nodes.clear();
        self.current = None;
        self.takeover = false;
        if session.ephemeral {
            // The isolation promise only holds if the directory really goes
            // away, so a failure here is reported rather than swallowed.
            remove_ephemeral_profile(&session.profile_dir).await?;
        }
        Ok(())
    }
}

/// Delete an ephemeral profile directory, waiting for Chromium to let go of it.
///
/// Killing the browser does not synchronously reap its zygote, renderer, and GPU
/// children, and those keep writing into the profile while they exit. A single
/// removal therefore fails intermittently under load with a non-empty directory,
/// which would leave cookies and cache from an "ephemeral" task on disk. Retry
/// briefly, then surface the failure.
async fn remove_ephemeral_profile(profile_dir: &Path) -> Result<(), BrowserError> {
    const ATTEMPTS: u32 = 50;
    const RETRY_DELAY: Duration = Duration::from_millis(40);

    let mut last_error = None;
    for attempt in 0..ATTEMPTS {
        match std::fs::remove_dir_all(profile_dir) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < ATTEMPTS {
            tokio::time::sleep(RETRY_DELAY).await;
        }
    }
    Err(BrowserError::Unavailable(format!(
        "the ephemeral browser profile could not be deleted, so task data would \
         outlive the task: {}",
        last_error.map_or_else(|| "unknown error".to_owned(), |error| error.to_string())
    )))
}

impl Drop for CdpBrowser {
    fn drop(&mut self) {
        if let Some(session) = &self.session {
            session.pump.abort();
            if session.ephemeral {
                // Drop cannot await, so retry briefly on the calling thread.
                // `close()` is the supported path and reports failures properly.
                for _ in 0..50u32 {
                    match std::fs::remove_dir_all(&session.profile_dir) {
                        Ok(()) => break,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                        Err(_) => std::thread::sleep(Duration::from_millis(40)),
                    }
                }
            }
        }
    }
}
