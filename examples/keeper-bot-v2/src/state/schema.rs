//! Persistent task state schema and database store.
//!
//! This module manages the keeper's durable record of task outcomes across
//! process restarts. It tracks what this keeper has done to each task (claimed,
//! executed, expired) and when, ensuring a restart never re-attempts a task
//! already finished in a prior run.
//!
//! The schema mirrors the indexer's design: `task_outcomes` is append-only
//! (via upsert-on-conflict), and any derived queries fold this history rather
//! than maintaining separate mutable state.

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

/// Outcome status values for a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Claimed,
    Executed,
    Expired,
    /// In-progress state for issue #381 (concurrent round processing).
    /// Used to mark a task as "claimed but execution not yet submitted" so concurrent
    /// rounds can detect a prior claim is in flight and avoid duplicate submission.
    InProgress,
}

impl TaskStatus {
    /// Convert to the string representation stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Executed => "executed",
            Self::Expired => "expired",
            Self::InProgress => "in_progress",
        }
    }

    /// Parse from a database string.
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "claimed" => Ok(Self::Claimed),
            "executed" => Ok(Self::Executed),
            "expired" => Ok(Self::Expired),
            "in_progress" => Ok(Self::InProgress),
            _ => Err(anyhow::anyhow!("unknown task status: {}", s)),
        }
    }
}

/// A recorded task outcome.
#[derive(Debug, Clone)]
pub struct TaskOutcome {
    /// The task id this outcome is for.
    pub task_id: u32,
    /// The status of the last action taken on this task.
    pub status: TaskStatus,
    /// Unix timestamp when this action was recorded.
    pub action_timestamp: i64,
    /// Outcome details as JSON (e.g., gas cost, proof, etc.).
    pub outcome_json: Value,
}

impl TaskOutcome {
    /// Create a new task outcome with current timestamp.
    pub fn new(task_id: u32, status: TaskStatus, outcome_json: Value) -> Self {
        Self {
            task_id,
            status,
            action_timestamp: Utc::now().timestamp(),
            outcome_json,
        }
    }
}

