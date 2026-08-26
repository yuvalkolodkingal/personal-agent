//! Shared, side-effect-bounded command-line interface.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Fully rendered CLI result so compatibility shims can share exact behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CliResponse {
    fn success(stdout: String) -> Self {
        Self {
            exit_code: 0,
            stdout,
            stderr: String::new(),
        }
    }

    fn failure(exit_code: i32, stderr: impl Into<String>) -> Self {
        Self {
            exit_code,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }
}

const USAGE: &str = "usage: personal-agent <status|doctor|config print-default|config check PATH|migration dry-run CONFIG_ROOT DATA_ROOT [OPENCODE_AUTH]>";

/// Execute the stable native CLI surface without opening the database or runtime.
///
/// `config check` validates the named existing file and never changes it.
#[must_use]
pub fn run_cli<I, S>(arguments: I) -> CliResponse
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let arguments = arguments
        .into_iter()
        .map(Into::into)
        .collect::<Vec<OsString>>();
    let command = arguments.first().and_then(|value| value.to_str());
    match command.unwrap_or("status") {
        "status" | "doctor" if arguments.len() == 1 || arguments.is_empty() => {
            match serde_json::to_string_pretty(&crate::diagnostic_snapshot()) {
                Ok(output) => CliResponse::success(format!("{output}\n")),
                Err(error) => CliResponse::failure(1, format!("diagnostics failed: {error}\n")),
            }
        }
        "config" => config_command(&arguments[1..]),
        "migration" => migration_command(&arguments[1..]),
        "--help" | "-h" | "help" => CliResponse::success(format!("{USAGE}\n")),
        "--version" | "-V" => {
            CliResponse::success(format!("personal-agent {}\n", env!("CARGO_PKG_VERSION")))
        }
        _ => CliResponse::failure(2, format!("{USAGE}\n")),
    }
}

fn migration_command(arguments: &[OsString]) -> CliResponse {
    let [command, config_root, data_root, tail @ ..] = arguments else {
        return CliResponse::failure(2, format!("{USAGE}\n"));
    };
    if command != "dry-run" || tail.len() > 1 {
        return CliResponse::failure(2, format!("{USAGE}\n"));
    }
    let roots = personal_agent_migration::LegacyRoots {
        config_root: PathBuf::from(config_root),
        data_root: PathBuf::from(data_root),
        opencode_auth: tail.first().map(PathBuf::from),
    };
    match personal_agent_migration::discover_profile(&roots) {
        Ok(plan) => match plan.to_json_pretty() {
            Ok(output) => CliResponse::success(format!("{output}\n")),
            Err(error) => CliResponse::failure(1, format!("dry run failed: {error}\n")),
        },
        Err(error) => CliResponse::failure(1, format!("dry run failed: {error}\n")),
    }
}

fn config_command(arguments: &[OsString]) -> CliResponse {
    match arguments {
        [command] if command == "print-default" => match crate::default_config_toml() {
            Ok(output) => CliResponse::success(output),
            Err(error) => CliResponse::failure(1, format!("default config failed: {error}\n")),
        },
        [command, path] if command == "check" => match std::fs::read_to_string(Path::new(path)) {
            Ok(input) => match crate::parse_config(&input) {
                Ok(load) => CliResponse::success(format!(
                    "valid configuration ({} safe defaults materialized in memory)\n",
                    load.repaired_fields.len()
                )),
                Err(error) => CliResponse::failure(1, format!("invalid configuration: {error}\n")),
            },
            Err(error) => {
                CliResponse::failure(1, format!("configuration cannot be read: {error}\n"))
            }
        },
        _ => CliResponse::failure(2, format!("{USAGE}\n")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_check_is_read_only_and_strict() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "schema_version = 1\n[persona]\nname = 'JARVIS'\n").expect("fixture");
        let before = std::fs::read(&path).expect("before");
        let response = run_cli([
            OsString::from("config"),
            OsString::from("check"),
            path.into(),
        ]);
        assert_eq!(response.exit_code, 0);
        assert_eq!(
            before,
            std::fs::read(temp.path().join("config.toml")).expect("after")
        );
    }

    #[test]
    fn unknown_commands_fail_with_usage() {
        let response = run_cli(["execute-spoken-code"]);
        assert_eq!(response.exit_code, 2);
        assert!(response.stderr.starts_with("usage:"));
    }

    #[test]
    fn migration_dry_run_is_read_only_and_machine_readable() {
        let temp = tempfile::tempdir().expect("temp");
        let config = temp.path().join("config");
        let data = temp.path().join("data");
        std::fs::create_dir_all(&config).expect("config root");
        std::fs::create_dir_all(data.join("memory")).expect("data root");
        let legacy = config.join("config.toml");
        std::fs::write(&legacy, "[persona]\nname = 'JARVIS'\n").expect("legacy config");
        std::fs::write(data.join("memory/2026-08-26.md"), "synthetic memory")
            .expect("legacy memory");
        let before = std::fs::read(&legacy).expect("before");

        let response = run_cli([
            OsString::from("migration"),
            OsString::from("dry-run"),
            config.into(),
            data.into(),
        ]);

        assert_eq!(response.exit_code, 0, "{}", response.stderr);
        let plan: personal_agent_migration::MigrationPlan =
            serde_json::from_str(&response.stdout).expect("plan JSON");
        assert!(plan.requires_confirmation);
        assert_eq!(before, std::fs::read(&legacy).expect("after"));
    }
}
