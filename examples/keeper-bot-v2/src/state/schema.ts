/**
 * Persistent task state schema for keeper-bot-v2 (Issue #0252)
 *
 * Tracks task outcomes across process restarts to prevent double-claiming
 * and to maintain a durable record of what this keeper has done.
 *
 * On startup, the bot loads this state before its first round so a restart
 * does not attempt to re-claim or re-execute a task it already finished.
 */

import Database from "better-sqlite3";
import { join } from "path";
import { homedir } from "os";

/**
 * Outcome states for a task this keeper has interacted with.
 */
export enum TaskOutcome {
  // Task was successfully claimed by this keeper; awaiting execution result
  Claimed = "claimed",

  // Task execution completed successfully; reward has been claimed
  Executed = "executed",

  // Task deadline passed; escrow was refunded to owner (no reward)
  Expired = "expired",

  // Execution failed; task was left for another keeper
  Failed = "failed",

  // Task was skipped (unsupported executor, unprofitable, etc.)
  Skipped = "skipped",

  // Claimed-in-progress: atomically marked before RPC call to prevent intra-process races
  ClaimedInProgress = "claimed_in_progress",
}

/**
 * Schema version for migrations.
 * Increment when the schema changes; old databases will be migrated or replaced.
 */
const SCHEMA_VERSION = 1;

/**
 * A single row in the task_outcomes table.
 */
export interface TaskStateRecord {
  task_id: bigint;
  outcome: TaskOutcome;
  timestamp: number; // Unix seconds
  notes?: string; // Optional: reason for outcome (e.g., "unprofitable", "executor error")
}

/**
 * Manager for the persistent task state database.
 * Provides atomic operations for claim coordination.
 */
export class TaskStateManager {
  private db: Database.Database;

  constructor(dbPath?: string) {
    const resolvedPath = dbPath || this.getDefaultDbPath();
    this.db = new Database(resolvedPath);
    this.db.pragma("journal_mode = WAL");
    this.db.pragma("synchronous = NORMAL");
    this.initializeSchema();
  }

  /**
   * Get the default database path (~/.soroban-keeper/state.db).
   */
  private getDefaultDbPath(): string {
    return join(homedir(), ".soroban-keeper", "keeper-state.db");
  }

  /**
   * Initialize or migrate the database schema.
   */
  private initializeSchema(): void {
    // Check if schema version table exists
    const tables = this.db
      .prepare(
        `SELECT name FROM sqlite_master WHERE type='table' AND name='schema_version'`
      )
      .all();

    if (tables.length === 0) {
      // Fresh database: create schema
      this.db.exec(`
        CREATE TABLE schema_version (
          version INTEGER PRIMARY KEY,
          migrated_at INTEGER NOT NULL
        );

        CREATE TABLE task_outcomes (
          task_id INTEGER PRIMARY KEY,
          outcome TEXT NOT NULL,
          timestamp INTEGER NOT NULL,
          notes TEXT
        );

        CREATE INDEX idx_task_outcomes_timestamp ON task_outcomes(timestamp);
      `);
      this.db
        .prepare("INSERT INTO schema_version (version, migrated_at) VALUES (?, ?)")
        .run(SCHEMA_VERSION, Math.floor(Date.now() / 1000));
    } else {
      // Existing database: verify version and migrate if needed
      const { version } = this.db
        .prepare("SELECT version FROM schema_version LIMIT 1")
        .get() as { version: number };

      if (version < SCHEMA_VERSION) {
        this.migrate(version, SCHEMA_VERSION);
      }
    }
  }

  /**
   * Migrate schema from old version to new.
   * Reuse this approach from indexer issue #0232 pattern.
   */
  private migrate(fromVersion: number, toVersion: number): void {
    // No migrations defined yet for v1 -> v2 since we're starting at v1.
    // When schema changes, add migration steps here.
    if (fromVersion === 1 && toVersion > 1) {
      // Example: ALTER TABLE task_outcomes ADD COLUMN new_field TEXT;
      throw new Error(
        `Schema migration from v${fromVersion} to v${toVersion} not yet implemented`
      );
    }
  }

  /**
   * Record a task outcome. Overwrites any prior outcome for the same task.
   */
  public recordOutcome(
    taskId: bigint,
    outcome: TaskOutcome,
    notes?: string
  ): void {
    const timestamp = Math.floor(Date.now() / 1000);
    this.db
      .prepare(
        `
        INSERT INTO task_outcomes (task_id, outcome, timestamp, notes)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(task_id) DO UPDATE SET
          outcome = excluded.outcome,
          timestamp = excluded.timestamp,
          notes = excluded.notes
      `
      )
      .run(taskId, outcome, timestamp, notes || null);
  }

