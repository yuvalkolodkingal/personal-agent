//! Chromium discovery, pinned managed download, and pipe-attached process launch.

use crate::BrowserError;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};

/// Digests and revisions for the managed fallback build.
const PINS: &str = include_str!("../../chromium-pins.json");

/// Environment override that points at an already-trusted browser binary.
pub const CHROME_PATH_ENV: &str = "PERSONAL_AGENT_CHROME";

/// Where a usable Chromium came from, so operators can tell a system browser
/// from the hash-verified managed download.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromiumSource {
    /// Explicitly configured or supplied through [`CHROME_PATH_ENV`].
    Configured,
    /// Discovered on `PATH` or at a well-known install location.
    System,
    /// Downloaded by this crate and verified against a pinned SHA-256.
    Managed,
}

/// A Chromium-family executable that the engine is allowed to launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChromiumBinary {
    pub path: PathBuf,
    pub source: ChromiumSource,
}

#[derive(Debug, Deserialize)]
struct PinFile {
    base_url: String,
    platforms: std::collections::BTreeMap<String, PinnedBuild>,
}

#[derive(Debug, Deserialize)]
struct PinnedBuild {
    snapshot_platform: String,
    revision: u64,
    archive: String,
    binary: String,
    sha256: String,
}

#[cfg(target_os = "windows")]
const SYSTEM_EXECUTABLES: &[&str] = &["chrome.exe", "msedge.exe"];
#[cfg(not(target_os = "windows"))]
const SYSTEM_EXECUTABLES: &[&str] = &[
    "google-chrome-stable",
    "google-chrome",
    "chromium",
    "chromium-browser",
    "microsoft-edge-stable",
    "microsoft-edge",
];

#[cfg(target_os = "macos")]
const WELL_KNOWN_PATHS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];
#[cfg(target_os = "windows")]
const WELL_KNOWN_PATHS: &[&str] = &[
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
];
#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
const WELL_KNOWN_PATHS: &[&str] = &[
    "/usr/bin/google-chrome-stable",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/snap/bin/chromium",
    "/usr/bin/microsoft-edge-stable",
];

/// Locate a system Chrome, Chromium, or Edge without touching the network.
#[must_use]
pub fn discover_system_chromium(configured: Option<&Path>) -> Option<ChromiumBinary> {
    if let Some(path) = configured.filter(|path| path.is_file()) {
        return Some(ChromiumBinary {
            path: path.to_path_buf(),
            source: ChromiumSource::Configured,
        });
    }
    if let Some(path) = std::env::var_os(CHROME_PATH_ENV).map(PathBuf::from)
        && path.is_file()
    {
        return Some(ChromiumBinary {
            path,
            source: ChromiumSource::Configured,
        });
    }
    for name in SYSTEM_EXECUTABLES {
        if let Some(path) = search_path(name) {
            return Some(ChromiumBinary {
                path,
                source: ChromiumSource::System,
            });
        }
    }
    WELL_KNOWN_PATHS
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .map(|path| ChromiumBinary {
            path,
            source: ChromiumSource::System,
        })
}

fn search_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn pins() -> Result<PinFile, BrowserError> {
    serde_json::from_str(PINS)
        .map_err(|error| BrowserError::Operation(format!("chromium-pins.json is invalid: {error}")))
}

/// The remediation a human must follow when no browser can be provisioned.
#[must_use]
pub fn no_browser_remediation() -> String {
    format!(
        "install Google Chrome, Chromium, or Microsoft Edge, or point {CHROME_PATH_ENV} at a \
         Chromium-family executable, or enable the managed download for target {}",
        current_target()
    )
}

fn current_target() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unsupported"
    }
}

/// Download, hash-verify, and unpack the pinned Chromium for this host.
///
/// Mirrors the pinned sidecar fetcher: the digest is compared before anything is
/// unpacked, and a mismatch aborts instead of falling back to whatever bytes
/// arrived.
///
/// # Errors
///
/// Returns [`BrowserError::Unavailable`] when the host has no pinned build and
/// [`BrowserError::Operation`] when the download, digest check, or extraction
/// fails.
pub async fn fetch_managed_chromium(root: &Path) -> Result<ChromiumBinary, BrowserError> {
    let pins = pins()?;
    let target = current_target();
    let build = pins.platforms.get(target).ok_or_else(|| {
        BrowserError::Unavailable(format!(
            "no pinned Chromium is published for {target}; {}",
            no_browser_remediation()
        ))
    })?;
    let install = root.join(format!(
        "chromium-{}-{}",
        build.snapshot_platform, build.revision
    ));
    let binary = install.join(&build.binary);
    if binary.is_file() {
        return Ok(ChromiumBinary {
            path: binary,
            source: ChromiumSource::Managed,
        });
    }
    let url = format!(
        "{}/{}/{}/{}",
        pins.base_url.trim_end_matches('/'),
        build.snapshot_platform,
        build.revision,
        build.archive
    );
    let archive = download_verified(&url, &build.sha256).await?;
    unpack(archive, &install)?;
    if !binary.is_file() {
        return Err(BrowserError::Operation(format!(
            "pinned Chromium archive did not contain {}",
            build.binary
        )));
    }
    Ok(ChromiumBinary {
        path: binary,
        source: ChromiumSource::Managed,
    })
}

