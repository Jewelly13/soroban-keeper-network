/**
 * Soroban Keeper Network — Example Keeper Bot
 *
 * This off-chain bot:
 *   1. Polls the Soroban RPC for TaskRegistered / TaskClaimed events emitted
 *      by the KeeperRegistry contract.
 *   2. For each Pending task whose deadline has not passed:
 *      a. Calls `claim_task` to lock the task.
 *      b. Executes the underlying operation off-chain (simulated here).
 *      c. Calls `execute_task` with a proof to claim the reward.
 *   3. Periodically calls `withdraw_rewards` to pull accumulated XLM.
 *
 * Usage (daemon mode):
 *   cp .env.example .env
 *   # Fill in your secret key and contract address
 *   npm install
 *   node index.js
 *
 * Usage (one-shot mode for cron or serverless):
 *   node index.js --once
 *   # or: RUN_ONCE=true node index.js
 *
 * This example already includes:
 *   - Comprehensive startup validation for all config settings.
 *   - Retry with exponential back-off + jitter on transient RPC errors.
 *   - Graceful shutdown (SIGINT/SIGTERM) that drains the in-flight round.
 *   - Permissionless expiry of stale tasks to refund owners.
 *
 * Production keepers should additionally add:
 *   - Persistent task state DB (SQLite / Redis) to avoid double-claiming.
 *     This example only keeps a bounded in-memory outcome cache (see
 *     `taskOutcomes` below) that is entirely lost on every restart — a real
 *     DB is meant to be a drop-in replacement for that Map, not a rewrite.
 *   - MEV-aware submission (bundle multiple tasks)
 *   - Prometheus metrics endpoint
 *   - Alerting (PagerDuty / Telegram) on missed executions
 */

"use strict";

require("dotenv").config();

const {
  Keypair,
  SorobanRpc,
  TransactionBuilder,
  Networks,
  BASE_FEE,
  nativeToScVal,
  scValToNative,
  Contract,
  StrKey,
} = require("@stellar/stellar-sdk");

// ─────────────────────────────────────────────────────────────────────────────
// Configuration — set via environment variables or .env file
// ─────────────────────────────────────────────────────────────────────────────

const NETWORK_CONFIG = {
  testnet: {
    rpcUrl: "https://soroban-testnet.stellar.org",
    networkPassphrase: Networks.TESTNET,
  },
  futurenet: {
    rpcUrl: "https://rpc-futurenet.stellar.org",
    networkPassphrase: Networks.FUTURENET,
  },
  mainnet: {
    rpcUrl: "https://mainnet.sorobanrpc.com",
    networkPassphrase: Networks.PUBLIC,
  },
};

let CONFIG; // Initialized in main() after validation

// ─────────────────────────────────────────────────────────────────────────────
// Cross-round state
// ─────────────────────────────────────────────────────────────────────────────

// Ledger to resume the event scan from on the next round. Starts out `null`,
// which tells the first round to fall back to the ~1000-ledger lookback
// window (see keeperLoop). Every round after that advances this cursor using
// the `latestLedger` reported by the getEvents RPC response itself, so each
// round only scans the ledgers that closed since the previous round instead
// of re-reading the same ~998 ledgers every time.
let cursorLedger = null;

// In-memory cache of taskId -> terminal outcome this bot has itself caused
// ('executed' via execute_task, 'expired' via expire_task), so a task the
// cursor re-surfaces (or that falls inside the very first lookback window
// more than once) isn't re-submitted as a fresh claim/execute attempt.
//
// Eviction policy: an entry is removed once its task's `deadline` has
// passed, because a task past its deadline can never be claimed or executed
// again regardless of what this cache remembers — so the map is naturally
// bounded by the number of tasks with a still-live deadline, not by time or
// an arbitrary size cap. Eviction runs at the top of every round.
//
// This cache is entirely in-memory and is lost on process restart — that is
// an accepted limitation for this example bot (see the header comment).
const taskOutcomes = new Map(); // taskId -> { outcome: "executed" | "expired", deadline: number }

// ─────────────────────────────────────────────────────────────────────────────
// Configuration validation
// ─────────────────────────────────────────────────────────────────────────────

function fail(name, value, reason) {
  let message = `❌  Invalid ${name}`;
  if (value) {
    message += `: ${value}`;
  }
  console.error(`${message} — ${reason}`);
  process.exit(1);
}

