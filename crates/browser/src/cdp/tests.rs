//! Live tests for the CDP engine against a real Chromium.
//!
//! These drive an actual browser over `--remote-debugging-pipe`.
//!
//! * If a Chromium-family browser is installed, or `PERSONAL_AGENT_CHROME`
//!   points at one, they simply run.
//! * With `PERSONAL_AGENT_CDP_LIVE_TEST=1` and no browser installed, they
//!   provision the hash-verified pinned Chromium once and run against it. This
//!   is the CI mode.
//! * Otherwise they print the exact remediation and return, following the same
//!   convention as the other pinned-runtime live tests in this repository.

mod fixture_server;

use super::{CdpBrowser, CdpConfig, ProfileKind, discover_system_chromium, fetch_managed_chromium};
use crate::cdp::launch::CHROME_PATH_ENV;
use crate::{
    BrowserEngine, BrowserError, BrowserPolicy, DownloadState, EgressDecision, PageSnapshot,
    SnapshotNode,
};
use fixture_server::{DOWNLOAD_BODY, FixtureServer, ISOLATION_COOKIE};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::OnceCell;

/// Opt-in switch that allows provisioning the pinned browser for CI.
const LIVE_ENV: &str = "PERSONAL_AGENT_CDP_LIVE_TEST";
/// Escape hatch for environments where the Chromium sandbox cannot start.
const NO_SANDBOX_ENV: &str = "PERSONAL_AGENT_CHROME_NO_SANDBOX";

/// Downloaded at most once per test binary, shared by every live test.
static PROVISIONED: OnceCell<PathBuf> = OnceCell::const_new();

/// Resolve the browser these tests should drive, or explain what is missing.
async fn browser_executable() -> Option<PathBuf> {
    if let Some(binary) = discover_system_chromium(None) {
        return Some(binary.path);
    }
    if std::env::var(LIVE_ENV).as_deref() != Ok("1") {
        eprintln!(
            "skipping the CDP engine test: install Chrome/Chromium/Edge, or set \
             {CHROME_PATH_ENV} to a Chromium-family binary, or set {LIVE_ENV}=1 to let the \
             test provision the pinned Chromium"
        );
        return None;
    }
    let cache = std::env::temp_dir().join("personal-agent-browser-test-chromium");
    let path = PROVISIONED
        .get_or_init(|| async {
            fetch_managed_chromium(&cache)
                .await
                .expect("provision the pinned Chromium for the live tests")
                .path
        })
        .await;
    Some(path.clone())
}

/// Everything one live test needs: a fixture origin and a scratch workspace.
struct Harness {
    server: FixtureServer,
    workspace: tempfile::TempDir,
    executable: PathBuf,
}

impl Harness {
    /// Returns `None`, after explaining what to install, when this machine has
    /// no browser to drive and provisioning was not requested.
    async fn start() -> Option<Self> {
        let executable = browser_executable().await?;
        Some(Self {
            server: FixtureServer::start().await,
            workspace: tempfile::tempdir().expect("scratch workspace"),
            executable,
        })
    }

    fn config(&self) -> CdpConfig {
        let root = self.workspace.path();
        CdpConfig {
            policy: BrowserPolicy {
                // `localhost` and `127.0.0.1` reach the same fixture listener, so
                // this is a real first-party/third-party split on one server.
                allowed_domains: ["127.0.0.1".into()].into(),
                blocked_domains: ["localhost".into()].into(),
                allow_third_party_subresources: false,
            },
            profile: ProfileKind::Ephemeral,
            profile_root: root.join("profiles"),
            quarantine_dir: root.join("quarantine"),
            managed_root: root.join("managed"),
            headless: true,
            sandbox: std::env::var(NO_SANDBOX_ENV).as_deref() != Ok("1"),
            executable: Some(self.executable.clone()),
            allow_managed_download: false,
            extra_arguments: Vec::new(),
        }
    }

    async fn open(&self, task: &str) -> CdpBrowser {
        let mut browser = CdpBrowser::new(self.config());
        browser
            .open_isolated_profile(task)
            .await
            .expect("launch Chromium over a devtools pipe");
        browser
    }
}

fn node<'a>(
    snapshot: &'a PageSnapshot,
    what: &str,
    predicate: impl Fn(&SnapshotNode) -> bool,
) -> &'a SnapshotNode {
    snapshot
        .nodes
        .iter()
        .find(|node| predicate(node))
        .unwrap_or_else(|| {
            panic!(
                "no {what} in snapshot; nodes were {:?}",
                snapshot
                    .nodes
                    .iter()
                    .map(|node| (node.role.as_str(), node.name.as_str()))
                    .collect::<Vec<_>>()
            )
        })
}

