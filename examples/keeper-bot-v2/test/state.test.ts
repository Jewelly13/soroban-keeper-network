/**
 * Tests for the persistent task state schema (Issue #0252)
 */

import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { TaskStateManager, TaskOutcome } from "../src/state/schema.js";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

describe("TaskStateManager", () => {
  let tmpDir: string;
  let manager: TaskStateManager;

  beforeEach(() => {
    tmpDir = mkdtempSync(join(tmpdir(), "keeper-test-"));
    const dbPath = join(tmpDir, "test.db");
    manager = new TaskStateManager(dbPath);
  });

  afterEach(() => {
    manager.close();
    rmSync(tmpDir, { recursive: true, force: true });
  });

  describe("recordOutcome and getOutcome", () => {
    it("records and retrieves a task outcome", () => {
      const taskId = 123n;
      manager.recordOutcome(taskId, TaskOutcome.Executed, "test execution");

      const outcome = manager.getOutcome(taskId);
      expect(outcome).toBe(TaskOutcome.Executed);
    });

    it("returns null for unknown tasks", () => {
      const outcome = manager.getOutcome(999n);
      expect(outcome).toBeNull();
    });

    it("overwrites prior outcomes", () => {
      const taskId = 456n;
      manager.recordOutcome(taskId, TaskOutcome.Claimed);
      expect(manager.getOutcome(taskId)).toBe(TaskOutcome.Claimed);

      manager.recordOutcome(taskId, TaskOutcome.Executed);
      expect(manager.getOutcome(taskId)).toBe(TaskOutcome.Executed);
    });
  });

  describe("isTerminal", () => {
    it("returns true for executed tasks", () => {
      manager.recordOutcome(1n, TaskOutcome.Executed);
      expect(manager.isTerminal(1n)).toBe(true);
    });

    it("returns true for expired tasks", () => {
      manager.recordOutcome(2n, TaskOutcome.Expired);
      expect(manager.isTerminal(2n)).toBe(true);
    });

    it("returns true for failed tasks", () => {
      manager.recordOutcome(3n, TaskOutcome.Failed);
      expect(manager.isTerminal(3n)).toBe(true);
    });

    it("returns false for claimed-in-progress tasks", () => {
      manager.recordOutcome(4n, TaskOutcome.ClaimedInProgress);
      expect(manager.isTerminal(4n)).toBe(false);
    });

    it("returns false for skipped tasks", () => {
      manager.recordOutcome(5n, TaskOutcome.Skipped);
      expect(manager.isTerminal(5n)).toBe(false);
    });

    it("returns false for unknown tasks", () => {
      expect(manager.isTerminal(999n)).toBe(false);
    });
  });

  describe("markClaimedInProgress - intra-process race prevention", () => {
    it("marks an unknown task as claimed-in-progress and returns true", () => {
      const taskId = 100n;
      const result = manager.markClaimedInProgress(taskId);
      expect(result).toBe(true);
      expect(manager.getOutcome(taskId)).toBe(TaskOutcome.ClaimedInProgress);
    });

    it("returns false if task is already claimed-in-progress (race detected)", () => {
      const taskId = 101n;
      const first = manager.markClaimedInProgress(taskId);
      const second = manager.markClaimedInProgress(taskId);

      expect(first).toBe(true);
      expect(second).toBe(false); // Second worker backed off
    });

    it("returns false if task is already terminal (executed)", () => {
      const taskId = 102n;
      manager.recordOutcome(taskId, TaskOutcome.Executed);

      const result = manager.markClaimedInProgress(taskId);
      expect(result).toBe(false);
    });

    it("returns false if task is already terminal (expired)", () => {
      const taskId = 103n;
      manager.recordOutcome(taskId, TaskOutcome.Expired);

      const result = manager.markClaimedInProgress(taskId);
      expect(result).toBe(false);
    });

    it("returns false if task is already terminal (failed)", () => {
      const taskId = 104n;
      manager.recordOutcome(taskId, TaskOutcome.Failed);

      const result = manager.markClaimedInProgress(taskId);
      expect(result).toBe(false);
    });

    /**
     * Simulates the intra-process race scenario described in the issue:
     * Two workers evaluate the same task, both see it as free, but only
     * one successfully marks it claimed-in-progress. The second sees
     * the in-progress marker and skips.
     */
    it("prevents two workers racing for the same task (core acceptance criterion)", () => {
      const taskId = 105n;

      // Worker 1 checks outcome (nil), marks claimed-in-progress, proceeds to RPC claim
      const worker1Marked = manager.markClaimedInProgress(taskId);
      expect(worker1Marked).toBe(true);

      // Worker 2 checks outcome after worker 1 marked it claimed-in-progress
      const worker2Marked = manager.markClaimedInProgress(taskId);
      expect(worker2Marked).toBe(false); // Worker 2 skips this task

      // Task state reflects only worker 1's claim attempt
      expect(manager.getOutcome(taskId)).toBe(TaskOutcome.ClaimedInProgress);
    });
  });

  describe("getTasksByOutcome", () => {
    it("returns tasks with a specific outcome", () => {
      manager.recordOutcome(1n, TaskOutcome.Executed);
      manager.recordOutcome(2n, TaskOutcome.Executed);
      manager.recordOutcome(3n, TaskOutcome.Failed);

      const executed = manager.getTasksByOutcome(TaskOutcome.Executed);
      expect(executed).toHaveLength(2);
      expect(executed).toContain(1n);
      expect(executed).toContain(2n);

      const failed = manager.getTasksByOutcome(TaskOutcome.Failed);
      expect(failed).toHaveLength(1);
      expect(failed).toContain(3n);
    });

    it("returns empty array for outcomes with no tasks", () => {
      manager.recordOutcome(1n, TaskOutcome.Executed);

      const skipped = manager.getTasksByOutcome(TaskOutcome.Skipped);
      expect(skipped).toEqual([]);
    });
  });

  describe("getRecentOutcomes", () => {
    it("returns tasks recorded within the time window", () => {
      manager.recordOutcome(1n, TaskOutcome.Executed);
      manager.recordOutcome(2n, TaskOutcome.Executed);

      const recent = manager.getRecentOutcomes(60); // Last 60 seconds
      expect(recent).toHaveLength(2);
      expect(recent[0].taskId).toBe(2n); // Most recent first
      expect(recent[1].taskId).toBe(1n);
    });

    it("returns empty array if no tasks in time window", () => {
      manager.recordOutcome(1n, TaskOutcome.Executed);
      const old = manager.getRecentOutcomes(1); // Last 1 second (likely none)
      expect(old).toHaveLength(0);
    });
  });

  describe("pruneOlderThan", () => {
    it("deletes outcomes older than the cutoff timestamp", () => {
      manager.recordOutcome(1n, TaskOutcome.Executed);
      manager.recordOutcome(2n, TaskOutcome.Executed);

      // Prune everything (current time in future)
      const nowSeconds = Math.floor(Date.now() / 1000);
      const futureSeconds = nowSeconds + 3600; // 1 hour in future

      const deleted = manager.pruneOlderThan(futureSeconds);
      expect(deleted).toBe(2);
      expect(manager.getOutcome(1n)).toBeNull();
      expect(manager.getOutcome(2n)).toBeNull();
    });

    it("preserves recent records", () => {
      manager.recordOutcome(1n, TaskOutcome.Executed);
      const nowSeconds = Math.floor(Date.now() / 1000);

      // Prune only very old records (before now)
      const deleted = manager.pruneOlderThan(nowSeconds - 60);
      expect(deleted).toBe(0); // Record was made just now, not old
      expect(manager.getOutcome(1n)).toBe(TaskOutcome.Executed);
    });
  });

  describe("getStats", () => {
    it("returns aggregated statistics", () => {
      manager.recordOutcome(1n, TaskOutcome.Executed);
      manager.recordOutcome(2n, TaskOutcome.Executed);
      manager.recordOutcome(3n, TaskOutcome.Expired);
      manager.recordOutcome(4n, TaskOutcome.Failed);
      manager.recordOutcome(5n, TaskOutcome.Skipped);
      manager.recordOutcome(6n, TaskOutcome.ClaimedInProgress);

      const stats = manager.getStats();
      expect(stats.totalTasks).toBe(6);
      expect(stats.executed).toBe(2);
      expect(stats.expired).toBe(1);
      expect(stats.failed).toBe(1);
      expect(stats.skipped).toBe(1);
      expect(stats.claimedInProgress).toBe(1);
    });
  });

  describe("persistence across restarts", () => {
    it("survives a process restart and reloads state", () => {
      const dbPath = join(tmpDir, "persistent.db");

      // First process: record some outcomes
      let manager1 = new TaskStateManager(dbPath);
      manager1.recordOutcome(1n, TaskOutcome.Executed);
      manager1.recordOutcome(2n, TaskOutcome.Failed);
      manager1.close();

      // Simulated restart: new process loads from same database
      let manager2 = new TaskStateManager(dbPath);
      expect(manager2.getOutcome(1n)).toBe(TaskOutcome.Executed);
      expect(manager2.getOutcome(2n)).toBe(TaskOutcome.Failed);
      manager2.close();
    });
  });
});
