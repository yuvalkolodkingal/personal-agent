//! Read-only legacy discovery and idempotent migration planning.

use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// One legacy input found without opening or changing personal content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredInput {
    pub kind: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub contains_possible_secrets: bool,
}

/// Dry-run report that must precede any personal-data copy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationPlan {
    pub source_root: PathBuf,
    pub inputs: Vec<DiscoveredInput>,
    pub requires_confirmation: bool,
    pub remote_devices_require_repairing: bool,
    pub plaintext_secrets_will_be_skipped: bool,
}

/// Discovery failure.
#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("legacy source is not a directory: {0}")]
    InvalidSource(PathBuf),
    #[error("cannot inspect legacy input: {0}")]
    Io(#[from] std::io::Error),
}

/// Discover only documented legacy inputs. Symlink targets are not followed.
///
/// # Errors
///
/// Returns an error when the source is absent/not a directory or metadata
/// cannot be inspected.
pub fn discover(source_root: &Path) -> Result<MigrationPlan, MigrationError> {
    if !source_root.is_dir() {
        return Err(MigrationError::InvalidSource(source_root.to_path_buf()));
    }
    let candidates = [
        ("config", "config.toml", true),
        ("history", "history.jsonl", false),
        ("memory", "memory", false),
        ("traces", "traces", true),
        ("skills", "skills", false),
        ("experts", "experts", false),
        ("projects", "projects.json", false),
        ("themes", "themes", false),
        ("schedules", "schedule.toml", false),
        ("mcp", "mcp.json", true),
        ("remote-devices", "devices.json", true),
    ];
    let mut inputs = Vec::new();
    for (kind, relative, secrets) in candidates {
        let path = source_root.join(relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        inputs.push(DiscoveredInput {
            kind: kind.into(),
            path,
            bytes: metadata.len(),
            contains_possible_secrets: secrets,
        });
    }
    Ok(MigrationPlan {
        source_root: source_root.to_path_buf(),
        inputs,
        requires_confirmation: true,
        remote_devices_require_repairing: true,
        plaintext_secrets_will_be_skipped: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_is_read_only_and_marks_secret_sources() {
        let temp = tempfile::tempdir().expect("temp");
        fs::write(temp.path().join("config.toml"), "token='secret'").expect("fixture");
        let before = fs::read(temp.path().join("config.toml")).unwrap();
        let plan = discover(temp.path()).expect("plan");
        let after = fs::read(temp.path().join("config.toml")).unwrap();
        assert_eq!(before, after);
        assert!(plan.requires_confirmation);
        assert!(plan.inputs[0].contains_possible_secrets);
    }
}
