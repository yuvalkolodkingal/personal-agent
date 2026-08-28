//! Cross-platform, policy-bound local process and Docker execution.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;

/// Process I/O mode. PTY requests are explicit so frontends can report whether
/// the installed platform adapter is supported, degraded, or unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessMode {
    Captured,
    InteractivePty,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityLevel {
    Supported,
    Degraded,
    Unavailable,
}

/// A process request uses argv fields, never an implicit command string.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    pub mode: ProcessMode,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
    pub network_requested: bool,
}

/// Workspace and process guardrails configured by the user or organization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // Independent security switches are clearer than a lossy state enum.
pub struct ExecutionPolicy {
    pub workspace_roots: Vec<PathBuf>,
    pub allowed_programs: BTreeSet<String>,
    pub allowed_environment: BTreeSet<String>,
    pub allow_network: bool,
    pub allow_interactive_shell: bool,
    pub allow_docker: bool,
    pub require_approval_for_destructive: bool,
}

impl ExecutionPolicy {
    /// Validate a process before anything is spawned.
    ///
    /// # Errors
    ///
    /// Returns an [`ExecutionError`] when a workspace, program, environment,
    /// network, interaction, or approval boundary is denied.
    pub fn validate(&self, spec: &CommandSpec, approved: bool) -> Result<PathBuf, ExecutionError> {
        if spec.program.trim().is_empty() {
            return Err(ExecutionError::Invalid("program cannot be blank".into()));
        }
        if !self.allowed_programs.is_empty() && !self.allowed_programs.contains(&spec.program) {
            return Err(ExecutionError::ProgramDenied(spec.program.clone()));
        }
        if spec.mode == ProcessMode::InteractivePty && !self.allow_interactive_shell {
            return Err(ExecutionError::InteractiveDenied);
        }
        if spec.network_requested && !self.allow_network {
            return Err(ExecutionError::NetworkDenied);
        }
        let disallowed_environment = spec
            .environment
            .keys()
            .find(|key| !self.allowed_environment.contains(*key));
        if let Some(key) = disallowed_environment {
            return Err(ExecutionError::EnvironmentDenied(key.clone()));
        }
        let cwd = spec
            .cwd
            .canonicalize()
            .map_err(|error| ExecutionError::Invalid(format!("invalid cwd: {error}")))?;
        let in_workspace = self.workspace_roots.iter().any(|root| {
            root.canonicalize()
                .is_ok_and(|canonical_root| cwd.starts_with(canonical_root))
        });
        if !in_workspace {
            return Err(ExecutionError::OutsideWorkspace(cwd));
        }
        if self.require_approval_for_destructive && is_destructive(spec) && !approved {
            return Err(ExecutionError::ApprovalRequired);
        }
        Ok(cwd)
    }
}

/// Bounded process result suitable for audit logs and postcondition checks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub operation_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub timed_out: bool,
    pub pty: CapabilityLevel,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ExecutionError {
    #[error("program is not allowed: {0}")]
    ProgramDenied(String),
    #[error("environment variable is not allowed: {0}")]
    EnvironmentDenied(String),
    #[error("working directory is outside an approved workspace: {0}")]
    OutsideWorkspace(PathBuf),
    #[error("network access is not allowed")]
    NetworkDenied,
    #[error("interactive shell or PTY access is not allowed")]
    InteractiveDenied,
    #[error("destructive command requires approval")]
    ApprovalRequired,
    #[error("Docker execution is not enabled")]
    DockerDenied,
    #[error("execution request is invalid: {0}")]
    Invalid(String),
    #[error("process failed: {0}")]
    Process(String),
}

/// Local executor with deterministic validation, bounded output, and timeouts.
#[derive(Clone, Debug)]
pub struct LocalExecutor {
    pub policy: ExecutionPolicy,
}