/// Handle to the persistent task state store.
#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open or create the store at `database_url` and apply any pending migrations.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(database_url)
            .with_context(|| format!("invalid database url: {}", database_url))?
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .context("connecting to the task state store")?;

        // Apply all pending migrations from the migrations/ directory
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("applying schema migrations")?;

        Ok(Self { pool })
    }

    /// Record a task outcome. If a task with this id already exists, update it.
    ///
    /// This is idempotent: recording the same outcome twice is a no-op (the
    /// second attempt will not create a duplicate row). This is critical for
    /// restart safety: if a crash occurs between submitting a transaction and
    /// persisting the outcome, a restart can safely re-attempt the persistence
    /// without corrupting the state.
    pub async fn record_outcome(&self, outcome: &TaskOutcome) -> Result<()> {
        let now = Utc::now().timestamp();
        let outcome_str = serde_json::to_string(&outcome.outcome_json)
            .context("serializing outcome JSON")?;

        sqlx::query(
            "INSERT INTO task_outcomes (task_id, status, action_timestamp, outcome_json, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(task_id) DO UPDATE SET
                status = excluded.status,
                action_timestamp = excluded.action_timestamp,
                outcome_json = excluded.outcome_json,
                updated_at = excluded.updated_at",
        )
        .bind(outcome.task_id as i64)
        .bind(outcome.status.as_str())
        .bind(outcome.action_timestamp)
        .bind(&outcome_str)
        .bind(now)
        .execute(&self.pool)
        .await
        .with_context(|| format!("recording outcome for task {}", outcome.task_id))?;

        Ok(())
    }

    /// Retrieve a recorded outcome for a task, if one exists.
    pub async fn get_outcome(&self, task_id: u32) -> Result<Option<TaskOutcome>> {
        let row = sqlx::query(
            "SELECT task_id, status, action_timestamp, outcome_json
             FROM task_outcomes
             WHERE task_id = ?",
        )
        .bind(task_id as i64)
        .fetch_optional(&self.pool)
        .await
        .context("fetching task outcome")?;

        Ok(row.map(|r| {
            let status_str: String = r.get("status");
            TaskOutcome {
                task_id: r.get::<i64, _>("task_id") as u32,
                status: TaskStatus::from_str(&status_str).unwrap_or(TaskStatus::Expired),
                action_timestamp: r.get("action_timestamp"),
                outcome_json: r
                    .get::<String, _>("outcome_json")
                    .parse()
                    .unwrap_or(json!({})),
            }
        }))
    }

    /// Get all task ids with a given status.
    pub async fn get_tasks_by_status(&self, status: TaskStatus) -> Result<Vec<u32>> {
        let rows = sqlx::query(
            "SELECT task_id FROM task_outcomes WHERE status = ? ORDER BY task_id ASC",
        )
        .bind(status.as_str())
        .fetch_all(&self.pool)
        .await
        .context("fetching tasks by status")?;

        Ok(rows
            .into_iter()
            .map(|r| r.get::<i64, _>("task_id") as u32)
            .collect())
    }

    /// Get all tasks that have been processed (have any outcome recorded).
    pub async fn get_all_processed_tasks(&self) -> Result<Vec<TaskOutcome>> {
        let rows = sqlx::query(
            "SELECT task_id, status, action_timestamp, outcome_json
             FROM task_outcomes
             ORDER BY task_id ASC",
        )
        .fetch_all(&self.pool)
        .await
        .context("fetching all processed tasks")?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let status_str: String = r.get("status");
                TaskOutcome {
                    task_id: r.get::<i64, _>("task_id") as u32,
                    status: TaskStatus::from_str(&status_str).unwrap_or(TaskStatus::Expired),
                    action_timestamp: r.get("action_timestamp"),
                    outcome_json: r
                        .get::<String, _>("outcome_json")
                        .parse()
                        .unwrap_or(json!({})),
                }
            })
            .collect())
    }

    /// Record the startup checkpoint: the last ledger fully processed and
    /// whether startup load completed.
    pub async fn record_startup_checkpoint(
        &self,
        last_processed_ledger: u32,
        startup_complete: bool,
    ) -> Result<()> {
        let now = Utc::now().timestamp();

        sqlx::query(
            "INSERT INTO startup_checkpoint (id, last_processed_ledger, startup_complete, updated_at)
             VALUES (1, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                last_processed_ledger = excluded.last_processed_ledger,
                startup_complete = excluded.startup_complete,
                updated_at = excluded.updated_at",
        )
        .bind(last_processed_ledger as i64)
        .bind(startup_complete as i64)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("recording startup checkpoint")?;

        Ok(())
    }

    /// Retrieve the startup checkpoint.
    pub async fn get_startup_checkpoint(&self) -> Result<Option<(u32, bool)>> {
        let row = sqlx::query(
            "SELECT last_processed_ledger, startup_complete FROM startup_checkpoint WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .context("fetching startup checkpoint")?;

        Ok(row.map(|r| {
            (
                r.get::<i64, _>("last_processed_ledger") as u32,
                r.get::<i64, _>("startup_complete") != 0,
            )
        }))
    }

    /// Clear all state (for testing and fresh starts).
    pub async fn clear(&self) -> Result<()> {
        sqlx::query("DELETE FROM task_outcomes")
            .execute(&self.pool)
            .await
            .context("clearing task outcomes")?;

        sqlx::query("DELETE FROM startup_checkpoint")
            .execute(&self.pool)
            .await
            .context("clearing startup checkpoint")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn create_test_store() -> (Store, TempDir) {
        let tmpdir = TempDir::new().expect("create temp dir");
        let db_path = tmpdir.path().join("test.db");
        let url = format!("sqlite://{}", db_path.display());
        let store = Store::connect(&url)
            .await
            .expect("connect to test store");
        (store, tmpdir)
    }

    #[tokio::test]
    async fn test_record_and_retrieve_outcome() {
        let (store, _tmpdir) = create_test_store().await;

        let outcome = TaskOutcome::new(1, TaskStatus::Claimed, json!({"claimed_at": 100}));
        store.record_outcome(&outcome).await.expect("record");

        let retrieved = store
            .get_outcome(1)
            .await
            .expect("get")
            .expect("outcome exists");
        assert_eq!(retrieved.task_id, 1);
        assert_eq!(retrieved.status, TaskStatus::Claimed);
    }

    #[tokio::test]
    async fn test_idempotent_recording() {
        let (store, _tmpdir) = create_test_store().await;

        let outcome1 = TaskOutcome::new(1, TaskStatus::Claimed, json!({"claimed_at": 100}));
        store.record_outcome(&outcome1).await.expect("first record");

        let outcome2 = TaskOutcome::new(1, TaskStatus::Claimed, json!({"claimed_at": 100}));
        store.record_outcome(&outcome2).await.expect("second record");

        let all = store.get_all_processed_tasks().await.expect("get all");
        assert_eq!(all.len(), 1, "should have exactly one task");
    }

    #[tokio::test]
    async fn test_upsert_updates_status() {
        let (store, _tmpdir) = create_test_store().await;

        let outcome1 = TaskOutcome::new(1, TaskStatus::Claimed, json!({"claimed_at": 100}));
        store.record_outcome(&outcome1).await.expect("first record");

        let outcome2 = TaskOutcome::new(1, TaskStatus::Executed, json!({"executed_at": 101}));
        store.record_outcome(&outcome2).await.expect("second record");

        let retrieved = store
            .get_outcome(1)
            .await
            .expect("get")
            .expect("outcome exists");
        assert_eq!(retrieved.status, TaskStatus::Executed);
    }

    #[tokio::test]
    async fn test_get_tasks_by_status() {
        let (store, _tmpdir) = create_test_store().await;

        store
            .record_outcome(&TaskOutcome::new(1, TaskStatus::Claimed, json!({})))
            .await
            .expect("record 1");
        store
            .record_outcome(&TaskOutcome::new(2, TaskStatus::Executed, json!({})))
            .await
            .expect("record 2");
        store
            .record_outcome(&TaskOutcome::new(3, TaskStatus::Executed, json!({})))
            .await
            .expect("record 3");
        store
            .record_outcome(&TaskOutcome::new(4, TaskStatus::InProgress, json!({})))
            .await
            .expect("record 4");

        let claimed = store
            .get_tasks_by_status(TaskStatus::Claimed)
            .await
            .expect("get claimed");
        assert_eq!(claimed, vec![1]);

        let executed = store
            .get_tasks_by_status(TaskStatus::Executed)
            .await
            .expect("get executed");
        assert_eq!(executed, vec![2, 3]);

        let in_progress = store
            .get_tasks_by_status(TaskStatus::InProgress)
            .await
            .expect("get in_progress");
        assert_eq!(in_progress, vec![4]);
    }

    #[tokio::test]
    async fn test_startup_checkpoint() {
        let (store, _tmpdir) = create_test_store().await;

        assert_eq!(
            store.get_startup_checkpoint().await.expect("get"),
            None,
            "no checkpoint initially"
        );

        store
            .record_startup_checkpoint(12345, false)
            .await
            .expect("record");

        let (ledger, complete) = store
            .get_startup_checkpoint()
            .await
            .expect("get")
            .expect("checkpoint exists");
        assert_eq!(ledger, 12345);
        assert!(!complete);

        store
            .record_startup_checkpoint(12346, true)
            .await
            .expect("record again");

        let (ledger, complete) = store
            .get_startup_checkpoint()
            .await
            .expect("get")
            .expect("checkpoint updated");
        assert_eq!(ledger, 12346);
        assert!(complete);
    }

    #[tokio::test]
    async fn test_restart_simulation_preserves_state() {
        let tmpdir = TempDir::new().expect("create temp dir");
        let db_path = tmpdir.path().join("test.db");
        let url = format!("sqlite://{}", db_path.display());

        // First "run": record some outcomes
        {
            let store = Store::connect(&url)
                .await
                .expect("connect first time");
            store
                .record_outcome(&TaskOutcome::new(1, TaskStatus::Claimed, json!({})
                ))
                .await
                .expect("record task 1");
            store
                .record_outcome(&TaskOutcome::new(2, TaskStatus::Executed, json!({})
                ))
                .await
                .expect("record task 2");
            store
                .record_startup_checkpoint(100, true)
                .await
                .expect("record checkpoint");
        }

        // "Restart": reconnect and verify state persisted
        {
            let store = Store::connect(&url)
                .await
                .expect("connect second time");
            let tasks = store
                .get_all_processed_tasks()
                .await
                .expect("get all tasks");
            assert_eq!(tasks.len(), 2);
            assert_eq!(tasks[0].task_id, 1);
            assert_eq!(tasks[0].status, TaskStatus::Claimed);
            assert_eq!(tasks[1].task_id, 2);
            assert_eq!(tasks[1].status, TaskStatus::Executed);

            let (ledger, complete) = store
                .get_startup_checkpoint()
                .await
                .expect("get")
                .expect("checkpoint exists");
            assert_eq!(ledger, 100);
            assert!(complete);
        }
    }

    #[tokio::test]
    async fn test_clear_all_state() {
        let (store, _tmpdir) = create_test_store().await;

        store
            .record_outcome(&TaskOutcome::new(1, TaskStatus::Claimed, json!({})))
            .await
            .expect("record");
        store
            .record_startup_checkpoint(100, true)
            .await
            .expect("checkpoint");

        store.clear().await.expect("clear");

        let tasks = store
            .get_all_processed_tasks()
            .await
            .expect("get all");
        assert!(tasks.is_empty());

        let checkpoint = store
            .get_startup_checkpoint()
            .await
            .expect("get checkpoint");
        assert_eq!(checkpoint, None);
    }
}
