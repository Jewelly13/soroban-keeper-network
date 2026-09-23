/**
 * Main keeper round loop with concurrent task processing (Issue #0253)
 *
 * Processes candidate tasks concurrently within a bounded worker pool,
 * using persistent state to prevent intra-process races.
 */

import { Keypair, nativeToScVal } from "@stellar/stellar-sdk";
import { KeeperRegistryClient } from "@soroban-keeper-network/sdk";
import { TaskStateManager, TaskOutcome } from "./state/schema.js";
import { Semaphore } from "./semaphore.js";
import type { KeeperConfig } from "./config.js";

/**
 * A task candidate fetched from the registry's pending events.
 */
export interface TaskCandidate {
  taskId: bigint;
  reward: bigint;
  deadline: number; // Unix seconds
}

/**
 * Result of processing a single task.
 */
export interface TaskResult {
  taskId: bigint;
  success: boolean;
  outcome: TaskOutcome;
  reason: string;
}

/**
 * Summary of a keeper round.
 */
export interface RoundSummary {
  startTime: Date;
  endTime: Date;
  tasksEvaluated: number;
  tasksClaimed: number;
  tasksExecuted: number;
  tasksFailed: number;
  tasksSkipped: number;
  errors: Error[];
}

/**
 * The main keeper loop that runs a single round of processing.
 * Processes tasks concurrently with strict bounds on concurrency.
 *
 * @param client - KeeperRegistryClient for contract interactions
 * @param keypair - Keeper's signing keypair
 * @param config - Validated keeper configuration
 * @param stateManager - Persistent state for task tracking
 * @param candidateTasks - Pre-fetched tasks to process this round
 * @returns Summary of the round
 */
export async function keeperRound(
  client: KeeperRegistryClient,
  keypair: Keypair,
  config: KeeperConfig,
  stateManager: TaskStateManager,
  candidateTasks: TaskCandidate[]
): Promise<RoundSummary> {
  const startTime = new Date();
  const summary: RoundSummary = {
    startTime,
    endTime: startTime,
    tasksEvaluated: 0,
    tasksClaimed: 0,
    tasksExecuted: 0,
    tasksFailed: 0,
    tasksSkipped: 0,
    errors: [],
  };

  try {
    const nowSeconds = Math.floor(Date.now() / 1000);
    console.log(`\nKeeper round at ${startTime.toISOString()}`);
    console.log(
      `  Found ${candidateTasks.length} pending task(s); max ${config.maxTasksPerRound} per round, ${config.maxConcurrentTasks} concurrent`
    );

    // Filter to tasks before deadline and up to the per-round ceiling
    const eligibleTasks = candidateTasks.filter((task) => task.deadline > nowSeconds);

    if (eligibleTasks.length > config.maxTasksPerRound) {
      eligibleTasks.length = config.maxTasksPerRound;
    }

    summary.tasksEvaluated = eligibleTasks.length;

    if (eligibleTasks.length === 0) {
      console.log("  No eligible tasks this round.");
      return summary;
    }

    // Process tasks concurrently with bounded concurrency
    const semaphore = new Semaphore(config.maxConcurrentTasks);

    const taskPromises = eligibleTasks.map(async (task) => {
      const release = await semaphore.acquire();
      try {
        return await processTaskConcurrently(
          task,
          client,
          keypair,
          config,
          stateManager,
          nowSeconds
        );
      } finally {
        release();
      }
    });

    const results = await Promise.all(taskPromises);

    // Aggregate results
    for (const result of results) {
      if (result.success) {
        if (result.outcome === TaskOutcome.Executed) {
          summary.tasksClaimed++;
          summary.tasksExecuted++;
        } else if (result.outcome === TaskOutcome.Expired) {
          summary.tasksClaimed++;
        }
      } else {
        if (result.outcome === TaskOutcome.Failed) {
          summary.tasksFailed++;
        } else {
          summary.tasksSkipped++;
        }
      }
      console.log(`  Task ${result.taskId}: ${result.reason}`);
    }

    // Withdraw accumulated rewards if above threshold
    try {
      const balance = await client.read("keeper_balance", [
        nativeToScVal(keypair.publicKey(), { type: "address" }),
      ]);
      const balanceBigInt = BigInt(balance || 0);
      console.log(`  Accumulated balance: ${balanceBigInt} stroops`);

      if (balanceBigInt >= config.withdrawThreshold) {
        console.log(`  Withdrawing ${balanceBigInt} stroops...`);
        await client.invoke("withdraw_rewards", [
          nativeToScVal(keypair.publicKey(), { type: "address" }),
        ]);
        console.log("  Withdrawal complete!");
      }
    } catch (err) {
      const errMsg = err instanceof Error ? err.message : String(err);
      console.warn(`  Balance check/withdrawal failed: ${errMsg}`);
      summary.errors.push(
        new Error(`Balance check failed: ${errMsg}`)
      );
    }
  } catch (err) {
    const errMsg = err instanceof Error ? err.message : String(err);
    console.error(`Keeper round error: ${errMsg}`);
    summary.errors.push(err instanceof Error ? err : new Error(String(err)));
  }

  summary.endTime = new Date();
  return summary;
}

