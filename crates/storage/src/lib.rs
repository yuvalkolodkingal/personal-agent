//! SQLCipher-backed append-only event store and transactional schema migrations.

use personal_agent_contracts::proto::EventEnvelope;
use prost::Message;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

const BASELINE_SCHEMA_VERSION: i64 = 4;
const SCHEMA_VERSION: i64 = 5;
const PROJECTION_CHECKPOINT_KIND: &str = "projection.checkpoint";
const PROJECTION_CHECKPOINT_ID: &str = "projection.checkpoint";
const SUPERVISOR_SNAPSHOT_KIND: &str = "agent-supervisor";
const SUPERVISOR_RECENT_EVENT_LIMIT: usize = 100;

/// Storage failure with enough context for diagnostics and recovery.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("stored event is not valid protobuf: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("stored migration payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("SQLCipher support is unavailable in this build")]
    SqlCipherUnavailable,
    #[error("event schema version {0} is not supported")]
    UnsupportedEventSchema(u32),
    #[error("event sequence is outside SQLite's signed integer range: {0}")]
    SequenceOutOfRange(u64),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("export or backup destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("content-addressed blob does not exist: {0}")]
    BlobMissing(String),
    #[error(
        "database schema version {found} is newer than supported version {supported}; downgrade is refused"
    )]
    DowngradeRefused { found: i64, supported: i64 },
    #[error("database schema version {0} predates the supported v4 migration baseline")]
    UnsupportedDatabaseSchema(i64),
    #[error("a file-backed database is required to back up schema version {0} before migration")]
    MigrationBackupRequired(i64),
}

/// Owned encrypted store. Database access never crosses the native-core boundary.
pub struct EventStore {
    connection: Connection,
}

/// Durable application projection plus the last event included in it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ProjectionCheckpoint<T> {
    /// Serialized application-owned projection state.
    pub projection_snapshot_blob: T,
    /// Highest event sequence incorporated into the projection.
    pub last_sequence: u64,
}

/// Content-free goal activity retained with a supervisor recovery base.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SupervisorActivityCheckpoint {
    /// Event sequence used for stable global ordering.
    pub sequence: u64,
    /// Goal/task transition type.
    pub event_type: String,
    /// Goal identifier when the event exposes one.
    pub goal_id: Option<String>,
    /// Task identifier when the event exposes one.
    pub task_id: Option<String>,
    /// Original event timestamp.
    pub timestamp: String,
}

/// Existing supervisor snapshot enriched with its event replay boundary.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct SupervisorRecoveryCheckpoint {
    /// Complete task-supervisor state at the checkpoint.
    pub snapshot: personal_agent_agent::SupervisorSnapshot,
    /// Highest goal event represented by this checkpoint.
    pub last_sequence: u64,
    /// Latest event carrying the complete goal definition.
    pub latest_goal_event: Option<EventEnvelope>,
    /// Approval requests that were unresolved at the checkpoint.
    pub pending_approval_events: Vec<EventEnvelope>,
    /// Bounded activity summaries used to rebuild the recent timeline.
    pub recent_activities: Vec<SupervisorActivityCheckpoint>,
    /// False for legacy bare snapshots that require one full replay to migrate.
    pub replay_base_complete: bool,
}

impl EventStore {
    /// Open, key, verify, and migrate a database atomically.
    ///
    /// # Errors
    ///
    /// Returns a database, cipher-availability, or migration error.
    pub fn open(path: &Path, key: &SecretString) -> Result<Self, StorageError> {
        let existed = path.exists();
        let connection = Connection::open(path)?;
        Self::configure(&connection, key, !path.as_os_str().is_empty())?;
        let backup = existed.then_some((path, key));
        Self::migrate(&connection, backup)?;
        Ok(Self { connection })
    }

    /// In-memory encrypted store for tests and ephemeral guest sessions.
    ///
    /// # Errors
    ///
    /// Returns a database, cipher-availability, or migration error.
    pub fn open_in_memory(key: &SecretString) -> Result<Self, StorageError> {
        let connection = Connection::open_in_memory()?;
        Self::configure(&connection, key, false)?;
        Self::migrate(&connection, None)?;
        Ok(Self { connection })
    }

    fn configure(
        connection: &Connection,
        key: &SecretString,
        file_backed: bool,
    ) -> Result<(), StorageError> {
        connection.pragma_update(None, "key", key.expose_secret())?;
        let cipher: Option<String> = connection
            .query_row("PRAGMA cipher_version", [], |row| row.get(0))
            .optional()?;
        if cipher.as_deref().is_none_or(str::is_empty) {
            return Err(StorageError::SqlCipherUnavailable);
        }
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "busy_timeout", 5000_i64)?;
        if file_backed {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        Ok(())
    }