function requireEnv(name, { parse, validate, secret = false, fallback }) {
  const raw = process.env[name];
  if (raw === undefined || raw === "") {
    if (fallback !== undefined) {
      return fallback;
    }
    fail(name, raw, "must be set");
  }
  try {
    const parsed = parse ? parse(raw) : raw;
    if (validate && !validate.fn(parsed)) {
      fail(name, secret ? null : raw, validate.reason);
    }
    return parsed;
  } catch (e) {
    fail(name, secret ? null : raw, e.message);
  }
}

async function validateAndLoadConfig() {
  const network = requireEnv("NETWORK", {
    validate: {
      fn: (v) => Object.keys(NETWORK_CONFIG).includes(v),
      reason: `must be one of: ${Object.keys(NETWORK_CONFIG).join(", ")}`,
    },
    fallback: "testnet",
  });

  const registryContractId = requireEnv("REGISTRY_CONTRACT_ID", {
    validate: {
      fn: StrKey.isValidContract,
      reason: "must be a valid contract ID (starts with C...)",
    },
  });

  const secretKey = requireEnv("KEEPER_SECRET_KEY", {
    secret: true,
    validate: {
      fn: StrKey.isValidEd25519SecretSeed,
      reason: "must be a valid secret key (starts with S...)",
    },
  });

  // After validating the required string values, we can create the server
  // connection and use it to validate the contract's existence on the network.
  const { rpcUrl } = NETWORK_CONFIG[network];
  const server = new SorobanRpc.Server(rpcUrl, { allowHttp: false });

  try {
    await server.getContractData(registryContractId);
  } catch (e) {
    if (e.response && e.response.status === 404) {
      fail(
        "REGISTRY_CONTRACT_ID",
        registryContractId,
        `not found on network ${network}. Please check the contract ID and NETWORK settings.`
      );
    }
    // For other errors, we'll let the main connectivity check handle it.
  }

  // Now that all critical configs are validated, build the final CONFIG object.
  CONFIG = {
    network,
    registryContractId,
    secretKey,
    once: process.argv.includes("--once") || process.env.RUN_ONCE === "true",
    pollIntervalMs: requireEnv("POLL_INTERVAL_MS", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => v >= 1000, reason: "must be >= 1000" },
      fallback: 10000,
    }),
    withdrawThreshold: requireEnv("WITHDRAW_THRESHOLD", {
      parse: BigInt,
      validate: { fn: (v) => v >= 0, reason: "must be a positive number" },
      fallback: 10000000n,
    }),
    maxTasksPerRound: requireEnv("MAX_TASKS_PER_ROUND", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => v >= 1, reason: "must be >= 1" },
      fallback: 5,
    }),
    maxRetries: requireEnv("MAX_RETRIES", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => v >= 0, reason: "must be >= 0" },
      fallback: 3,
    }),
    retryBaseMs: requireEnv("RETRY_BASE_MS", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => v > 0, reason: "must be > 0" },
      fallback: 500,
    }),
    expireStaleTasks: requireEnv("EXPIRE_STALE_TASKS", {
      parse: (v) => v.toLowerCase() === "true",
      fallback: true,
    }),
  };
}

// ─────────────────────────────────────────────────────────────────────────────
// Reliability helpers
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Retries an async operation with exponential back-off and jitter.
 *
 * Only transient failures (RPC timeouts, network blips, transaction not-yet-
 * confirmed) should be retried. Deterministic contract errors — e.g. a task
 * already claimed by another keeper — are surfaced immediately so we don't
 * waste fees resubmitting a call that can never succeed.
 */
async function withRetry(label, fn) {
  let lastErr;
  for (let attempt = 0; attempt <= CONFIG.maxRetries; attempt++) {
    try {
      return await fn();
    } catch (err) {
      lastErr = err;
      if (isPermanentError(err) || attempt === CONFIG.maxRetries) {
        throw err;
      }
      const backoff = CONFIG.retryBaseMs * 2 ** attempt;
      const jitter = Math.floor(Math.random() * CONFIG.retryBaseMs);
      const delay = backoff + jitter;
      console.warn(`  ↻  ${label} failed (attempt ${attempt + 1}), retrying in ${delay}ms: ${err.message}`);
      await sleep(delay);
    }
  }
  throw lastErr;
}

/**
 * Heuristic: contract-level business errors are permanent for this bot and must
 * not be retried, whereas transport/consensus errors are worth another attempt.
 */
