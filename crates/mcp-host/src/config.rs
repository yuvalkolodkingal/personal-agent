//! Host policy: pinned working directory, environment allowlist, timeouts, and
//! reconnect backoff.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

/// Environment variables an stdio MCP server may inherit from this process.
///
/// The child environment is cleared before spawning, so anything absent from
/// this list (tokens, proxy credentials, `NODE_OPTIONS`, `LD_PRELOAD`, ...) can
/// only reach the server through an explicit, reviewed binding.
pub const DEFAULT_ENVIRONMENT_ALLOWLIST: &[&str] = &[
    "APPDATA",
    "COMSPEC",
    "HOME",
    "LANG",
    "LC_ALL",
    "LOCALAPPDATA",
    "PATH",
    "PATHEXT",
    "PROGRAMFILES",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "TMPDIR",
    "TZ",
    "USERPROFILE",
];

/// Exponential reconnect schedule shared by every transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackoffPolicy {
    /// Total connection attempts, including the first one.
    pub attempts: u32,
    /// Delay before the second attempt.
    pub initial: Duration,
    /// Upper bound for any single delay.
    pub maximum: Duration,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            initial: Duration::from_millis(250),
            maximum: Duration::from_secs(8),
        }
    }
}

impl BackoffPolicy {
    /// Delay to wait before `attempt` (1-based); attempt 1 never waits.
    #[must_use]
    pub fn delay(&self, attempt: u32) -> Duration {
        if attempt <= 1 {
            return Duration::ZERO;
        }
        let factor = 1_u32 << (attempt - 2).min(16);
        self.initial.saturating_mul(factor).min(self.maximum)
    }
}

/// Native MCP host policy.
#[derive(Clone, Debug)]
pub struct HostConfig {
    /// Directory stdio servers run in when a definition pins no other path.
    /// The process working directory is never inherited.
    pub working_directory: PathBuf,
    /// Parent environment variables an stdio server may inherit.
    pub environment_allowlist: BTreeSet<String>,
    /// Budget for transport setup plus MCP initialization.
    pub connect_timeout: Duration,
    /// Budget for a single request once a session exists.
    pub request_timeout: Duration,
    /// Reconnect schedule.
    pub backoff: BackoffPolicy,
    /// `clientInfo.name` sent during initialization.
    pub client_name: String,
    /// `clientInfo.version` sent during initialization.
    pub client_version: String,
}

impl HostConfig {
    /// Builds a policy that pins stdio servers to `working_directory`.
    #[must_use]
    pub fn new(working_directory: impl Into<PathBuf>) -> Self {
        Self {
            working_directory: working_directory.into(),
            environment_allowlist: DEFAULT_ENVIRONMENT_ALLOWLIST
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            connect_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(60),
            backoff: BackoffPolicy::default(),
            client_name: "Personal Agent".to_owned(),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Returns the allowlisted parent environment for a child server.
    #[must_use]
    pub fn inherited_environment(&self) -> Vec<(String, String)> {
        self.environment_allowlist
            .iter()
            .filter_map(|name| std::env::var(name).ok().map(|value| (name.clone(), value)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{BackoffPolicy, DEFAULT_ENVIRONMENT_ALLOWLIST, HostConfig};
    use std::time::Duration;

    #[test]
    fn backoff_grows_then_saturates() {
        let policy = BackoffPolicy {
            attempts: 6,
            initial: Duration::from_millis(100),
            maximum: Duration::from_millis(400),
        };
        assert_eq!(policy.delay(1), Duration::ZERO);
        assert_eq!(policy.delay(2), Duration::from_millis(100));
        assert_eq!(policy.delay(3), Duration::from_millis(200));
        assert_eq!(policy.delay(4), Duration::from_millis(400));
        assert_eq!(policy.delay(9), Duration::from_millis(400));
    }

    #[test]
    fn allowlist_excludes_credential_carrying_variables() {
        let config = HostConfig::new("/tmp");
        for denied in ["AWS_SECRET_ACCESS_KEY", "GITHUB_TOKEN", "LD_PRELOAD"] {
            assert!(!config.environment_allowlist.contains(denied));
        }
        assert!(config.environment_allowlist.contains("PATH"));
        assert_eq!(
            config.environment_allowlist.len(),
            DEFAULT_ENVIRONMENT_ALLOWLIST.len()
        );
    }

    #[test]
    fn inherited_environment_only_reports_allowlisted_names() {
        let config = HostConfig::new("/tmp");
        for (name, _) in config.inherited_environment() {
            assert!(config.environment_allowlist.contains(&name));
        }
    }
}