async fn download_verified(url: &str, expected: &str) -> Result<Vec<u8>, BrowserError> {
    let response = reqwest::get(url)
        .await
        .map_err(|error| BrowserError::Unavailable(format!("cannot download {url}: {error}")))?;
    if !response.status().is_success() {
        return Err(BrowserError::Unavailable(format!(
            "{url} returned HTTP {}",
            response.status()
        )));
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| BrowserError::Unavailable(format!("cannot read {url}: {error}")))?;
    let digest = hex(&Sha256::digest(&body));
    if digest != expected {
        return Err(BrowserError::Operation(format!(
            "pinned Chromium digest mismatch for {url}: expected {expected}, got {digest}"
        )));
    }
    Ok(body.to_vec())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn unpack(archive: Vec<u8>, install: &Path) -> Result<(), BrowserError> {
    let staging = install.with_extension("partial");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|error| {
        BrowserError::Operation(format!("cannot create {}: {error}", staging.display()))
    })?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|error| BrowserError::Operation(format!("invalid Chromium archive: {error}")))?;
    zip.extract(&staging)
        .map_err(|error| BrowserError::Operation(format!("cannot unpack Chromium: {error}")))?;
    if let Some(parent) = install.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            BrowserError::Operation(format!("cannot create {}: {error}", parent.display()))
        })?;
    }
    let _ = std::fs::remove_dir_all(install);
    std::fs::rename(&staging, install)
        .map_err(|error| BrowserError::Operation(format!("cannot install Chromium: {error}")))
}

/// Command-line flags shared by every managed launch.
fn base_arguments(profile_dir: &Path, headless: bool, sandbox: bool) -> Vec<String> {
    let mut arguments = vec![
        format!("--user-data-dir={}", profile_dir.display()),
        // The automation channel is a pair of inherited file descriptors, never a
        // TCP port, so no other local process can attach to this browser.
        "--remote-debugging-pipe".to_owned(),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--disable-background-networking".to_owned(),
        "--disable-component-update".to_owned(),
        "--disable-client-side-phishing-detection".to_owned(),
        "--disable-sync".to_owned(),
        "--metrics-recording-only".to_owned(),
        "--no-service-autorun".to_owned(),
        "--password-store=basic".to_owned(),
        "--use-mock-keychain".to_owned(),
        "--disable-features=Translate,MediaRouter,OptimizationHints,AcceptCHFrame".to_owned(),
    ];
    if headless {
        arguments.push("--headless=new".to_owned());
        arguments.push("--disable-gpu".to_owned());
    }
    if !sandbox {
        arguments.push("--no-sandbox".to_owned());
    }
    arguments.push("about:blank".to_owned());
    arguments
}

/// A launched browser process together with its devtools pipe endpoints.
pub(crate) struct LaunchedBrowser {
    pub child: Child,
    pub reader: PipeReader,
    pub writer: PipeWriter,
}

#[cfg(unix)]
pub(crate) type PipeReader = tokio::net::unix::pipe::Receiver;
#[cfg(unix)]
pub(crate) type PipeWriter = tokio::net::unix::pipe::Sender;
#[cfg(not(unix))]
pub(crate) type PipeReader = tokio::io::Empty;
#[cfg(not(unix))]
pub(crate) type PipeWriter = tokio::io::Sink;

/// Spawn Chromium with the devtools protocol bound to inherited pipes.
///
/// # Errors
///
/// Returns [`BrowserError::Unavailable`] on unsupported platforms and
/// [`BrowserError::Operation`] when the pipes or the process cannot be created.
#[cfg(unix)]
pub(crate) fn launch(
    executable: &Path,
    profile_dir: &Path,
    headless: bool,
    sandbox: bool,
    extra: &[String],
) -> Result<LaunchedBrowser, BrowserError> {
    use command_fds::{CommandFdExt, FdMapping};
    use std::os::fd::OwnedFd;

    let (browser_input, our_output) = std::io::pipe().map_err(|error| {
        BrowserError::Operation(format!("cannot create devtools pipe: {error}"))
    })?;
    let (our_input, browser_output) = std::io::pipe().map_err(|error| {
        BrowserError::Operation(format!("cannot create devtools pipe: {error}"))
    })?;

    let mut command = Command::new(executable);
    command
        .args(base_arguments(profile_dir, headless, sandbox))
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
        .fd_mappings(vec![
            FdMapping {
                parent_fd: OwnedFd::from(browser_input),
                child_fd: 3,
            },
            FdMapping {
                parent_fd: OwnedFd::from(browser_output),
                child_fd: 4,
            },
        ])
        .map_err(|error| BrowserError::Operation(format!("cannot map devtools pipe: {error}")))?;

    let child = command.spawn().map_err(|error| {
        BrowserError::Unavailable(format!("cannot start {}: {error}", executable.display()))
    })?;
    // Dropping the builder closes this process's copies of the child ends, so the
    // reader observes EOF as soon as the browser exits.
    drop(command);

    let reader = PipeReader::from_owned_fd(OwnedFd::from(our_input))
        .map_err(|error| BrowserError::Operation(format!("cannot open devtools pipe: {error}")))?;
    let writer = PipeWriter::from_owned_fd(OwnedFd::from(our_output))
        .map_err(|error| BrowserError::Operation(format!("cannot open devtools pipe: {error}")))?;
    Ok(LaunchedBrowser {
        child,
        reader,
        writer,
    })
}