fn named<'a>(snapshot: &'a PageSnapshot, name: &str) -> &'a SnapshotNode {
    node(snapshot, &format!("node named {name:?}"), |node| {
        node.name == name
    })
}

#[tokio::test]
async fn cdp_engine_reads_a_fixture_page_through_the_accessibility_tree() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let mut browser = harness.open("task-read").await;
    let snapshot = browser
        .navigate(&harness.server.first_party("/form.html"))
        .await
        .expect("navigate to the fixture");

    assert_eq!(snapshot.title, "Personal Agent browser fixture");
    assert!(snapshot.text.contains("Fixture form"), "{}", snapshot.text);
    assert_eq!(snapshot.handles.len(), snapshot.nodes.len());

    let submit = named(&snapshot, "Submit profile");
    assert_eq!(submit.role, "button");
    assert!(submit.bounds.is_some(), "the button has no layout box");

    let plan = named(&snapshot, "Plan");
    assert_eq!(plan.options, vec!["Free", "Pro", "Team"]);

    let name_field = named(&snapshot, "Full name");
    assert!(name_field.editable, "{name_field:?}");

    let profile_dir = browser.profile_dir().expect("profile dir").to_path_buf();
    assert!(profile_dir.starts_with(harness.workspace.path()));
    browser.close().await.expect("close");
}

#[tokio::test]
async fn cdp_engine_refuses_blocked_domains_and_leaves_content_free_receipts() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let mut browser = harness.open("task-policy").await;

    let blocked = browser
        .navigate(&harness.server.url("localhost", "/form.html"))
        .await
        .expect_err("a blocked domain must be refused before the browser is asked");
    assert!(
        matches!(blocked, BrowserError::DomainBlocked(_)),
        "{blocked:?}"
    );

    browser
        .navigate(&harness.server.first_party("/form.html"))
        .await
        .expect("navigate to the allowed origin");

    let receipts = browser.receipts();
    assert!(!receipts.is_empty(), "no egress receipts were recorded");
    assert!(
        receipts.iter().any(|receipt| {
            receipt.host == "127.0.0.1" && receipt.decision == EgressDecision::Allowed
        }),
        "{receipts:?}"
    );
    let refused = receipts
        .iter()
        .find(|receipt| receipt.host == "localhost")
        .expect("the third-party script must produce a receipt");
    assert_eq!(refused.decision, EgressDecision::Blocked);
    assert!(refused.reason.is_some(), "{refused:?}");

    let serialized = serde_json::to_string(&receipts).expect("serialize receipts");
    assert!(!serialized.contains("tracker.js"), "receipts leaked a path");
    assert!(
        !serialized.contains("third-party tracker"),
        "receipts leaked response content"
    );
    browser.close().await.expect("close");
}

#[tokio::test]
async fn ephemeral_profiles_do_not_share_cookies_between_tasks() {
    let Some(harness) = Harness::start().await else {
        return;
    };

    let mut task_a = harness.open("task-a").await;
    task_a
        .navigate(&harness.server.first_party("/set-cookie"))
        .await
        .expect("set the probe cookie");
    let cookies_a = task_a.cookie_names().await.expect("cookies in task A");
    assert!(
        cookies_a.iter().any(|name| name == ISOLATION_COOKIE),
        "task A never stored the probe cookie: {cookies_a:?}"
    );
    let profile_a = task_a.profile_dir().expect("profile A").to_path_buf();
    task_a.close().await.expect("close task A");

    let mut task_b = harness.open("task-b").await;
    let profile_b = task_b.profile_dir().expect("profile B").to_path_buf();
    assert_ne!(profile_a, profile_b, "two tasks shared a user data dir");
    task_b
        .navigate(&harness.server.first_party("/form.html"))
        .await
        .expect("navigate task B");
    let cookies_b = task_b.cookie_names().await.expect("cookies in task B");
    assert!(
        !cookies_b.iter().any(|name| name == ISOLATION_COOKIE),
        "task B inherited task A's cookie: {cookies_b:?}"
    );
    task_b.close().await.expect("close task B");
    assert!(
        !profile_a.exists(),
        "the ephemeral profile outlived the task"
    );
}

#[tokio::test]
async fn form_fill_select_and_submit_round_trip_through_the_page() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let mut browser = harness.open("task-form").await;
    let snapshot = browser
        .navigate(&harness.server.first_party("/form.html"))
        .await
        .expect("navigate to the fixture");

    let name_handle = named(&snapshot, "Full name").handle.clone();
    let snapshot = browser
        .type_into(&name_handle, "Ada Lovelace")
        .await
        .expect("type into the name field");

    let plan_handle = named(&snapshot, "Plan").handle.clone();
    let snapshot = browser
        .select_option(&plan_handle, "Pro")
        .await
        .expect("choose the Pro plan");

    let submit_handle = named(&snapshot, "Submit profile").handle.clone();
    let result = browser
        .click_node(&submit_handle)
        .await
        .expect("submit the form");

    assert!(
        result.text.contains("full_name=Ada Lovelace"),
        "server saw {:?}",
        result.text
    );
    assert!(
        result.text.contains("plan=pro"),
        "server saw {:?}",
        result.text
    );
    browser.close().await.expect("close");
}

