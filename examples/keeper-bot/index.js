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
 * Usage:
 *   cp .env.example .env
 *   # Fill in your secret key and contract address
 *   node index.js
 *
 * This example already includes:
 *   - Retry with exponential back-off + jitter on transient RPC errors
 *   - Graceful shutdown (SIGINT/SIGTERM) that drains the in-flight round
 *   - Permissionless expiry of stale tasks to refund owners
 *
 * Production keepers should additionally add:
 *   - Persistent task state DB (SQLite / Redis) to avoid double-claiming
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

async function fetchPendingTasks(server, contractId, startLedger) {
  const tasks = [];
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
  return tasks;
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
  const nowSeconds = Math.floor(Date.now() / 1000);
  console.log(`\n🔄  Keeper round at ${new Date().toISOString()}`);

  // Determine start ledger for event query (last ~1000 ledgers ≈ 1.4h at 5s)
  const latestLedger = await server.getLatestLedger();
  const startLedger = Math.max(1, latestLedger.sequence - 1000);

  const pendingTasks = await fetchPendingTasks(server, contractId, startLedger);
  console.log(`  📋  Found ${pendingTasks.length} TaskRegistered events to evaluate`);

  let processed = 0;
  for (const task of pendingTasks) {
    if (processed >= CONFIG.maxTasksPerRound) break;

    // Past-deadline tasks can't be executed. Optionally unwind them so the
    // owner's escrow is refunded (permissionless — anyone may call expire_task).
    if (task.deadline <= nowSeconds) {
      if (CONFIG.expireStaleTasks) {
        try {
          await withRetry(`expire_task ${task.taskId}`, () =>
            invokeContract(server, keypair, networkPassphrase, contractId, "expire_task", [
              nativeToScVal(task.taskId, { type: "u64" }),
            ])
          );
          console.log(`  ♻️  Task ${task.taskId} expired — escrow refunded to owner`);
        } catch (err) {
          // Already expired/executed by someone else — nothing to do.
          console.log(`  ⏰  Task ${task.taskId} past deadline (skip: ${err.message})`);
        }
      } else {
        console.log(`  ⏰  Task ${task.taskId} is past deadline, skipping`);
      }
      continue;
    }

    try {
      console.log(`  📌  Attempting to claim task ${task.taskId} (reward: ${task.reward})...`);

      // 1. Claim the task (retry transient RPC errors; bail on "already claimed")
      await withRetry(`claim_task ${task.taskId}`, () =>
        invokeContract(server, keypair, networkPassphrase, contractId, "claim_task", [
          nativeToScVal(keypair.publicKey(), { type: "address" }),
          nativeToScVal(task.taskId, { type: "u64" }),
        ])
      );
      console.log(`  ✅  Task ${task.taskId} claimed!`);

      // 2. Execute off-chain
      const proof = await executeTaskOffChain(task);

      // 3. Submit execution proof on-chain
      await withRetry(`execute_task ${task.taskId}`, () =>
        invokeContract(server, keypair, networkPassphrase, contractId, "execute_task", [
          nativeToScVal(keypair.publicKey(), { type: "address" }),
          nativeToScVal(task.taskId, { type: "u64" }),
          nativeToScVal(Buffer.from(proof, "hex"), { type: "bytes" }),
        ])
      );
      console.log(`  💰  Task ${task.taskId} executed! Proof: ${proof.slice(0, 20)}...`);
      processed++;
    } catch (err) {
      // Common reasons: already claimed by another keeper, or status mismatch
      console.warn(`  ⚠️  Failed to process task ${task.taskId}: ${err.message}`);
    }
  }

  // Check accumulated rewards and withdraw if above threshold
  try {
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
    console.warn(`  ⚠️  Balance check failed: ${err.message}`);
  }
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
  console.log(`  Poll     : every ${CONFIG.pollIntervalMs / 1000}s`);
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

  // Graceful shutdown: stop scheduling new rounds and let the in-flight round
  // finish so we never leave a task claimed-but-unexecuted on our account.
  let shuttingDown = false;
  let roundInFlight = false;
  let timer = null;

  function requestShutdown(signal) {
    if (shuttingDown) return;
    shuttingDown = true;
    console.log(`\n🛑  ${signal} received — finishing current round then exiting...`);
    if (timer) clearInterval(timer);
    // If nothing is running, exit now; otherwise the runner exits when it drains.
    if (!roundInFlight) {
      console.log("👋  Clean shutdown.");
      process.exit(0);
    }
  }
  process.on("SIGINT", () => requestShutdown("SIGINT"));
  process.on("SIGTERM", () => requestShutdown("SIGTERM"));

  async function runRound() {
    if (shuttingDown || roundInFlight) return;
    roundInFlight = true;
    try {
      await keeperLoop(server, keypair, networkPassphrase, CONFIG.registryContractId);
    } catch (err) {
      console.error("❌  Keeper loop error:", err.message);
    } finally {
      roundInFlight = false;
      if (shuttingDown) {
        console.log("👋  Clean shutdown.");
        process.exit(0);
      }
    }
  }

  // Run initial round immediately, then poll.
  await runRound();
  timer = setInterval(runRound, CONFIG.pollIntervalMs);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

main().catch((err) => {
  console.error("Fatal error:", err);
  process.exit(1);
});