/// Windows exposes the devtools pipe through inherited handles that this crate
/// does not yet plumb through; state the reason rather than silently opening a
/// TCP debugging port.
#[cfg(not(unix))]
pub(crate) fn launch(
    _executable: &Path,
    _profile_dir: &Path,
    _headless: bool,
    _sandbox: bool,
    _extra: &[String],
) -> Result<LaunchedBrowser, BrowserError> {
    Err(BrowserError::Unavailable(
        "the CDP engine needs --remote-debugging-pipe, which this build only implements on Unix; \
         use the WebDriver engine on this platform"
            .into(),
    ))
}

/// Whether a path looks like a Chromium-family executable name.
#[must_use]
pub fn is_chromium_executable(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name.contains("chrome") || name.contains("chromium") || name.contains("edge")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pinned_platform_has_a_full_sha256_and_a_snapshot_url() {
        let pins = pins().expect("chromium-pins.json parses");
        assert!(pins.base_url.starts_with("https://"), "{}", pins.base_url);
        assert!(!pins.platforms.is_empty());
        for (target, build) in &pins.platforms {
            assert_eq!(build.sha256.len(), 64, "{target} digest is not a sha256");
            assert!(
                build.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "{target} digest is not hex"
            );
            assert!(build.revision > 0, "{target} has no revision");
            assert!(
                build.archive.to_ascii_lowercase().ends_with(".zip"),
                "{target} archive"
            );
            assert!(
                is_chromium_executable(Path::new(&build.binary)),
                "{target} binary {} is not a Chromium executable",
                build.binary
            );
        }
    }

    /// Downloads roughly 220 MB, so it is opt-in. It is the only way to prove the
    /// pinned digest still matches what the snapshot bucket serves.
    #[tokio::test]
    async fn managed_download_verifies_the_pinned_digest_and_yields_a_runnable_binary() {
        const ENV: &str = "PERSONAL_AGENT_CDP_MANAGED_DOWNLOAD_TEST";
        if std::env::var(ENV).as_deref() != Ok("1") {
            eprintln!("set {ENV}=1 to download and hash-verify the pinned managed Chromium");
            return;
        }
        let cache = tempfile::tempdir().expect("cache dir");
        let binary = fetch_managed_chromium(cache.path())
            .await
            .expect("fetch the pinned Chromium");
        assert_eq!(binary.source, ChromiumSource::Managed);
        assert!(
            binary.path.is_file(),
            "{} is not a file",
            binary.path.display()
        );
        assert!(binary.path.starts_with(cache.path()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&binary.path)
                .expect("stat the unpacked browser")
                .permissions()
                .mode();
            assert!(
                mode & 0o111 != 0,
                "unpacked browser is not executable: {mode:o}"
            );
        }

        // A second call must reuse the unpacked tree rather than download again.
        let again = fetch_managed_chromium(cache.path())
            .await
            .expect("reuse the cached Chromium");
        assert_eq!(again.path, binary.path);
    }

    #[test]
    fn launch_arguments_use_a_pipe_and_a_dedicated_profile_and_never_a_debug_port() {
        let arguments = base_arguments(Path::new("/tmp/profile-a"), false, true);
        assert!(arguments.iter().any(|a| a == "--remote-debugging-pipe"));
        assert!(
            arguments
                .iter()
                .any(|a| a == "--user-data-dir=/tmp/profile-a")
        );
        assert!(
            !arguments
                .iter()
                .any(|a| a.starts_with("--remote-debugging-port")),
            "a TCP debugging port would reopen the fixed-port attack surface"
        );
        assert!(
            !arguments.iter().any(|a| a == "--headless=new"),
            "headful is the default"
        );
        assert!(
            !arguments.iter().any(|a| a == "--no-sandbox"),
            "the browser sandbox stays on unless explicitly disabled"
        );
    }

    #[test]
    fn remediation_names_the_override_variable() {
        assert!(no_browser_remediation().contains(CHROME_PATH_ENV));
    }

    #[test]
    fn hex_encodes_lowercase_fixed_width() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }
}