  /**
   * Get the outcome for a single task, or null if not yet recorded.
   */
  public getOutcome(taskId: bigint): TaskOutcome | null {
    const row = this.db
      .prepare("SELECT outcome FROM task_outcomes WHERE task_id = ?")
      .get(taskId) as { outcome: TaskOutcome } | undefined;
    return row?.outcome ?? null;
  }

  /**
   * Check if a task is already claimed or executed (i.e., should be skipped).
   * Returns true if the keeper has already interacted with this task in a terminal state.
   */
  public isTerminal(taskId: bigint): boolean {
    const outcome = this.getOutcome(taskId);
    return (
      outcome === TaskOutcome.Executed ||
      outcome === TaskOutcome.Expired ||
      outcome === TaskOutcome.Failed
    );
  }

  /**
   * Atomically mark a task as claimed-in-progress before submitting the RPC call.
   * This prevents intra-process races where two workers both see the task as free,
   * then both attempt to claim it.
   *
   * Returns true if the mark succeeded (this worker owns the claim attempt).
   * Returns false if another worker already marked it claimed-in-progress.
   */
  public markClaimedInProgress(taskId: bigint): boolean {
    const outcome = this.getOutcome(taskId);

    // If already terminal (executed, expired, failed), task is unavailable
    if (this.isTerminal(taskId)) {
      return false;
    }

    // If already claimed-in-progress by another worker, back off
    if (outcome === TaskOutcome.ClaimedInProgress) {
      return false;
    }

    // Atomically set to claimed-in-progress
    this.recordOutcome(taskId, TaskOutcome.ClaimedInProgress);
    return true;
  }

  /**
   * Get all tasks with a specific outcome (e.g., all claimed-in-progress tasks).
   * Useful for cleanup or monitoring.
   */
  public getTasksByOutcome(outcome: TaskOutcome): bigint[] {
    const rows = this.db
      .prepare("SELECT task_id FROM task_outcomes WHERE outcome = ? ORDER BY timestamp DESC")
      .all(outcome) as { task_id: bigint }[];
    return rows.map((r) => r.task_id);
  }

  /**
   * Get all outcomes for tasks processed in the last N seconds.
   * Useful for round reporting and metrics.
   */
  public getRecentOutcomes(
    sinceSeconds: number
  ): { taskId: bigint; outcome: TaskOutcome; timestamp: number; notes?: string }[] {
    const cutoff = Math.floor(Date.now() / 1000) - sinceSeconds;
    const rows = this.db
      .prepare(
        "SELECT task_id, outcome, timestamp, notes FROM task_outcomes WHERE timestamp > ? ORDER BY timestamp DESC"
      )
      .all(cutoff) as Array<{
        task_id: bigint;
        outcome: TaskOutcome;
        timestamp: number;
        notes?: string;
      }>;
    return rows.map((r) => ({
      taskId: r.task_id,
      outcome: r.outcome,
      timestamp: r.timestamp,
      notes: r.notes,
    }));
  }

  /**
   * Clean up old records older than the given timestamp (Unix seconds).
   * Useful for maintenance and to keep database size bounded.
   * In production, consider running this periodically (e.g., weekly).
   */
  public pruneOlderThan(cutoffSeconds: number): number {
    const result = this.db
      .prepare("DELETE FROM task_outcomes WHERE timestamp < ?")
      .run(cutoffSeconds) as { changes: number };
    return result.changes;
  }

  /**
   * Close the database connection gracefully.
   */
  public close(): void {
    this.db.close();
  }

  /**
   * Get database statistics (for monitoring/debugging).
   */
  public getStats(): {
    totalTasks: number;
    executed: number;
    expired: number;
    failed: number;
    skipped: number;
    claimedInProgress: number;
  } {
    const total = this.db
      .prepare("SELECT COUNT(*) as count FROM task_outcomes")
      .get() as { count: number };

    const executed = this.db
      .prepare("SELECT COUNT(*) as count FROM task_outcomes WHERE outcome = ?")
      .get(TaskOutcome.Executed) as { count: number };

    const expired = this.db
      .prepare("SELECT COUNT(*) as count FROM task_outcomes WHERE outcome = ?")
      .get(TaskOutcome.Expired) as { count: number };

    const failed = this.db
      .prepare("SELECT COUNT(*) as count FROM task_outcomes WHERE outcome = ?")
      .get(TaskOutcome.Failed) as { count: number };

    const skipped = this.db
      .prepare("SELECT COUNT(*) as count FROM task_outcomes WHERE outcome = ?")
      .get(TaskOutcome.Skipped) as { count: number };

    const claimedInProgress = this.db
      .prepare("SELECT COUNT(*) as count FROM task_outcomes WHERE outcome = ?")
      .get(TaskOutcome.ClaimedInProgress) as { count: number };

    return {
      totalTasks: total.count,
      executed: executed.count,
      expired: expired.count,
      failed: failed.count,
      skipped: skipped.count,
      claimedInProgress: claimedInProgress.count,
    };
  }
}