impl LocalExecutor {
    /// Execute one program. `approved` is a signed/recorded policy decision from
    /// the tool gateway rather than a prompt generated by the process itself.
    ///
    /// # Errors
    ///
    /// Returns an [`ExecutionError`] when validation, process startup, I/O, or
    /// timeout handling fails.
    pub async fn run(
        &self,
        spec: CommandSpec,
        approved: bool,
    ) -> Result<ExecutionResult, ExecutionError> {
        let cwd = self.policy.validate(&spec, approved)?;
        let operation_id = Uuid::now_v7();
        let started_at = Utc::now();
        let mut environment = self
            .policy
            .allowed_environment
            .iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.clone(), value)))
            .collect::<BTreeMap<_, _>>();
        environment.extend(spec.environment.clone());
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(cwd)
            .env_clear()
            .envs(environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| ExecutionError::Process(error.to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ExecutionError::Process("stdout pipe unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ExecutionError::Process("stderr pipe unavailable".into()))?;
        let stdout_task = tokio::spawn(read_bounded(stdout, spec.max_output_bytes));
        let stderr_task = tokio::spawn(read_bounded(stderr, spec.max_output_bytes));
        let wait = timeout(Duration::from_millis(spec.timeout_ms.max(1)), child.wait()).await;
        let (status, timed_out) = if let Ok(status) = wait {
            (
                Some(status.map_err(|error| ExecutionError::Process(error.to_string()))?),
                false,
            )
        } else {
            child
                .kill()
                .await
                .map_err(|error| ExecutionError::Process(error.to_string()))?;
            let _ = child.wait().await;
            (None, true)
        };
        let (stdout, stdout_truncated) = stdout_task
            .await
            .map_err(|error| ExecutionError::Process(error.to_string()))?
            .map_err(|error| ExecutionError::Process(error.to_string()))?;
        let (stderr, stderr_truncated) = stderr_task
            .await
            .map_err(|error| ExecutionError::Process(error.to_string()))?
            .map_err(|error| ExecutionError::Process(error.to_string()))?;
        Ok(ExecutionResult {
            operation_id,
            started_at,
            finished_at: Utc::now(),
            exit_code: status.and_then(|status| status.code()),
            stdout,
            stderr,
            truncated: stdout_truncated || stderr_truncated,
            timed_out,
            pty: if spec.mode == ProcessMode::InteractivePty {
                CapabilityLevel::Degraded
            } else {
                CapabilityLevel::Unavailable
            },
        })
    }

    /// Run an isolated container with safe defaults through the local Docker CLI.
    ///
    /// # Errors
    ///
    /// Returns an [`ExecutionError`] for invalid images or mounts, denied
    /// networking, missing approval, or a failed Docker process.
    pub async fn run_docker(
        &self,
        request: DockerRequest,
        approved: bool,
    ) -> Result<ExecutionResult, ExecutionError> {
        if !self.policy.allow_docker {
            return Err(ExecutionError::DockerDenied);
        }
        request.validate(&self.policy)?;
        let mut args = vec![
            "run".into(),
            "--rm".into(),
            "--read-only".into(),
            "--cap-drop=ALL".into(),
            "--security-opt=no-new-privileges".into(),
            "--pids-limit=256".into(),
        ];
        if !request.network_requested {
            args.push("--network=none".into());
        }
        for mount in &request.mounts {
            let mode = if mount.writable { "rw" } else { "ro" };
            args.push("--mount".into());
            args.push(format!(
                "type=bind,src={},dst={},{}",
                mount.host.display(),
                mount.container.display(),
                mode
            ));
        }
        args.push(request.image.clone());
        args.extend(request.command.clone());
        self.run(
            CommandSpec {
                program: "docker".into(),
                args,
                cwd: request.cwd,
                environment: BTreeMap::new(),
                mode: ProcessMode::Captured,
                timeout_ms: request.timeout_ms,
                max_output_bytes: request.max_output_bytes,
                network_requested: request.network_requested,
            },
            approved,
        )
        .await
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DockerMount {
    pub host: PathBuf,
    pub container: PathBuf,
    pub writable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DockerRequest {
    pub image: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub mounts: Vec<DockerMount>,
    pub network_requested: bool,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}

impl DockerRequest {
    fn validate(&self, policy: &ExecutionPolicy) -> Result<(), ExecutionError> {
        if self.image.trim().is_empty() || self.image.starts_with('-') {
            return Err(ExecutionError::Invalid("invalid Docker image".into()));
        }
        if self.network_requested && !policy.allow_network {
            return Err(ExecutionError::NetworkDenied);
        }
        for mount in &self.mounts {
            if !mount.container.is_absolute() {
                return Err(ExecutionError::Invalid(
                    "container mount path must be absolute".into(),
                ));
            }
            let host = mount
                .host
                .canonicalize()
                .map_err(|error| ExecutionError::Invalid(format!("invalid mount: {error}")))?;
            let permitted = policy
                .workspace_roots
                .iter()
                .any(|root| root.canonicalize().is_ok_and(|root| host.starts_with(root)));
            if !permitted {
                return Err(ExecutionError::OutsideWorkspace(host));
            }
        }
        Ok(())
    }
}

async fn read_bounded<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<(String, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
        if read > remaining {
            truncated = true;
        }
    }
    Ok((String::from_utf8_lossy(&output).into_owned(), truncated))
}

fn is_destructive(spec: &CommandSpec) -> bool {
    let program = Path::new(&spec.program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&spec.program)
        .to_ascii_lowercase();
    matches!(
        program.as_str(),
        "rm" | "rmdir" | "del" | "erase" | "format" | "mkfs" | "shutdown" | "reboot"
    ) || (program == "git"
        && spec
            .args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "reset" | "clean")))
        || (program == "docker"
            && spec
                .args
                .first()
                .is_some_and(|arg| matches!(arg.as_str(), "system" | "volume" | "rm")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(root: &Path) -> ExecutionPolicy {
        ExecutionPolicy {
            workspace_roots: vec![root.to_path_buf()],
            allowed_programs: BTreeSet::new(),
            allowed_environment: BTreeSet::new(),
            allow_network: false,
            allow_interactive_shell: false,
            allow_docker: false,
            require_approval_for_destructive: true,
        }
    }

    #[tokio::test]
    async fn runs_bounded_process_in_workspace() {
        let root = std::env::current_dir().expect("cwd");
        let executor = LocalExecutor {
            policy: policy(&root),
        };
        let result = executor
            .run(
                CommandSpec {
                    program: if cfg!(windows) {
                        "cmd".into()
                    } else {
                        "printf".into()
                    },
                    args: if cfg!(windows) {
                        vec!["/C".into(), "echo hello".into()]
                    } else {
                        vec!["hello".into()]
                    },
                    cwd: root,
                    environment: BTreeMap::new(),
                    mode: ProcessMode::Captured,
                    timeout_ms: 5_000,
                    max_output_bytes: 100,
                    network_requested: false,
                },
                false,
            )
            .await
            .expect("process");
        assert!(result.stdout.contains("hello"));
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn destructive_commands_require_external_approval() {
        let root = std::env::current_dir().expect("cwd");
        let spec = CommandSpec {
            program: "rm".into(),
            args: vec!["file".into()],
            cwd: root.clone(),
            environment: BTreeMap::new(),
            mode: ProcessMode::Captured,
            timeout_ms: 1_000,
            max_output_bytes: 100,
            network_requested: false,
        };
        assert_eq!(
            policy(&root).validate(&spec, false),
            Err(ExecutionError::ApprovalRequired)
        );
    }

    #[test]
    fn docker_mounts_cannot_escape_workspace() {
        let root = std::env::current_dir().expect("cwd");
        let request = DockerRequest {
            image: "alpine:3".into(),
            command: vec!["true".into()],
            cwd: root,
            mounts: vec![DockerMount {
                host: PathBuf::from("/"),
                container: PathBuf::from("/host"),
                writable: false,
            }],
            network_requested: false,
            timeout_ms: 1_000,
            max_output_bytes: 100,
        };
        assert!(matches!(
            request.validate(&policy(Path::new("/tmp"))),
            Err(ExecutionError::OutsideWorkspace(_))
        ));
    }
}
