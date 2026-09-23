-- Initial keeper-bot v2 persistent task state schema.
--
-- Tracks, per task id that this keeper has interacted with, what it has done
-- (claimed, executed, expired), when it happened, and the outcome.
--
-- Design mirrors the indexer's append-only event table pattern: task_outcomes
-- is authoritative, and any derived queries fold this history rather than
-- maintaining separate mutable state that could drift. This ensures a restart
-- never loses or corrupts the record of what this keeper has already done.

CREATE TABLE IF NOT EXISTS task_outcomes (
    -- Autoincremented row id for transaction idempotency (restart-safe upserts).
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    
    -- Task id this keeper has interacted with. Non-null for all rows.
    task_id             INTEGER NOT NULL UNIQUE,
    
    -- Status of the last action taken on this task by this keeper:
    --   - "claimed"      : Successfully locked the task with claim_task
    --   - "in_progress"  : Claimed and execution submission is in flight (issue #381)
    --   - "executed"     : Successfully executed and submitted execute_task
    --   - "expired"      : Task expired (deadline passed) or was already expired when checked
    -- 
    -- The "in_progress" state allows issue #381 (concurrent round processing) to detect
    -- when a prior claim is still being executed, preventing duplicate submission.
    status              TEXT NOT NULL,
    
    -- Unix timestamp (seconds since epoch) when this action was recorded.
    -- Used to track task aging, detect stale in-progress states on restart,
    -- and for observability/debugging.
    action_timestamp    INTEGER NOT NULL,
    
    -- Outcome details: JSON blob capturing the result of the action.
    -- Schema examples:
    --   - {"claimed_at": 1234567890}
    --   - {"executed_at": 1234567891, "proof": "..."}
    --   - {"expired_at": 1234567892}
    --
    -- Stored as JSON to permit future flexibility in recording per-action metadata
    -- (e.g., gas costs, fee received) without schema migrations.
    outcome_json        TEXT NOT NULL,
    
    -- Timestamp when this row was last updated (for restart idempotency).
    updated_at          INTEGER NOT NULL
);

-- Index on task_id for fast lookups (already UNIQUE but explicit for clarity).
CREATE INDEX IF NOT EXISTS idx_task_outcomes_task_id ON task_outcomes (task_id);

-- Index on status for queries like "find all executed tasks" or "find in-progress tasks".
CREATE INDEX IF NOT EXISTS idx_task_outcomes_status ON task_outcomes (status);

-- Index on action_timestamp for time-window queries (e.g., "what did we claim in the last hour").
CREATE INDEX IF NOT EXISTS idx_task_outcomes_timestamp ON task_outcomes (action_timestamp);

-- Startup progress tracking: stores the ledger height and cursor position from the
-- last successful ingestion round, so a restarted keeper can skip already-processed
-- tasks and avoid duplicate attempts.
--
-- This table mirrors the indexer's pattern (see indexer/migrations/0001_initial.sql).
CREATE TABLE IF NOT EXISTS startup_checkpoint (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    -- Last successfully processed ledger; tasks at or before this were loaded into memory
    last_processed_ledger INTEGER NOT NULL DEFAULT 0,
    -- Whether initial startup load completed (after this is true, every new round
    -- only needs to load deltas, not replay all history)
    startup_complete    INTEGER NOT NULL DEFAULT 0,
    -- Timestamp when this checkpoint was last updated
    updated_at          INTEGER NOT NULL
);