function isPermanentError(err) {
  const msg = (err && err.message ? err.message : "").toLowerCase();
  return (
    msg.includes("simulation failed") || // contract returned an Err()
    msg.includes("invalidaction") ||
    msg.includes("unauthorized") ||
    msg.includes("already")
  );
}

// ─────────────────────────────────────────────────────────────────────────────
// Soroban helpers
// ─────────────────────────────────────────────────────────────────────────────

async function simulateAndSend(server, keypair, networkPassphrase, tx) {
  const simResponse = await server.simulateTransaction(tx);
  if (SorobanRpc.Api.isSimulationError(simResponse)) {
    throw new Error(`Simulation failed: ${simResponse.error}`);
  }

  const preparedTx = SorobanRpc.assembleTransaction(tx, simResponse).build();
  preparedTx.sign(keypair);

  const sendResponse = await server.sendTransaction(preparedTx);
  if (sendResponse.status === "ERROR") {
    throw new Error(`Send failed: ${JSON.stringify(sendResponse.errorResult)}`);
  }

  // Poll for confirmation
  let getResponse = await server.getTransaction(sendResponse.hash);
  let attempts = 0;
  while (getResponse.status === SorobanRpc.Api.GetTransactionStatus.NOT_FOUND && attempts < 30) {
    await sleep(2000);
    getResponse = await server.getTransaction(sendResponse.hash);
    attempts++;
  }

  if (getResponse.status === SorobanRpc.Api.GetTransactionStatus.SUCCESS) {
    return getResponse;
  } else {
    throw new Error(`Transaction failed with status: ${getResponse.status}`);
  }
}