    fn migrate(
        connection: &Connection,
        backup: Option<(&Path, &SecretString)>,
    ) -> Result<(), StorageError> {
        let version = database_schema_version(connection)?;
        if version > SCHEMA_VERSION {
            return Err(StorageError::DowngradeRefused {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        if version != 0 && version < BASELINE_SCHEMA_VERSION {
            return Err(StorageError::UnsupportedDatabaseSchema(version));
        }
        if version == SCHEMA_VERSION {
            return Ok(());
        }

        if version >= BASELINE_SCHEMA_VERSION {
            let Some((path, key)) = backup else {
                return Err(StorageError::MigrationBackupRequired(version));
            };
            let backup_path = migration_backup_path(path, version, SCHEMA_VERSION);
            backup_connection_to(connection, &backup_path, key)?;
        }

        let tx = connection.unchecked_transaction()?;
        let mut migrated_version = version;
        if migrated_version == 0 {
            migrate_to_v4(&tx)?;
            migrated_version = BASELINE_SCHEMA_VERSION;
            tx.pragma_update(None, "user_version", migrated_version)?;
        }
        while migrated_version < SCHEMA_VERSION {
            let next_version = migrated_version + 1;
            match next_version {
                5 => migrate_to_v5(&tx)?,
                _ => return Err(StorageError::UnsupportedDatabaseSchema(migrated_version)),
            }
            migrated_version = next_version;
            tx.pragma_update(None, "user_version", migrated_version)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Atomically append an event. Sequence and event ID uniqueness make retries safe.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported/out-of-range events or failed storage.
    pub fn append(&mut self, event: &EventEnvelope) -> Result<(), StorageError> {
        if event.schema_version != personal_agent_contracts::EVENT_SCHEMA_VERSION {
            return Err(StorageError::UnsupportedEventSchema(event.schema_version));
        }
        let sequence = i64::try_from(event.monotonic_sequence)
            .map_err(|_| StorageError::SequenceOutOfRange(event.monotonic_sequence))?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO events(monotonic_sequence,event_id,profile_id,event_type,wall_clock_timestamp,envelope) VALUES (?1,?2,?3,?4,?5,?6)",
            params![sequence, event.event_id, event.profile_id, event.r#type, event.wall_clock_timestamp, event.encode_to_vec()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Resume a bounded event subscription after a known sequence number.
    ///
    /// # Errors
    ///
    /// Returns an error for out-of-range sequences, database reads, or corrupt
    /// protobuf envelopes.
    pub fn after(&self, sequence: u64, limit: usize) -> Result<Vec<EventEnvelope>, StorageError> {
        let sequence =
            i64::try_from(sequence).map_err(|_| StorageError::SequenceOutOfRange(sequence))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare("SELECT envelope FROM events WHERE monotonic_sequence > ?1 ORDER BY monotonic_sequence LIMIT ?2")?;
        let blobs =
            statement.query_map(params![sequence, limit], |row| row.get::<_, Vec<u8>>(0))?;
        blobs
            .map(|blob| Ok(EventEnvelope::decode(blob?.as_slice())?))
            .collect()
    }

    /// Highest committed sequence for projection recovery.
    ///
    /// # Errors
    ///
    /// Returns a database read error.
    pub fn last_sequence(&self) -> Result<u64, StorageError> {
        let value: i64 = self.connection.query_row(
            "SELECT COALESCE(MAX(monotonic_sequence), 0) FROM events",
            [],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(value).unwrap_or_default())
    }

    /// Persist a rebuildable application projection and its replay boundary.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn save_projection_checkpoint<T: serde::Serialize>(
        &mut self,
        projection_snapshot_blob: &T,
        last_sequence: u64,
    ) -> Result<(), StorageError> {
        let checkpoint = ProjectionCheckpoint {
            projection_snapshot_blob,
            last_sequence,
        };
        save_snapshot(
            &mut self.connection,
            PROJECTION_CHECKPOINT_KIND,
            PROJECTION_CHECKPOINT_ID,
            &checkpoint,
        )
    }

    /// Load the latest application projection checkpoint, if one exists.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn projection_checkpoint<T: serde::de::DeserializeOwned>(
        &self,
    ) -> Result<Option<ProjectionCheckpoint<T>>, StorageError> {
        load_snapshot(
            &self.connection,
            PROJECTION_CHECKPOINT_KIND,
            PROJECTION_CHECKPOINT_ID,
        )
    }

    /// Persist a content-free migration run report inside the encrypted store.
    ///
    /// # Errors
    ///
    /// Returns a serialization or database transaction error.
    pub fn record_migration_report(
        &mut self,
        report: &personal_agent_migration::MigrationReport,
    ) -> Result<(), StorageError> {
        let body = report.to_json_pretty()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO migration_runs(id,source_fingerprint,state,body_json) VALUES (?1,?2,'completed',?3)",
            params![report.run_id, report.source_fingerprint, body],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Store bytes once under their SHA-256 address inside `SQLCipher`.
    ///
    /// # Errors
    ///
    /// Returns a database error or a length-conversion error.
    pub fn store_blob(&mut self, bytes: &[u8]) -> Result<String, StorageError> {
        let hash = hex(&Sha256::digest(bytes));
        let length = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO blobs(content_hash,byte_length,body) VALUES (?1,?2,?3)",
            params![hash, length, bytes],
        )?;
        tx.commit()?;
        Ok(hash)
    }

    /// Retrieve an exact content-addressed blob.
    ///
    /// # Errors
    ///
    /// Returns `BlobMissing` or a database error.
    pub fn blob(&self, hash: &str) -> Result<Vec<u8>, StorageError> {
        self.connection
            .query_row(
                "SELECT body FROM blobs WHERE content_hash = ?1",
                [hash],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StorageError::BlobMissing(hash.into()))
    }

    /// Create a consistent encrypted database backup before update/migration.
    /// The destination must not already exist.
    ///
    /// # Errors
    ///
    /// Returns destination, I/O, `SQLCipher`, or backup errors.
    pub fn backup_to(&self, path: &Path, key: &SecretString) -> Result<(), StorageError> {
        backup_connection_to(&self.connection, path, key)
    }

    /// Export all event envelopes to a private, atomic JSON file. This is an
    /// explicit user-data export and therefore includes event payloads.
    ///
    /// # Errors
    ///
    /// Returns destination, decode, serialization, or I/O errors.
    pub fn export_events_json(&self, path: &Path) -> Result<(), StorageError> {
        reject_existing_destination(path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let events = self.after(0, usize::MAX)?;
        let events = events
            .into_iter()
            .map(|event| {
                Ok(serde_json::json!({
                    "schema_version": event.schema_version,
                    "event_id": event.event_id,
                    "wall_clock_timestamp": event.wall_clock_timestamp,
                    "monotonic_sequence": event.monotonic_sequence,
                    "origin": event.origin,
                    "profile_id": event.profile_id,
                    "session_id": event.session_id,
                    "goal_id": event.goal_id,
                    "task_id": event.task_id,
                    "agent_id": event.agent_id,
                    "type": event.r#type,
                    "payload": serde_json::from_slice::<serde_json::Value>(&event.payload_json)?,
                }))
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?;
        let body = serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "product": "Personal Agent",
            "events": events,
        }))?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("export");
        let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        if let Err(error) = (|| -> Result<(), std::io::Error> {
            file.write_all(&body)?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Ok(())
        })() {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(())
    }

    /// Atomically persist a durable task supervisor snapshot.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn save_supervisor_snapshot(
        &mut self,
        snapshot: &personal_agent_agent::SupervisorSnapshot,
    ) -> Result<(), StorageError> {
        let id = snapshot.graph.goal_id.to_string();
        let checkpoint = self
            .supervisor_recovery_checkpoint(snapshot.graph.goal_id)?
            .map_or_else(
                || SupervisorRecoveryCheckpoint {
                    snapshot: snapshot.clone(),
                    last_sequence: 0,
                    latest_goal_event: None,
                    pending_approval_events: Vec::new(),
                    recent_activities: Vec::new(),
                    replay_base_complete: false,
                },
                |mut checkpoint| {
                    checkpoint.snapshot.clone_from(snapshot);
                    checkpoint
                },
            );
        save_snapshot(
            &mut self.connection,
            SUPERVISOR_SNAPSHOT_KIND,
            &id,
            &checkpoint,
        )
    }

    /// Load a task supervisor snapshot; absence is `Ok(None)`.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn supervisor_snapshot(
        &self,
        goal_id: uuid::Uuid,
    ) -> Result<Option<personal_agent_agent::SupervisorSnapshot>, StorageError> {
        Ok(self
            .supervisor_recovery_checkpoint(goal_id)?
            .map(|checkpoint| checkpoint.snapshot))
    }

    /// Load one enriched task-supervisor recovery checkpoint.
    ///
    /// Legacy bare snapshots are returned with `replay_base_complete = false`
    /// so callers safely fall back to a full event replay once.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn supervisor_recovery_checkpoint(
        &self,
        goal_id: uuid::Uuid,
    ) -> Result<Option<SupervisorRecoveryCheckpoint>, StorageError> {
        load_supervisor_checkpoint(&self.connection, &goal_id.to_string())
    }

    /// Load all task-supervisor recovery checkpoints in stable goal-ID order.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn supervisor_recovery_checkpoints(
        &self,
    ) -> Result<Vec<SupervisorRecoveryCheckpoint>, StorageError> {
        let mut statement = self
            .connection
            .prepare("SELECT body_json FROM runtime_snapshots WHERE kind=?1 ORDER BY id")?;
        let bodies =
            statement.query_map([SUPERVISOR_SNAPSHOT_KIND], |row| row.get::<_, String>(0))?;
        bodies
            .map(|body| decode_supervisor_checkpoint(&body?))
            .collect()
    }

    /// Atomically persist a supervisor transition and its append-only projection event.
    ///
    /// # Errors
    /// Returns schema, sequence, JSON, or database errors.
    pub fn save_supervisor_snapshot_and_event(
        &mut self,
        snapshot: &personal_agent_agent::SupervisorSnapshot,
        event: &EventEnvelope,
    ) -> Result<(), StorageError> {
        validate_event(event)?;
        let sequence = i64::try_from(event.monotonic_sequence)
            .map_err(|_| StorageError::SequenceOutOfRange(event.monotonic_sequence))?;
        let id = snapshot.graph.goal_id.to_string();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = load_supervisor_checkpoint(&tx, &id)?;
        let checkpoint = evolve_supervisor_checkpoint(previous, snapshot, event)?;
        let body = serde_json::to_string(&checkpoint)?;
        tx.execute(
            "INSERT INTO runtime_snapshots(kind,id,body_json,updated_at)
             VALUES (?1,?2,?3,strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(kind,id) DO UPDATE SET body_json=excluded.body_json, updated_at=excluded.updated_at",
            params![SUPERVISOR_SNAPSHOT_KIND, id, body],
        )?;
        tx.execute(
            "INSERT INTO events(monotonic_sequence,event_id,profile_id,event_type,wall_clock_timestamp,envelope) VALUES (?1,?2,?3,?4,?5,?6)",
            params![sequence, event.event_id, event.profile_id, event.r#type, event.wall_clock_timestamp, event.encode_to_vec()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically persist the complete scheduler snapshot.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn save_scheduler_snapshot(
        &mut self,
        profile_id: &str,
        snapshot: &personal_agent_automation::SchedulerSnapshot,
    ) -> Result<(), StorageError> {
        save_snapshot(
            &mut self.connection,
            "automation-scheduler",
            profile_id,
            snapshot,
        )
    }

    /// Load the scheduler snapshot for a profile; absence is `Ok(None)`.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn scheduler_snapshot(
        &self,
        profile_id: &str,
    ) -> Result<Option<personal_agent_automation::SchedulerSnapshot>, StorageError> {
        load_snapshot(&self.connection, "automation-scheduler", profile_id)
    }

    /// Atomically persist the provenance-first memory index and vectors.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn save_memory_snapshot(
        &mut self,
        profile_id: &str,
        snapshot: &personal_agent_memory::MemoryStore,
    ) -> Result<(), StorageError> {
        save_snapshot(&mut self.connection, "memory-index", profile_id, snapshot)
    }

    /// Load the memory snapshot for a profile; absence is `Ok(None)`.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn memory_snapshot(
        &self,
        profile_id: &str,
    ) -> Result<Option<personal_agent_memory::MemoryStore>, StorageError> {
        load_snapshot(&self.connection, "memory-index", profile_id)
    }

    /// Atomically persist the complete namespaced memory system.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn save_persistent_memory_snapshot(
        &mut self,
        profile_id: &str,
        snapshot: &personal_agent_memory::PersistentMemory,
    ) -> Result<(), StorageError> {
        save_snapshot(
            &mut self.connection,
            "memory-system-v2",
            profile_id,
            snapshot,
        )
    }

    /// Load the complete namespaced memory system; absence is `Ok(None)`.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn persistent_memory_snapshot(
        &self,
        profile_id: &str,
    ) -> Result<Option<personal_agent_memory::PersistentMemory>, StorageError> {
        load_snapshot(&self.connection, "memory-system-v2", profile_id)
    }