#[tokio::test]
async fn uploads_require_an_approved_path_and_reach_the_server() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let payload = b"uploaded fixture bytes";
    let upload = harness.workspace.path().join("fixture-upload.txt");
    std::fs::write(&upload, payload).expect("write the upload fixture");

    let mut browser = harness.open("task-upload").await;
    let snapshot = browser
        .navigate(&harness.server.first_party("/form.html"))
        .await
        .expect("navigate to the fixture");
    let attachment = named(&snapshot, "Attachment").handle.clone();

    let refused = browser
        .upload_files(&attachment, std::slice::from_ref(&upload))
        .await
        .expect_err("an unapproved path must never reach the page");
    assert!(
        matches!(refused, BrowserError::UploadPathNotApproved(_)),
        "{refused:?}"
    );

    browser.approve_upload(&upload).expect("approve the upload");
    let snapshot = browser
        .upload_files(&attachment, &[upload])
        .await
        .expect("attach the approved file");

    let submit = named(&snapshot, "Submit profile").handle.clone();
    let result = browser.click_node(&submit).await.expect("submit the form");
    assert!(
        result.text.contains("attachment=fixture-upload.txt"),
        "server saw {:?}",
        result.text
    );
    assert!(
        result
            .text
            .contains(&format!("attachment_bytes={}", payload.len())),
        "server saw {:?}",
        result.text
    );
    browser.close().await.expect("close");
}

#[tokio::test]
async fn downloads_land_in_quarantine_and_cannot_be_released_unscanned() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let mut browser = harness.open("task-download").await;
    let snapshot = browser
        .navigate(&harness.server.first_party("/form.html"))
        .await
        .expect("navigate to the fixture");
    let link = named(&snapshot, "Download the report").handle.clone();
    browser.click_node(&link).await.expect("start the download");

    let mut downloads = browser.downloads();
    for _ in 0..100 {
        if !downloads.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        downloads = browser.downloads();
    }
    let mut download = downloads
        .first()
        .cloned()
        .expect("the download never reached quarantine");

    assert_eq!(download.state, DownloadState::Quarantined);
    let quarantine = harness.workspace.path().join("quarantine");
    assert!(
        PathBuf::from(&download.quarantine_path).starts_with(&quarantine),
        "download escaped quarantine: {}",
        download.quarantine_path
    );
    let expected = Sha256::digest(DOWNLOAD_BODY);
    assert_eq!(download.sha256, crate::cdp::launch::hex(&expected));
    assert_eq!(
        download.release(),
        Err(BrowserError::DownloadNotScanned),
        "quarantined bytes must not be releasable before a scan"
    );
    browser.close().await.expect("close");
}

#[tokio::test]
async fn handles_are_rejected_after_the_page_navigates() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let mut browser = harness.open("task-stale").await;
    let first = browser
        .navigate(&harness.server.first_party("/form.html"))
        .await
        .expect("navigate to the fixture");
    let stale = named(&first, "Submit profile").handle.clone();

    let second = browser
        .navigate(&harness.server.first_party("/second.html"))
        .await
        .expect("navigate to the second page");
    assert!(second.generation > first.generation);

    let error = browser
        .click_node(&stale)
        .await
        .expect_err("a handle from the previous page must not be replayable");
    assert!(matches!(error, BrowserError::StaleHandle), "{error:?}");

    let fresh = named(&second, "Nothing happens").handle.clone();
    browser
        .click_node(&fresh)
        .await
        .expect("fresh handle works");
    browser.close().await.expect("close");
}

#[tokio::test]
async fn screenshots_and_pdfs_are_produced_without_evaluating_page_script() {
    let Some(harness) = Harness::start().await else {
        return;
    };
    let mut browser = harness.open("task-capture").await;
    browser
        .navigate(&harness.server.first_party("/form.html"))
        .await
        .expect("navigate to the fixture");

    let png = browser.screenshot().await.expect("screenshot");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    let pdf = browser.print_pdf().await.expect("pdf");
    assert_eq!(&pdf[..5], b"%PDF-", "not a PDF");

    let tabs = browser.tabs().await.expect("tabs");
    assert_eq!(tabs.iter().filter(|tab| tab.active).count(), 1);
    browser.close().await.expect("close");
}