async function invokeContract(server, keypair, networkPassphrase, contractId, method, args) {
  const account = await server.getAccount(keypair.publicKey());
  const contract = new Contract(contractId);

  const tx = new TransactionBuilder(account, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(contract.call(method, ...args))
    .setTimeout(30)
    .build();

  return simulateAndSend(server, keypair, networkPassphrase, tx);
}

// ─────────────────────────────────────────────────────────────────────────────
// Task fetching — reads pending tasks by querying events
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Returns `{ tasks, latestLedger }`. `latestLedger` is the ledger the RPC
 * node had most recently ingested at query time (straight from the
 * getEvents response) so callers can advance a scan cursor without an extra
 * getLatestLedger() round-trip. It is `null` if the query failed, so callers
 * know not to advance their cursor past ledgers that were never actually
 * scanned.
 */
async function fetchPendingTasks(server, contractId, startLedger) {
  const tasks = [];
  let latestLedger = null;
  try {
    // Query TaskRegistered events (topic: ["reg", "task"])
    const response = await server.getEvents({
      startLedger,
      filters: [
        {
          type: "contract",
          contractIds: [contractId],
          topics: [
            ["AAAADwAAAANyZWc=", "AAAADwAAAAR0YXNr"], // "reg", "task" as base64 XDR
          ],
        },
      ],
      limit: 100,
    });

    latestLedger = response.latestLedger;

    for (const event of response.events || []) {
      try {
        const [taskIdVal, , rewardVal, deadlineVal] = event.value.value();
        const taskId = scValToNative(taskIdVal);
        const reward = scValToNative(rewardVal);
        const deadline = scValToNative(deadlineVal);

        tasks.push({ taskId, reward, deadline });
      } catch (e) {
        // Skip malformed events
      }
    }
  } catch (e) {
    console.warn("⚠️  Failed to fetch events:", e.message);
  }
  return { tasks, latestLedger };
}

// ─────────────────────────────────────────────────────────────────────────────
// Keeper logic — off-chain execution simulation
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Simulates off-chain execution of the task (liquidation, oracle push, etc.)
 * In a real keeper this would:
 *   - Call the target protocol contract
 *   - Verify the action succeeded
 *   - Return the tx hash or state proof
 */
async function executeTaskOffChain(task) {
  console.log(`  ⚙️  Executing task ${task.taskId} off-chain...`);
  // Simulate network latency
  await sleep(500);

  // Return a fake "proof" — in production this is the target tx hash
  const fakeTxHash = Buffer.from(
    `keeper-proof:task:${task.taskId}:ts:${Date.now()}`
  ).toString("hex");
  return fakeTxHash;
}

// ─────────────────────────────────────────────────────────────────────────────
// Main keeper loop
// ─────────────────────────────────────────────────────────────────────────────

async function keeperLoop(server, keypair, networkPassphrase, contractId) {
  // A round is successful if it runs to completion without any unhandled
  // exceptions. An RPC error that cannot be resolved with retries, or any
  // other unexpected error, is a failure.
  // Note: a round that finds no tasks is a success. Losing a claim race to
  // another keeper is also a success, as this is normal competitive behaviour.
  const summary = { processed: 0, errors: [] };

  try {
    const nowSeconds = Math.floor(Date.now() / 1000);
    console.log(`\n🔄  Keeper round at ${new Date().toISOString()}`);

    // Evict outcome-cache entries whose deadline has passed — see the
    // `taskOutcomes` declaration for why this is a safe & sufficient policy.
    for (const [taskId, entry] of taskOutcomes) {
      if (entry.deadline <= nowSeconds) taskOutcomes.delete(taskId);
    }

    // Only the very first round (no cursor yet) uses a fixed lookback
    // window (last ~1000 ledgers ≈ 1.4h at 5s) so a freshly started bot
    // still picks up recently registered tasks. Every subsequent round
    // resumes exactly where the previous one left off.
    let startLedger;
    if (cursorLedger === null) {
      const latestLedger = await server.getLatestLedger();
      startLedger = Math.max(1, latestLedger.sequence - 1000);
    } else {
      startLedger = cursorLedger;
    }

    const { tasks: fetchedTasks, latestLedger: scannedLedger } = await fetchPendingTasks(
      server, contractId, startLedger
    );

    if (scannedLedger !== null) {
      console.log(
        `  📜  Scanned ledgers ${startLedger} to ${scannedLedger} (${scannedLedger - startLedger + 1} ledgers)`
      );
      // Resume from the next unscanned ledger next round. If the query
      // failed (scannedLedger === null), leave the cursor where it is so
      // the next round retries the same window instead of silently
      // skipping ledgers we never actually read.
      cursorLedger = scannedLedger + 1;
    }

    const pendingTasks = fetchedTasks.filter((task) => !taskOutcomes.has(task.taskId));
    const skipped = fetchedTasks.length - pendingTasks.length;
    console.log(
      `  📋  Found ${fetchedTasks.length} TaskRegistered event(s)` +
        (skipped > 0 ? `, ${skipped} already resolved this run (skipped)` : "") +
        ` — ${pendingTasks.length} to evaluate`
    );

    for (const task of pendingTasks) {
      if (summary.processed >= CONFIG.maxTasksPerRound) break;

      if (task.deadline <= nowSeconds) {
        if (CONFIG.expireStaleTasks) {
          try {
            await withRetry(`expire_task ${task.taskId}`, () =>
              invokeContract(server, keypair, networkPassphrase, contractId, "expire_task", [
                nativeToScVal(task.taskId, { type: "u64" }),
              ])
            );
            taskOutcomes.set(task.taskId, { outcome: "expired", deadline: task.deadline });
            console.log(`  ♻️  Task ${task.taskId} expired — escrow refunded to owner`);
          } catch (err) {
            console.log(`  ⏰  Task ${task.taskId} past deadline (skip: ${err.message})`);
          }
        } else {
          console.log(`  ⏰  Task ${task.taskId} is past deadline, skipping`);
        }
        continue;
      }

      try {
        console.log(`  📌  Attempting to claim task ${task.taskId} (reward: ${task.reward})...`);
        await withRetry(`claim_task ${task.taskId}`, () =>
          invokeContract(server, keypair, networkPassphrase, contractId, "claim_task", [
            nativeToScVal(keypair.publicKey(), { type: "address" }),
            nativeToScVal(task.taskId, { type: "u64" }),
          ])
        );
        console.log(`  ✅  Task ${task.taskId} claimed!`);

        const proof = await executeTaskOffChain(task);

        await withRetry(`execute_task ${task.taskId}`, () =>
          invokeContract(server, keypair, networkPassphrase, contractId, "execute_task", [
            nativeToScVal(keypair.publicKey(), { type: "address" }),
            nativeToScVal(task.taskId, { type: "u64" }),
            nativeToScVal(Buffer.from(proof, "hex"), { type: "bytes" }),
          ])
        );
        taskOutcomes.set(task.taskId, { outcome: "executed", deadline: task.deadline });
        console.log(`  💰  Task ${task.taskId} executed! Proof: ${proof.slice(0, 20)}...`);
        summary.processed++;
      } catch (err) {
        console.warn(`  ⚠️  Failed to process task ${task.taskId}: ${err.message}`);
        summary.errors.push(err);
      }
    }

    // Check and withdraw rewards
    const balanceResult = await invokeContract(
      server, keypair, networkPassphrase, contractId, "keeper_balance",
      [nativeToScVal(keypair.publicKey(), { type: "address" })]
    );
    if (balanceResult.returnValue) {
      const balance = BigInt(scValToNative(balanceResult.returnValue) || 0);
      console.log(`  💎  Accumulated reward balance: ${balance} stroops`);

      if (balance >= CONFIG.withdrawThreshold) {
        console.log(`  💸  Withdrawing ${balance} stroops...`);
        await invokeContract(server, keypair, networkPassphrase, contractId, "withdraw_rewards", [
          nativeToScVal(keypair.publicKey(), { type: "address" }),
        ]);
        console.log(`  ✅  Withdrawal complete!`);
      }
    }
  } catch (err) {
    console.error(`❌  Keeper loop error: ${err.message}`);
    summary.errors.push(err);
  }
  return summary;
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

async function main() {
  await validateAndLoadConfig();

  const { rpcUrl, networkPassphrase } = NETWORK_CONFIG[CONFIG.network];
  const keypair = Keypair.fromSecret(CONFIG.secretKey);
  const server = new SorobanRpc.Server(rpcUrl, { allowHttp: false });

  console.log("╔══════════════════════════════════════════════════════════════╗");
  console.log("║         Soroban Keeper Network — Keeper Bot v0.1.0          ║");
  console.log("╚══════════════════════════════════════════════════════════════╝");
  console.log(`  Network  : ${CONFIG.network}`);
  console.log(`  RPC URL  : ${rpcUrl}`);
  console.log(`  Keeper   : ${keypair.publicKey()}`);
  console.log(`  Registry : ${CONFIG.registryContractId}`);
  if (CONFIG.once) {
    console.log("  Mode     : --once (single run)");
  } else {
    console.log(`  Poll     : every ${CONFIG.pollIntervalMs / 1000}s`);
  }
  console.log(`  Withdraw : when balance ≥ ${CONFIG.withdrawThreshold} stroops`);
  console.log("");

  // Verify connectivity
  try {
    const health = await server.getHealth();
    console.log(`✅  RPC healthy — ledger ${health.ledger}`);
  } catch (e) {
    console.error(`❌  RPC unreachable at ${rpcUrl}: ${e.message}`);
    process.exit(1);
  }

  if (CONFIG.once) {
    const summary = await keeperLoop(server, keypair, networkPassphrase, CONFIG.registryContractId);
    const ok = summary.errors.length === 0;
    console.log(ok ? "✅  Round complete." : "⚠️  Round completed with errors.");
    process.exit(ok ? 0 : 1);
  }

  // Graceful shutdown for daemon mode
  let shuttingDown = false;
  let roundInFlight = false;
  let timer = null;

  function requestShutdown(signal) {
    if (shuttingDown) return;
    shuttingDown = true;
    console.log(`\n🛑  ${signal} received — finishing current round then exiting...`);
    if (timer) clearInterval(timer);
    if (!roundInFlight) {
      console.log("👋  Clean shutdown.");
      process.exit(0);
    }
  }
  process.on("SIGINT", () => requestShutdown("SIGINT"));
  process.on("SIGTERM", () => requestShutdown("SIGTERM"));

  async function runDaemonRound() {
    if (shuttingDown || roundInFlight) return;
    roundInFlight = true;
    try {
      const summary = await keeperLoop(server, keypair, networkPassphrase, CONFIG.registryContractId);
      if (summary.errors.length > 0) {
        console.error(`❌  Keeper round finished with ${summary.errors.length} error(s)`);
      }
    } catch (err) {
      // This is for truly unexpected errors in the loop itself
      console.error("❌  Fatal keeper loop error:", err.message);
    } finally {
      roundInFlight = false;
      if (shuttingDown) {
        console.log("👋  Clean shutdown.");
        process.exit(0);
      }
    }
  }

  // Run initial round immediately, then poll.
  await runDaemonRound();
  timer = setInterval(runDaemonRound, CONFIG.pollIntervalMs);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

main().catch((err) => {
  console.error("Fatal error:", err);
  process.exit(1);
});