/**
 * Process a single task concurrently, respecting the persistent state
 * for intra-process race prevention.
 *
 * This is where the core race-prevention logic lives: before making the RPC call,
 * we atomically mark the task claimed-in-progress in persistent state.
 */
async function processTaskConcurrently(
  task: TaskCandidate,
  client: KeeperRegistryClient,
  keypair: Keypair,
  config: KeeperConfig,
  stateManager: TaskStateManager,
  nowSeconds: number
): Promise<TaskResult> {
  const taskId = task.taskId;

  try {
    // Check if we've already processed this task
    const priorOutcome = stateManager.getOutcome(taskId);
    if (stateManager.isTerminal(taskId)) {
      return {
        taskId,
        success: false,
        outcome: priorOutcome || TaskOutcome.Skipped,
        reason: `already ${priorOutcome} in prior round`,
      };
    }

    // ─────────────────────────────────────────────────────────────────────
    // CORE RACE PREVENTION: Atomically mark claimed-in-progress
    // ─────────────────────────────────────────────────────────────────────
    // Before attempting the RPC claim call, we mark the task claimed-in-progress
    // in persistent state. If another concurrent worker in the same process also
    // tries to claim this task, it will see the in-progress marker and skip.
    //
    // This must happen atomically from the worker's perspective: if markClaimedInProgress
    // returns false, we abort without making the RPC call, preventing a wasted claim fee.
    const marked = stateManager.markClaimedInProgress(taskId);
    if (!marked) {
      return {
        taskId,
        success: false,
        outcome: TaskOutcome.Skipped,
        reason: `already claimed-in-progress by another worker`,
      };
    }

    // Attempt to claim the task
    try {
      console.log(`  Claiming task ${taskId}...`);
      await client.claimTask({
        keeper: keypair.publicKey(),
        taskId,
      });
      console.log(`  Task ${taskId} claimed successfully!`);

      // Record the successful claim
      stateManager.recordOutcome(taskId, TaskOutcome.Claimed);

      // Now attempt execution (simplified; real implementation would dispatch to executor)
      // For now, mark as executed for demonstration
      stateManager.recordOutcome(taskId, TaskOutcome.Executed, "execution simulated");

      return {
        taskId,
        success: true,
        outcome: TaskOutcome.Executed,
        reason: `executed and claimed reward`,
      };
    } catch (claimErr) {
      const errMsg = claimErr instanceof Error ? claimErr.message : String(claimErr);

      // Determine if this is a "normal" loss (already claimed by another keeper) or an error
      if (errMsg.toLowerCase().includes("already")) {
        stateManager.recordOutcome(taskId, TaskOutcome.Failed, "lost race to another keeper");
        return {
          taskId,
          success: false,
          outcome: TaskOutcome.Failed,
          reason: `lost claim race to another keeper`,
        };
      }

      // Transient error; leave as claimed-in-progress for retry
      console.warn(`  Task ${taskId} claim failed: ${errMsg}`);
      return {
        taskId,
        success: false,
        outcome: TaskOutcome.ClaimedInProgress,
        reason: `claim failed: ${errMsg}`,
      };
    }
  } catch (err) {
    const errMsg = err instanceof Error ? err.message : String(err);
    console.error(`  Task ${taskId} processing error: ${errMsg}`);
    stateManager.recordOutcome(taskId, TaskOutcome.Failed, errMsg);
    return {
      taskId,
      success: false,
      outcome: TaskOutcome.Failed,
      reason: `unexpected error: ${errMsg}`,
    };
  }
}