    /// Persist an application-owned encrypted runtime snapshot.
    ///
    /// This narrow generic boundary avoids crate cycles for higher-level core
    /// subsystems while retaining the same atomic `SQLCipher` transaction.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn save_runtime_snapshot<T: serde::Serialize>(
        &mut self,
        kind: &str,
        id: &str,
        snapshot: &T,
    ) -> Result<(), StorageError> {
        save_snapshot(&mut self.connection, kind, id, snapshot)
    }

    /// Load an application-owned encrypted runtime snapshot.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn runtime_snapshot<T: serde::de::DeserializeOwned>(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<Option<T>, StorageError> {
        load_snapshot(&self.connection, kind, id)
    }

    /// Atomically persist one runtime snapshot and its append-only domain event.
    ///
    /// # Errors
    /// Returns schema, sequence, JSON, or database errors.
    pub fn save_runtime_snapshot_and_event<T: serde::Serialize>(
        &mut self,
        kind: &str,
        id: &str,
        snapshot: &T,
        event: &EventEnvelope,
    ) -> Result<(), StorageError> {
        if event.schema_version != personal_agent_contracts::EVENT_SCHEMA_VERSION {
            return Err(StorageError::UnsupportedEventSchema(event.schema_version));
        }
        let sequence = i64::try_from(event.monotonic_sequence)
            .map_err(|_| StorageError::SequenceOutOfRange(event.monotonic_sequence))?;
        let body = serde_json::to_string(snapshot)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO runtime_snapshots(kind,id,body_json,updated_at)
             VALUES (?1,?2,?3,strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(kind,id) DO UPDATE SET body_json=excluded.body_json, updated_at=excluded.updated_at",
            params![kind, id, body],
        )?;
        tx.execute(
            "INSERT INTO events(monotonic_sequence,event_id,profile_id,event_type,wall_clock_timestamp,envelope) VALUES (?1,?2,?3,?4,?5,?6)",
            params![sequence, event.event_id, event.profile_id, event.r#type, event.wall_clock_timestamp, event.encode_to_vec()],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn database_schema_version(connection: &Connection) -> Result<i64, StorageError> {
    let user_version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if user_version != 0 {
        return Ok(user_version);
    }

    // Releases before PERF-6 recorded v4 only in `schema_migrations`. Recognize
    // that one legacy marker so the first user-version migration can still make
    // a backup before changing any schema state.
    let has_legacy_marker: bool = connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_schema
           WHERE type = 'table' AND name = 'schema_migrations'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_legacy_marker {
        return Ok(0);
    }
    let version = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    Ok(version)
}

fn migrate_to_v4(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS events(
           monotonic_sequence INTEGER PRIMARY KEY,
           event_id TEXT NOT NULL UNIQUE,
           profile_id TEXT NOT NULL,
           event_type TEXT NOT NULL,
           wall_clock_timestamp TEXT NOT NULL,
           envelope BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS profiles(id TEXT PRIMARY KEY, created_at TEXT NOT NULL, mode TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, status TEXT NOT NULL, updated_at TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS goals(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, status TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, goal_id TEXT NOT NULL, status TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS task_edges(goal_id TEXT NOT NULL, dependency_id TEXT NOT NULL, dependent_id TEXT NOT NULL, PRIMARY KEY(goal_id, dependency_id, dependent_id));
         CREATE TABLE IF NOT EXISTS agent_runs(id TEXT PRIMARY KEY, task_id TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS tool_runs(id TEXT PRIMARY KEY, task_id TEXT, tool_id TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS permission_requests(id TEXT PRIMARY KEY, task_id TEXT, state TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS consent_grants(id TEXT PRIMARY KEY, goal_id TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS checkpoints(id TEXT PRIMARY KEY, task_id TEXT NOT NULL, coverage TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS memories(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, trust TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS memory_links(memory_id TEXT NOT NULL, source_event_id TEXT NOT NULL, PRIMARY KEY(memory_id, source_event_id));
         CREATE TABLE IF NOT EXISTS automations(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS automation_runs(id TEXT PRIMARY KEY, automation_id TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS artifacts(id TEXT PRIMARY KEY, content_hash TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS blobs(content_hash TEXT PRIMARY KEY, byte_length INTEGER NOT NULL, body BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS connectors(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS provider_usage(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, amount_usd REAL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS egress(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, destination TEXT NOT NULL, size_bytes INTEGER NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS settings(profile_id TEXT NOT NULL, key TEXT NOT NULL, value_json TEXT NOT NULL, PRIMARY KEY(profile_id, key));
         CREATE TABLE IF NOT EXISTS migration_runs(id TEXT PRIMARY KEY, source_fingerprint TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS migration_items(
           id TEXT PRIMARY KEY,
           kind TEXT NOT NULL,
           source_locator TEXT NOT NULL,
           source_modified_at TEXT,
           content_sha256 TEXT NOT NULL,
           destination TEXT NOT NULL,
           enabled INTEGER NOT NULL,
           contains_personal_data INTEGER NOT NULL,
           payload BLOB NOT NULL,
           imported_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS runtime_snapshots(
           kind TEXT NOT NULL,
           id TEXT NOT NULL,
           body_json TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           PRIMARY KEY(kind,id)
         );",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at)
         VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        [BASELINE_SCHEMA_VERSION],
    )?;
    Ok(())
}

fn migrate_to_v5(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute_batch(
        "ALTER TABLE provider_usage ADD COLUMN day TEXT NOT NULL DEFAULT '';
         ALTER TABLE provider_usage ADD COLUMN session_id TEXT;
         ALTER TABLE egress ADD COLUMN day TEXT NOT NULL DEFAULT '';
         ALTER TABLE egress ADD COLUMN session_id TEXT;
         CREATE INDEX provider_usage_profile_day_idx ON provider_usage(profile_id, day);
         CREATE INDEX provider_usage_session_id_idx ON provider_usage(session_id);
         CREATE INDEX egress_profile_day_idx ON egress(profile_id, day);
         CREATE INDEX egress_session_id_idx ON egress(session_id);",
    )?;
    tx.execute(
        "INSERT INTO schema_migrations(version, applied_at)
         VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        [SCHEMA_VERSION],
    )?;
    Ok(())
}

fn migration_backup_path(path: &Path, from_version: i64, to_version: i64) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("personal-agent.db");
    path.with_file_name(format!(
        "{file_name}.pre-v{from_version}-to-v{to_version}.backup"
    ))
}

fn backup_connection_to(
    source: &Connection,
    path: &Path,
    key: &SecretString,
) -> Result<(), StorageError> {
    reject_existing_destination(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    drop(options.open(path)?);
    let mut destination = Connection::open(path)?;
    EventStore::configure(&destination, key, true)?;
    {
        let backup = rusqlite::backup::Backup::new(source, &mut destination)?;
        backup.run_to_completion(128, Duration::from_millis(1), None)?;
    }
    destination.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
    Ok(())
}

fn validate_event(event: &EventEnvelope) -> Result<(), StorageError> {
    if event.schema_version != personal_agent_contracts::EVENT_SCHEMA_VERSION {
        return Err(StorageError::UnsupportedEventSchema(event.schema_version));
    }
    i64::try_from(event.monotonic_sequence)
        .map(|_| ())
        .map_err(|_| StorageError::SequenceOutOfRange(event.monotonic_sequence))
}

fn load_supervisor_checkpoint(
    connection: &Connection,
    id: &str,
) -> Result<Option<SupervisorRecoveryCheckpoint>, StorageError> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body_json FROM runtime_snapshots WHERE kind=?1 AND id=?2",
            params![SUPERVISOR_SNAPSHOT_KIND, id],
            |row| row.get(0),
        )
        .optional()?;
    body.map(|body| decode_supervisor_checkpoint(&body))
        .transpose()
}

fn decode_supervisor_checkpoint(body: &str) -> Result<SupervisorRecoveryCheckpoint, StorageError> {
    if let Ok(checkpoint) = serde_json::from_str(body) {
        return Ok(checkpoint);
    }
    let snapshot = serde_json::from_str(body)?;
    Ok(SupervisorRecoveryCheckpoint {
        snapshot,
        last_sequence: 0,
        latest_goal_event: None,
        pending_approval_events: Vec::new(),
        recent_activities: Vec::new(),
        replay_base_complete: false,
    })
}

fn evolve_supervisor_checkpoint(
    previous: Option<SupervisorRecoveryCheckpoint>,
    snapshot: &personal_agent_agent::SupervisorSnapshot,
    event: &EventEnvelope,
) -> Result<SupervisorRecoveryCheckpoint, StorageError> {
    let mut checkpoint = previous.unwrap_or_else(|| SupervisorRecoveryCheckpoint {
        snapshot: snapshot.clone(),
        last_sequence: 0,
        latest_goal_event: None,
        pending_approval_events: Vec::new(),
        recent_activities: Vec::new(),
        replay_base_complete: event.r#type == "goal.created",
    });
    checkpoint.snapshot.clone_from(snapshot);
    checkpoint.last_sequence = event.monotonic_sequence;
    checkpoint.latest_goal_event = Some(event.clone());

    let payload: serde_json::Value = serde_json::from_slice(&event.payload_json)?;
    let task_id = payload.get("task_id").and_then(serde_json::Value::as_str);
    if event.r#type == "approval.requested" {
        if let Some(task_id) = task_id {
            checkpoint
                .pending_approval_events
                .retain(|pending| event_task_id(pending).as_deref() != Some(task_id));
            checkpoint.pending_approval_events.push(event.clone());
        } else {
            checkpoint.replay_base_complete = false;
        }
    }
    if event.r#type == "approval.resolved"
        || payload
            .get("approval_resolved")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    {
        if let Some(task_id) = task_id {
            checkpoint
                .pending_approval_events
                .retain(|pending| event_task_id(pending).as_deref() != Some(task_id));
        } else if event.r#type == "approval.resolved" {
            checkpoint.replay_base_complete = false;
        }
    }
    if payload
        .get("approvals_resolved")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        checkpoint.pending_approval_events.clear();
    }

    checkpoint
        .recent_activities
        .push(supervisor_activity(event, &payload));
    if checkpoint.recent_activities.len() > SUPERVISOR_RECENT_EVENT_LIMIT {
        let excess = checkpoint.recent_activities.len() - SUPERVISOR_RECENT_EVENT_LIMIT;
        checkpoint.recent_activities.drain(..excess);
    }
    Ok(checkpoint)
}

fn event_task_id(event: &EventEnvelope) -> Option<String> {
    event.task_id.clone().or_else(|| {
        serde_json::from_slice::<serde_json::Value>(&event.payload_json)
            .ok()?
            .get("task_id")?
            .as_str()
            .map(str::to_owned)
    })
}

fn supervisor_activity(
    event: &EventEnvelope,
    payload: &serde_json::Value,
) -> SupervisorActivityCheckpoint {
    SupervisorActivityCheckpoint {
        sequence: event.monotonic_sequence,
        event_type: event.r#type.clone(),
        goal_id: event.goal_id.clone().or_else(|| {
            payload
                .get("goal_id")
                .or_else(|| payload.get("id"))
                .or_else(|| payload.pointer("/goal/id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        }),
        task_id: event.task_id.clone().or_else(|| {
            payload
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        }),
        timestamp: event.wall_clock_timestamp.clone(),
    }
}

fn save_snapshot<T: serde::Serialize>(
    connection: &mut Connection,
    kind: &str,
    id: &str,
    snapshot: &T,
) -> Result<(), StorageError> {
    let body = serde_json::to_string(snapshot)?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO runtime_snapshots(kind,id,body_json,updated_at)
         VALUES (?1,?2,?3,strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         ON CONFLICT(kind,id) DO UPDATE SET body_json=excluded.body_json, updated_at=excluded.updated_at",
        params![kind, id, body],
    )?;
    tx.commit()?;
    Ok(())
}

fn load_snapshot<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    kind: &str,
    id: &str,
) -> Result<Option<T>, StorageError> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body_json FROM runtime_snapshots WHERE kind=?1 AND id=?2",
            params![kind, id],
            |row| row.get(0),
        )
        .optional()?;
    body.map(|body| serde_json::from_str(&body))
        .transpose()
        .map_err(Into::into)
}

fn reject_existing_destination(path: &Path) -> Result<(), StorageError> {
    if path.exists() {
        Err(StorageError::DestinationExists(path.into()))
    } else {
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

impl personal_agent_migration::MigrationSink for EventStore {
    type Error = StorageError;

    fn contains(&mut self, record_id: &str) -> Result<bool, Self::Error> {
        let present = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM migration_items WHERE id = ?1)",
            [record_id],
            |row| row.get::<_, bool>(0),
        )?;
        Ok(present)
    }

    fn store(
        &mut self,
        record: &personal_agent_migration::PreparedRecord,
    ) -> Result<(), Self::Error> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO migration_items(
               id,kind,source_locator,source_modified_at,content_sha256,destination,
               enabled,contains_personal_data,payload,imported_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            params![
                record.id,
                record.kind,
                record.source_locator,
                record.source_modified_at,
                record.content_sha256,
                record.destination,
                record.enabled,
                record.contains_personal_data,
                record.payload(),
            ],
        )?;
        materialize_migration_record(&tx, record)?;
        tx.commit()?;
        Ok(())
    }
}

fn materialize_migration_record(
    tx: &Transaction<'_>,
    record: &personal_agent_migration::PreparedRecord,
) -> Result<(), StorageError> {
    let payload = String::from_utf8_lossy(record.payload());
    match record.kind.as_str() {
        "history-event" => {
            let last: i64 = tx.query_row(
                "SELECT COALESCE(MAX(monotonic_sequence), 0) FROM events",
                [],
                |row| row.get(0),
            )?;
            let sequence = last
                .checked_add(1)
                .ok_or(StorageError::SequenceOutOfRange(u64::MAX))?;
            let timestamp = record
                .source_modified_at
                .clone()
                .unwrap_or_else(|| "1970-01-01T00:00:00+00:00".to_owned());
            let event = EventEnvelope {
                schema_version: personal_agent_contracts::EVENT_SCHEMA_VERSION,
                event_id: record.id.clone(),
                wall_clock_timestamp: timestamp.clone(),
                monotonic_sequence: u64::try_from(sequence)
                    .map_err(|_| StorageError::SequenceOutOfRange(u64::MAX))?,
                origin: "legacy-migration".to_owned(),
                profile_id: "default".to_owned(),
                session_id: None,
                goal_id: None,
                task_id: None,
                agent_id: None,
                r#type: "conversation.legacy-imported".to_owned(),
                payload_json: record.payload().to_vec(),
            };
            tx.execute(
                "INSERT INTO events(monotonic_sequence,event_id,profile_id,event_type,wall_clock_timestamp,envelope) VALUES (?1,?2,?3,?4,?5,?6)",
                params![sequence, event.event_id, event.profile_id, event.r#type, timestamp, event.encode_to_vec()],
            )?;
        }
        "memory" => {
            let body = serde_json::to_string(&serde_json::json!({
                "origin": "legacy-migration",
                "source_locator": record.source_locator,
                "source_modified_at": record.source_modified_at,
                "content_sha256": record.content_sha256,
                "markdown": payload,
            }))?;
            tx.execute(
                "INSERT INTO memories(id,profile_id,trust,body_json) VALUES (?1,'default','legacy-imported',?2)",
                params![record.id, body],
            )?;
        }
        "automation" => {
            tx.execute(
                "INSERT INTO automations(id,profile_id,state,body_json) VALUES (?1,'default','disabled',?2)",
                params![record.id, payload.as_ref()],
            )?;
        }
        "connector-metadata" => {
            tx.execute(
                "INSERT INTO connectors(id,profile_id,state,body_json) VALUES (?1,'default','disabled',?2)",
                params![record.id, payload.as_ref()],
            )?;
        }
        "settings" | "conversation-state" | "projects" | "remote-device-metadata" => {
            tx.execute(
                "INSERT INTO settings(profile_id,key,value_json) VALUES ('default',?1,?2)",
                params![
                    format!("migration:{}:{}", record.destination, record.id),
                    payload.as_ref()
                ],
            )?;
        }
        "skill-file" | "expert-file" | "theme" => {
            let body = serde_json::to_string(&serde_json::json!({
                "origin": "legacy-migration",
                "migration_item_id": record.id,
                "source_locator": record.source_locator,
                "destination": record.destination,
                "enabled": false,
            }))?;
            tx.execute(
                "INSERT INTO artifacts(id,content_hash,body_json) VALUES (?1,?2,?3)",
                params![record.id, record.content_sha256, body],
            )?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn append_and_resume_events() {
        let mut store = EventStore::open_in_memory(&SecretString::from("test-only-key".to_owned()))
            .expect("store");
        let first = EventEnvelope::new(1, "test", "default", "goal.created", &json!({"id":1}))
            .expect("event");
        let second = EventEnvelope::new(2, "test", "default", "task.created", &json!({"id":2}))
            .expect("event");
        store.append(&first).expect("append");
        store.append(&second).expect("append");
        let resumed = store.after(1, 10).expect("resume");
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].event_id, second.event_id);
        assert_eq!(store.last_sequence().expect("sequence"), 2);
    }

    #[test]
    fn projection_checkpoint_reopens_with_fewer_than_one_hundred_rows_to_replay() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("projection-checkpoint.db");
        let key = SecretString::from("test-only-key".to_owned());
        {
            let mut store = EventStore::open(&database, &key).expect("store");
            for sequence in 1..=10_000 {
                let event = EventEnvelope::new(
                    sequence,
                    "checkpoint-test",
                    "default",
                    "message.user",
                    &json!({"sequence": sequence}),
                )
                .expect("event");
                store.append(&event).expect("append");
                if sequence % 1_000 == 0 {
                    store
                        .save_projection_checkpoint(&json!({"last_sequence": sequence}), sequence)
                        .expect("checkpoint");
                }
            }
        }

        let reopened = EventStore::open(&database, &key).expect("reopen");
        let checkpoint = reopened
            .projection_checkpoint::<serde_json::Value>()
            .expect("load checkpoint")
            .expect("checkpoint exists");
        let replayed_rows = reopened
            .after(checkpoint.last_sequence, 100)
            .expect("tail")
            .len();
        assert!(
            replayed_rows < 100,
            "checkpoint recovery replayed {replayed_rows} rows"
        );
        assert_eq!(checkpoint.last_sequence, 10_000);
        assert_eq!(
            checkpoint.projection_snapshot_blob,
            json!({"last_sequence": 10_000})
        );
    }

    #[test]
    fn v4_database_is_backed_up_then_migrated_to_v5_with_data_intact() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("profile.db");
        let backup = migration_backup_path(&database, 4, 5);
        let key = SecretString::from("test-only-key".to_owned());
        let event = EventEnvelope::new(
            1,
            "migration-test",
            "default",
            "goal.created",
            &json!({"preserved": true}),
        )
        .expect("event");

        {
            let connection = Connection::open(&database).expect("open v4 database");
            EventStore::configure(&connection, &key, true).expect("configure v4 database");
            let tx = connection.unchecked_transaction().expect("v4 transaction");
            migrate_to_v4(&tx).expect("create v4 schema");
            tx.pragma_update(None, "user_version", BASELINE_SCHEMA_VERSION)
                .expect("mark v4");
            tx.execute(
                "INSERT INTO events(
                   monotonic_sequence,event_id,profile_id,event_type,wall_clock_timestamp,envelope
                 ) VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    1_i64,
                    event.event_id,
                    event.profile_id,
                    event.r#type,
                    event.wall_clock_timestamp,
                    event.encode_to_vec()
                ],
            )
            .expect("seed v4 data");
            tx.commit().expect("commit v4 database");
        }

        let migrated = EventStore::open(&database, &key).expect("migrate v4 to v5");
        assert!(backup.is_file(), "pre-migration backup was not created");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&backup)
                    .expect("backup metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert_eq!(
            migrated
                .connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .expect("source user_version"),
            SCHEMA_VERSION
        );
        let migration_rows: i64 = migrated
            .connection
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version IN (4, 5)",
                [],
                |row| row.get(0),
            )
            .expect("migration rows");
        assert_eq!(migration_rows, 2);
        let usage_index: i64 = migrated
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'index' AND name = 'provider_usage_profile_day_idx'",
                [],
                |row| row.get(0),
            )
            .expect("v5 index");
        assert_eq!(usage_index, 1);
        let migrated_events = migrated.after(0, 10).expect("migrated events");
        assert_eq!(migrated_events, vec![event.clone()]);

        let backup_connection = Connection::open(&backup).expect("open backup");
        EventStore::configure(&backup_connection, &key, true).expect("configure backup");
        assert_eq!(
            backup_connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .expect("backup user_version"),
            BASELINE_SCHEMA_VERSION
        );
        let backup_envelope: Vec<u8> = backup_connection
            .query_row(
                "SELECT envelope FROM events WHERE monotonic_sequence = 1",
                [],
                |row| row.get(0),
            )
            .expect("backup event");
        assert_eq!(
            EventEnvelope::decode(backup_envelope.as_slice()).expect("decode backup event"),
            event
        );
    }

    #[test]
    fn newer_database_version_is_rejected_instead_of_downgraded() {
        let temp = tempfile::tempdir().expect("temp");
        let database = temp.path().join("future.db");
        let key = SecretString::from("test-only-key".to_owned());
        drop(EventStore::open(&database, &key).expect("create current database"));
        {
            let connection = Connection::open(&database).expect("open current database");
            EventStore::configure(&connection, &key, true).expect("configure database");
            connection
                .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .expect("mark future version");
        }

        match EventStore::open(&database, &key) {
            Err(StorageError::DowngradeRefused { found, supported }) => {
                assert_eq!(found, SCHEMA_VERSION + 1);
                assert_eq!(supported, SCHEMA_VERSION);
            }
            Err(error) => panic!("unexpected error: {error}"),
            Ok(_) => panic!("future database was silently downgraded"),
        }
    }

    #[test]
    fn duplicate_sequence_is_rejected() {
        let mut store = EventStore::open_in_memory(&SecretString::from("test-only-key".to_owned()))
            .expect("store");
        let event =
            EventEnvelope::new(1, "test", "default", "goal.created", &json!({})).expect("event");
        store.append(&event).expect("first");
        assert!(store.append(&event).is_err());
    }

    #[test]
    fn snapshot_and_event_commit_or_rollback_together() {
        let mut store = EventStore::open_in_memory(&SecretString::from("test-only-key".to_owned()))
            .expect("store");
        let first = EventEnvelope::new(
            1,
            "test",
            "default",
            "artifact.created",
            &json!({"version": 1}),
        )
        .expect("event");
        store
            .save_runtime_snapshot_and_event(
                "artifact-workspace-v1",
                "default",
                &json!({"version": 1}),
                &first,
            )
            .expect("commit");
        let duplicate = EventEnvelope::new(
            1,
            "test",
            "default",
            "artifact.changed",
            &json!({"version": 2}),
        )
        .expect("duplicate");
        assert!(
            store
                .save_runtime_snapshot_and_event(
                    "artifact-workspace-v1",
                    "default",
                    &json!({"version": 2}),
                    &duplicate,
                )
                .is_err()
        );
        assert_eq!(
            store
                .runtime_snapshot::<serde_json::Value>("artifact-workspace-v1", "default")
                .expect("load"),
            Some(json!({"version": 1}))
        );
    }

    #[test]
    fn blobs_deduplicate_and_backup_reopens_with_the_same_key() {
        let temp = tempfile::tempdir().expect("temp");
        let source = temp.path().join("profile.db");
        let backup = temp.path().join("profile.backup.db");
        let key = SecretString::from("test-only-key".to_owned());
        let mut store = EventStore::open(&source, &key).expect("store");
        let event = EventEnvelope::new(1, "test", "default", "artifact.created", &json!({}))
            .expect("event");
        store.append(&event).expect("append");
        let first = store.store_blob(b"large encrypted bytes").expect("blob");
        let second = store.store_blob(b"large encrypted bytes").expect("dedup");
        assert_eq!(first, second);
        assert_eq!(store.blob(&first).expect("read"), b"large encrypted bytes");
        store.backup_to(&backup, &key).expect("backup");
        let reopened = EventStore::open(&backup, &key).expect("reopen backup");
        assert_eq!(reopened.last_sequence().expect("sequence"), 1);
        assert_eq!(
            reopened.blob(&first).expect("backup blob"),
            b"large encrypted bytes"
        );
    }

    #[test]
    fn event_export_is_private_atomic_and_never_overwrites() {
        let temp = tempfile::tempdir().expect("temp");
        let export = temp.path().join("profile-export.json");
        let mut store = EventStore::open_in_memory(&SecretString::from("test-only-key".to_owned()))
            .expect("store");
        store
            .append(
                &EventEnvelope::new(
                    1,
                    "test",
                    "default",
                    "message.user",
                    &json!({"text":"owned data"}),
                )
                .expect("event"),
            )
            .expect("append");
        store.export_events_json(&export).expect("export");
        let body = fs::read_to_string(&export).expect("read");
        assert!(body.contains("owned data"));
        assert!(matches!(
            store.export_events_json(&export),
            Err(StorageError::DestinationExists(_))
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&export).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn durable_runtime_snapshots_survive_database_restart() {
        use personal_agent_agent::{DurableSupervisor, ExecutionZone, Task, TaskGraph, WorkStatus};
        use std::collections::{BTreeMap, BTreeSet};

        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("durable.db");
        let key = SecretString::from("test-only-key".to_owned());
        let goal_id = Uuid::now_v7();
        let task = Task {
            id: Uuid::now_v7(),
            goal_id,
            parent_task_id: None,
            title: "durable task".into(),
            assigned_agent: "executor".into(),
            workspace: None,
            browser_profile: None,
            tool_scopes: BTreeSet::new(),
            risk: "read".into(),
            execution_zone: ExecutionZone::Isolated,
            max_attempts: 3,
            attempt: 0,
            idempotency_key: None,
            checkpoint_id: None,
            status: WorkStatus::Queued,
            progress: 0,
            output: None,
        };
        let graph = TaskGraph {
            goal_id,
            revision: 1,
            tasks: BTreeMap::from([(task.id, task)]),
            edges: vec![],
        };
        let supervisor = DurableSupervisor::new(graph, 3, 3).expect("supervisor");
        {
            let mut store = EventStore::open(&path, &key).expect("store");
            store
                .save_supervisor_snapshot(supervisor.snapshot())
                .expect("save agent");
            store
                .save_scheduler_snapshot(
                    "default",
                    &personal_agent_automation::SchedulerSnapshot::default(),
                )
                .expect("save scheduler");
            store
                .save_memory_snapshot("default", &personal_agent_memory::MemoryStore::default())
                .expect("save memory");
        }
        let store = EventStore::open(&path, &key).expect("reopen");
        let agent = store
            .supervisor_snapshot(goal_id)
            .expect("load agent")
            .expect("agent exists");
        assert_eq!(agent.graph.goal_id, goal_id);
        assert_eq!(
            store
                .scheduler_snapshot("default")
                .expect("scheduler")
                .expect("scheduler exists"),
            personal_agent_automation::SchedulerSnapshot::default()
        );
        assert!(
            store
                .memory_snapshot("default")
                .expect("memory")
                .expect("memory exists")
                .export()
                .is_empty()
        );
    }

    #[test]
    fn migration_sink_is_encrypted_idempotent_and_excludes_secret_payloads() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/legacy/synthetic-v1");
        let plan = personal_agent_migration::discover(&fixture).expect("plan");
        let consent = personal_agent_migration::MigrationConsent {
            copy_personal_data: true,
            adopt_opencode_auth: false,
        };
        let mut store = EventStore::open_in_memory(&SecretString::from("test-only-key".to_owned()))
            .expect("store");

        let first =
            personal_agent_migration::migrate(&plan, consent, &mut store).expect("first migration");
        store
            .record_migration_report(&first)
            .expect("migration report");
        let second = personal_agent_migration::migrate(&plan, consent, &mut store)
            .expect("second migration");

        assert!(first.summary.imported > 5);
        assert_eq!(second.summary.imported, 0);
        assert_eq!(second.summary.already_present, first.summary.imported);
        let leaked: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM migration_items WHERE instr(CAST(payload AS TEXT), 'fixture-provider-value-must-never-migrate') > 0",
                [],
                |row| row.get(0),
            )
            .expect("secret check");
        assert_eq!(leaked, 0);
        let history_events: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE event_type = 'conversation.legacy-imported'",
                [],
                |row| row.get(0),
            )
            .expect("history events");
        let memories: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE trust = 'legacy-imported'",
                [],
                |row| row.get(0),
            )
            .expect("memories");
        let disabled_automations: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM automations WHERE state = 'disabled'",
                [],
                |row| row.get(0),
            )
            .expect("automations");
        assert_eq!(history_events, 2);
        assert_eq!(memories, 1);
        assert_eq!(disabled_automations, 1);
        let migration_runs: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM migration_runs", [], |row| row.get(0))
            .expect("migration runs");
        assert_eq!(migration_runs, 1);
    }
}
