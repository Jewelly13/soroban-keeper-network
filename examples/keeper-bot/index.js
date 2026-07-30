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
 *   - Comprehensive startup validation for all config settings
 *   - Retry with exponential back-off + jitter on transient RPC errors
 *   - Graceful shutdown (SIGINT/SIGTERM) that drains the in-flight round
 *   - Permissionless expiry of stale tasks to refund owners
 *   - Read-only views (`keeper_balance`, etc.) are evaluated via simulation
 *     through `readContract`, not submitted as signed transactions — see
 *     that function's doc comment for why this matters
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
} = require("@stellar/stellar-sdk");

const NETWORK_CONFIG = {
  testnet: { rpcUrl: "https://soroban-testnet.stellar.org", networkPassphrase: Networks.TESTNET },
  futurenet: { rpcUrl: "https://rpc-futurenet.stellar.org", networkPassphrase: Networks.FUTURENET },
  mainnet: { rpcUrl: "https://mainnet.sorobanrpc.com", networkPassphrase: Networks.PUBLIC },
};

let CONFIG;
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
  if (value) message += `: ${value}`;
  console.error(`${message} — ${reason}`);
  process.exit(1);
}

function requireEnv(name, { parse, validate, secret = false, fallback }) {
  const raw = process.env[name];
  if (raw === undefined || raw === "") {
    if (fallback !== undefined) return fallback;
    fail(name, raw, "must be set");
  }
  try {
    const parsed = parse ? parse(raw) : raw;
    if (validate && !validate.fn(parsed)) fail(name, secret ? null : raw, validate.reason);
    return parsed;
  } catch (e) {
    fail(name, secret ? null : raw, e.message);
  }
}

function parseProfitMultiple(value) {
  const parsed = Number.parseFloat(value);
  if (!Number.isFinite(parsed) || parsed <= 0) throw new Error("must be a finite number greater than 0");
  return parsed;
}

function profitMultipleScale(value) {
  return BigInt(Math.ceil(value * 1000));
}

async function validateAndLoadConfig() {
  const { StrKey } = require("@stellar/stellar-sdk"); // Moved inside to be used only here
  const network = requireEnv("NETWORK", {
    validate: { fn: (v) => Object.keys(NETWORK_CONFIG).includes(v), reason: `must be one of: ${Object.keys(NETWORK_CONFIG).join(", ")}` },
    fallback: "testnet",
  });
  const registryContractId = requireEnv("REGISTRY_CONTRACT_ID", {
    validate: { fn: StrKey.isValidContract, reason: "must be a valid contract ID (starts with C...)" },
  });
  const secretKey = requireEnv("KEEPER_SECRET_KEY", {
    secret: true,
    validate: { fn: StrKey.isValidEd25519SecretSeed, reason: "must be a valid secret key (starts with S...)" },
  });

  // Optional: a signing key for tasks whose attached verifier is (or is
  // compatible with) the reference signature-verifier contract — see
  // docs/VERIFIERS.md. Deliberately separate from KEEPER_SECRET_KEY: the
  // party a verifier trusts to attest completion is not necessarily the
  // same party running this keeper bot. Left unset, the bot still runs
  // fine for tasks with no verifier or a verifier of a kind it doesn't
  // know how to produce a proof for — see generateProof's fallback.
  const signatureProofSecretKey = requireEnv("SIGNATURE_PROOF_SECRET_KEY", {
    secret: true,
    validate: {
      fn: StrKey.isValidEd25519SecretSeed,
      reason: "must be a valid secret key (starts with S...)",
    },
    fallback: null,
  });

  // After validating the required string values, we can create the server
  // connection and use it to validate the contract's existence on the network.
  const { rpcUrl } = NETWORK_CONFIG[network];
  const server = new SorobanRpc.Server(rpcUrl, { allowHttp: false });
  try {
    await server.getContractData(registryContractId);
  } catch (e) {
    if (e.response && e.response.status === 404) {
      fail("REGISTRY_CONTRACT_ID", registryContractId, `not found on network ${network}. Please check the contract ID and NETWORK settings.`);
    }
  }

  const minProfitMultiple = requireEnv("MIN_PROFIT_MULTIPLE", {
    parse: parseProfitMultiple,
    fallback: 2.0,
  });

  CONFIG = {
    network,
    registryContractId,
    secretKey,
    signatureProofSecretKey,
    once: process.argv.includes("--once") || process.env.RUN_ONCE === "true",
    pollIntervalMs: requireEnv("POLL_INTERVAL_MS", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => Number.isInteger(v) && v >= 1000, reason: "must be >= 1000" },
      fallback: 10000,
    }),
    withdrawThreshold: requireEnv("WITHDRAW_THRESHOLD", {
      parse: BigInt,
      validate: { fn: (v) => v >= 0n, reason: "must be non-negative" },
      fallback: 10000000n,
    }),
    minNetRewardStroops: requireEnv("MIN_NET_REWARD_STROOPS", {
      parse: BigInt,
      validate: { fn: (v) => v >= 0n, reason: "must be non-negative" },
      fallback: 1000000n,
    }),
    minProfitMultiple,
    minProfitMultipleScale: profitMultipleScale(minProfitMultiple),
    estimatedTransactionCostStroops: requireEnv("ESTIMATED_TX_COST_STROOPS", {
      parse: BigInt,
      validate: { fn: (v) => v > 0n, reason: "must be greater than 0" },
      fallback: 10000n,
    }),
    maxTasksPerRound: requireEnv("MAX_TASKS_PER_ROUND", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => Number.isInteger(v) && v >= 1, reason: "must be >= 1" },
      fallback: 5,
    }),
    maxRetries: requireEnv("MAX_RETRIES", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => Number.isInteger(v) && v >= 0, reason: "must be >= 0" },
      fallback: 3,
    }),
    retryBaseMs: requireEnv("RETRY_BASE_MS", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => Number.isInteger(v) && v > 0, reason: "must be > 0" },
      fallback: 500,
    }),
    expireStaleTasks: requireEnv("EXPIRE_STALE_TASKS", {
      parse: (v) => v.toLowerCase() === "true",
      fallback: true,
    }),
    eventsPageSize: requireEnv("EVENTS_PAGE_SIZE", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => v > 0, reason: "must be a positive number" },
      fallback: 100,
    }),
    eventsMaxPages: requireEnv("EVENTS_MAX_PAGES", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => v > 0, reason: "must be a positive number" },
      fallback: 10,
    }),
  };
  return CONFIG;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function isPermanentError(err) {
  const msg = (err && err.message ? err.message : "").toLowerCase();
  return msg.includes("simulation failed") || msg.includes("invalidaction") || msg.includes("unauthorized") || msg.includes("already");
}

async function withRetry(label, fn) {
  let lastErr;
  for (let attempt = 0; attempt <= CONFIG.maxRetries; attempt++) {
    try {
      return await fn();
    } catch (err) {
      lastErr = err;
      if (isPermanentError(err) || attempt === CONFIG.maxRetries) throw err;
      const backoff = CONFIG.retryBaseMs * 2 ** attempt;
      const delay = backoff + Math.floor(Math.random() * CONFIG.retryBaseMs);
      console.warn(`  ↻  ${label} failed (attempt ${attempt + 1}), retrying in ${delay}ms: ${err.message}`);
      await sleep(delay);
    }
  }
  throw lastErr;
}

const MAX_SYMBOL_LENGTH = 9;
function topicSymbol(name) {
  if (name.length > MAX_SYMBOL_LENGTH) throw new Error(`Symbol "${name}" is too long`);
  return nativeToScVal(name, { type: "symbol" }).toXDR("base64");
}

const REGISTRY_EVENTS = {
  taskRegistered: [topicSymbol("reg"), topicSymbol("task")],
};

async function simulateAndSend(server, keypair, networkPassphrase, tx) {
  const simResponse = await server.simulateTransaction(tx);
  if (SorobanRpc.Api.isSimulationError(simResponse)) throw new Error(`Simulation failed: ${simResponse.error}`);
  const preparedTx = SorobanRpc.assembleTransaction(tx, simResponse).build();
  preparedTx.sign(keypair);
  const sendResponse = await server.sendTransaction(preparedTx);
  if (sendResponse.status === "ERROR") throw new Error(`Send failed: ${JSON.stringify(sendResponse.errorResult)}`);
  let getResponse = await server.getTransaction(sendResponse.hash);
  let attempts = 0;
  while (getResponse.status === SorobanRpc.Api.GetTransactionStatus.NOT_FOUND && attempts < 30) {
    await sleep(2000);
    getResponse = await server.getTransaction(sendResponse.hash);
    attempts++;
  }
  if (getResponse.status === SorobanRpc.Api.GetTransactionStatus.SUCCESS) return getResponse;
  throw new Error(`Transaction failed with status: ${getResponse.status}`);
}

async function invokeContract(server, keypair, networkPassphrase, contractId, method, args) {
  const account = await server.getAccount(keypair.publicKey());
  const tx = new TransactionBuilder(account, { fee: BASE_FEE, networkPassphrase })
    .addOperation(new Contract(contractId).call(method, ...args))
    .setTimeout(30)
    .build();
  return simulateAndSend(server, keypair, networkPassphrase, tx);
}

async function readContract(server, sourcePublicKey, networkPassphrase, contractId, method, args) {
  const account = await server.getAccount(sourcePublicKey);
  const tx = new TransactionBuilder(account, { fee: BASE_FEE, networkPassphrase })
    .addOperation(new Contract(contractId).call(method, ...args))
    .setTimeout(30)
    .build();
  const sim = await server.simulateTransaction(tx);
  if (SorobanRpc.Api.isSimulationError(sim)) throw new Error(`Simulation failed: ${sim.error}`);
  return sim.result ? scValToNative(sim.result.retval) : null;
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
 * Fetches TaskRegistered events across the window, following the pagination
 * cursor. Bounded by `CONFIG.eventsMaxPages` so one very busy window cannot
 * stall a round indefinitely; hitting the bound is logged, never silent.
 *
 * This pagination behaviour is compatible with Soroban RPC v1.2.0 and later,
 * where `startLedger` and `cursor` are mutually exclusive.
 */
async function fetchPendingTasks(server, contractId, startLedger) {
  const tasks = [];
  let latestLedger = null;
  try {
    const response = await server.getEvents({
      startLedger,
      filters: [{ type: "contract", contractIds: [contractId], topics: [REGISTRY_EVENTS.taskRegistered] }],
      limit: 100,
    });

    latestLedger = response.latestLedger;

    for (const event of response.events || []) {
      try {
        // TaskRegistered data is (task_id, owner, reward, deadline); the owner is not needed here.
        const [taskIdVal, , rewardVal, deadlineVal] = event.value.value();
        tasks.push({ taskId: scValToNative(taskIdVal), reward: scValToNative(rewardVal), deadline: scValToNative(deadlineVal) });
      } catch (err) {
        console.warn(`⚠️  Could not decode a TaskRegistered event: ${err.message} — the contract's event shape may have changed.`);
      }

      pages++;
      if (
        !response.events ||
        response.events.length < CONFIG.eventsPageSize ||
        !response.cursor
      ) {
        break; // Window exhausted
      }
      cursor = response.cursor;
    } catch (e) {
      console.warn("⚠️  Failed to fetch events page:", e.message);
      break; // Stop pagination on error
    }
  }

  if (pages === CONFIG.eventsMaxPages) {
    console.warn(
      `⚠️  Stopped fetching events after ${pages} pages — more may remain in this window.`
    );
  }

  return tasks;
}

function profitability(task, feeBps) {
  const basisPoints = BigInt(feeBps);
  const netReward = task.reward * (10000n - basisPoints) / 10000n;
  const estimatedCost = CONFIG.estimatedTransactionCostStroops * 3n;
  const clearsMinimum = netReward >= CONFIG.minNetRewardStroops;
  const clearsMultiple = netReward * 1000n > estimatedCost * CONFIG.minProfitMultipleScale;
  return { netReward, estimatedCost, clearsMinimum, clearsMultiple };
}

async function executeTaskOffChain(task) {
  console.log(`  ⚙️  Executing task ${task.taskId} off-chain...`);
  await sleep(500);
  return Buffer.from(`keeper-proof:task:${task.taskId}:ts:${Date.now()}`).toString("hex");
}

// ─────────────────────────────────────────────────────────────────────────────
// Verifier-aware proof generation
//
// A task's `verifier` field (see docs/VERIFIERS.md) determines what kind of
// proof `execute_task` will actually accept. This bot only knows how to
// produce proofs for the reference signature-verifier kind (see #102) —
// extending this for other verifier kinds (oracle-based, inclusion-based)
// is a follow-up; `generateProof` below is the extension point for that.
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Builds the exact message the reference signature-verifier contract
 * expects `proof` to be a valid ed25519 signature over: the task owner's
 * Address XDR bytes, then calldata, then deadline and reward as big-endian
 * bytes — byte-for-byte matching
 * `signature_verifier::signed_message(&env, &task)` on the contract side
 * (see contracts/verifiers/signature-verifier/src/lib.rs).
 *
 * `task` here is the full record from `get_task` (owner, calldata, deadline,
 * reward as returned by `scValToNative`), not the trimmed
 * `{taskId, reward, deadline}` shape `fetchPendingTasks` uses internally.
 */
function buildSignatureVerifierMessage(task) {
  const ownerAddressBytes = new Address(task.owner).toScVal().toXDR();

  const deadlineBytes = Buffer.alloc(8);
  deadlineBytes.writeBigUInt64BE(BigInt(task.deadline));

  const rewardBytes = Buffer.alloc(16);
  // i128 as two big-endian 64-bit halves, matching Rust's `i128::to_be_bytes`.
  const rewardBig = BigInt(task.reward);
  rewardBytes.writeBigInt64BE(rewardBig >> 64n, 0);
  rewardBytes.writeBigUInt64BE(rewardBig & 0xffffffffffffffffn, 8);

  return Buffer.concat([
    ownerAddressBytes,
    Buffer.from(task.calldata),
    deadlineBytes,
    rewardBytes,
  ]);
}

/**
 * Signs `task`'s identity with `signatureProofKeypair` and returns the raw
 * 64-byte ed25519 signature the reference signature-verifier contract's
 * `verify` expects as `proof`. The caller is responsible for confirming the
 * task's attached verifier is actually configured with this keypair's
 * public key as its `signer` — this function doesn't check that (the bot
 * has no on-chain way to distinguish "a signature verifier with a
 * different signer" from "not a signature verifier at all" without calling
 * the verifier contract's own `signer()` view, which is left as a
 * follow-up rather than done unconditionally on every task).
 */
function signProofForTask(task, signatureProofKeypair) {
  const message = buildSignatureVerifierMessage(task);
  return signatureProofKeypair.sign(message);
}

/**
 * Produces the `proof` bytes to submit with `execute_task` for `task`.
 *
 * If the task has no verifier attached, or this bot isn't configured with
 * a signing key (`SIGNATURE_PROOF_SECRET_KEY`), falls back to the base MVP
 * placeholder proof — unchanged behavior from before verifiers existed.
 * If a signing key is configured and the task has a verifier attached, this
 * bot assumes it's (or is compatible with) the reference signature-verifier
 * kind, since that's currently the only kind it knows how to produce a
 * proof for.
 */
async function generateProof(task, fullTask, signatureProofKeypair) {
  if (fullTask.verifier && signatureProofKeypair) {
    console.log(
      `  ✍️  Task ${task.taskId} has a verifier attached — signing proof with the configured signature key.`
    );
    return signProofForTask(fullTask, signatureProofKeypair).toString("hex");
  }
  return executeTaskOffChain(task);
}

// ─────────────────────────────────────────────────────────────────────────────
// Main keeper loop
// ─────────────────────────────────────────────────────────────────────────────

async function keeperLoop(
  server,
  keypair,
  networkPassphrase,
  contractId,
  emptyRounds = 0,
  signatureProofKeypair = null
) {
  // A round is successful if it runs to completion without any unhandled
  // exceptions. An RPC error that cannot be resolved with retries, or any
  // other unexpected error, is a failure.
  // Note: a round that finds no tasks is a success. Losing a claim race to
  // another keeper is also a success, as this is normal competitive behaviour.
async function keeperLoop(server, keypair, networkPassphrase, contractId, emptyRounds = 0) {
  const summary = { processed: 0, errors: [] };
  let newEmptyRounds = emptyRounds;
  try {
    const nowSeconds = BigInt(Math.floor(Date.now() / 1000));
    console.log(`\n🔄  Keeper round at ${new Date().toISOString()}`);
    const latestLedger = await server.getLatestLedger();
    const pendingTasks = await fetchPendingTasks(server, contractId, Math.max(1, latestLedger.sequence - 1000));
    console.log(`  📋  Found ${pendingTasks.length} TaskRegistered events to evaluate`);

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

    const feeBpsValue = await readContract(server, keypair.publicKey(), networkPassphrase, contractId, "get_fee_bps", []);
    const feeBps = BigInt(feeBpsValue);
    if (feeBps < 0n || feeBps > 10000n) throw new Error(`Invalid on-chain fee rate: ${feeBps}`);
    console.log(`  💸  Current registry fee: ${feeBps} bps`);

    if (pendingTasks.length === 0) {
      newEmptyRounds++;
      if (newEmptyRounds % 30 === 0) console.warn(`  ⚠️  No TaskRegistered events found for ${newEmptyRounds} consecutive rounds.`);
    } else {
      newEmptyRounds = 0;
    }

    for (const task of pendingTasks) {
      if (summary.processed >= CONFIG.maxTasksPerRound) break;

      // The event-derived deadline is potentially stale, but it's a cheap
      // client-side filter. is_claimable will check the true current deadline.
      if (task.deadline <= nowSeconds) {
        if (CONFIG.expireStaleTasks) {
          try {
            await withRetry(`expire_task ${task.taskId}`, () => invokeContract(server, keypair, networkPassphrase, contractId, "expire_task", [nativeToScVal(task.taskId, { type: "u64" })]));
            console.log(`  ♻️  Task ${task.taskId} expired — escrow refunded to owner`);
          } catch (err) {
            console.log(`  ⏰  Task ${task.taskId} past deadline (skip: ${err.message})`);
          }
        } else {
          console.log(`  ⏰  Task ${task.taskId} is past deadline, skipping`);
        }
        continue;
      }

      const economics = profitability(task, feeBps);
      if (!economics.clearsMinimum || !economics.clearsMultiple) {
        const reason = !economics.clearsMinimum
          ? `net reward ${economics.netReward} is below minimum ${CONFIG.minNetRewardStroops}`
          : `net reward ${economics.netReward} does not exceed ${CONFIG.minProfitMultiple}x estimated cost`;
        console.log(`  ⏭️  Skipping task ${task.taskId} (reward: ${task.reward}, estimated cost: ${economics.estimatedCost} stroops): ${reason}`);
        continue;
      }
      try {
        // Pre-flight check: is the task actually claimable right now? This
        // is a read-only simulation, so it costs nothing. It confirms the
        // task is still pending and not locked by another keeper.
        const claimable = await readContract(
          server,
          keypair.publicKey(),
          networkPassphrase,
          contractId,
          "is_claimable",
          [nativeToScVal(task.taskId, { type: "u64" })]
        );

        if (!claimable) {
          console.log(
            `  ⏩  Skipping task ${task.taskId} — not claimable (already claimed or finished)`
          );
          continue;
        }

        // The pre-check is advisory, not a lock. A competitor can still
        // claim the task in the interval between our simulation and our
        // submission. The `claim_task` call can still fail, which is
        // normal and expected.
        console.log(
          `  📌  Attempting to claim task ${task.taskId} (reward: ${task.reward})...`
        );
        await withRetry(`claim_task ${task.taskId}`, () =>
          invokeContract(
            server,
            keypair,
            networkPassphrase,
            contractId,
            "claim_task",
            [
              nativeToScVal(keypair.publicKey(), { type: "address" }),
              nativeToScVal(task.taskId, { type: "u64" }),
            ]
          )
        );
        console.log(`  ✅  Task ${task.taskId} claimed!`);

        // Fetch the full task record (including `verifier`, which the
        // trimmed TaskRegistered-event shape in `task` doesn't carry) so
        // `generateProof` can decide how to produce a proof this task's
        // verifier (if any) will actually accept. Read-only, so this goes
        // through `readContract` (simulation only) like `keeper_balance`.
        const fullTask = await readContract(
          server,
          keypair.publicKey(),
          networkPassphrase,
          contractId,
          "get_task",
          [nativeToScVal(task.taskId, { type: "u64" })]
        );

        const proof = await generateProof(
          task,
          fullTask,
          signatureProofKeypair
        );

        await withRetry(`execute_task ${task.taskId}`, () =>
          invokeContract(
            server,
            keypair,
            networkPassphrase,
            contractId,
            "execute_task",
            [
              nativeToScVal(keypair.publicKey(), { type: "address" }),
              nativeToScVal(task.taskId, { type: "u64" }),
              nativeToScVal(Buffer.from(proof, "hex"), { type: "bytes" }),
            ]
          )
        );
        console.log(
          `  💰  Task ${task.taskId} executed! Proof: ${proof.slice(0, 20)}...`
        );
      try {
        console.log(`  📌  Attempting to claim task ${task.taskId} (reward: ${task.reward}, estimated cost: ${economics.estimatedCost} stroops, net reward: ${economics.netReward})...`);
        await withRetry(`claim_task ${task.taskId}`, () => invokeContract(server, keypair, networkPassphrase, contractId, "claim_task", [nativeToScVal(task.taskId, { type: "u64" })]));
        const proof = await executeTaskOffChain(task);
        await withRetry(`execute_task ${task.taskId}`, () => invokeContract(server, keypair, networkPassphrase, contractId, "execute_task", [nativeToScVal(task.taskId, { type: "u64" }), nativeToScVal(proof, { type: "bytes" })]));
        summary.processed++;
        console.log(`  ✅  Task ${task.taskId} executed successfully`);
      } catch (err) {
        summary.errors.push(err);
        console.error(`  ❌  Task ${task.taskId} failed: ${err.message}`);
      }
    }

    try {
      const balance = BigInt(await readContract(server, keypair.publicKey(), networkPassphrase, contractId, "keeper_balance", [nativeToScVal(keypair.publicKey(), { type: "address" })]));
      if (balance >= CONFIG.withdrawThreshold) {
        await withRetry("withdraw_rewards", () => invokeContract(server, keypair, networkPassphrase, contractId, "withdraw_rewards", []));
        console.log(`  💰  Withdrew keeper balance of ${balance} stroops`);
      }
    } catch (err) {
      console.warn(`  ⚠️  Withdrawal check failed: ${err.message}`);
    }
  } catch (err) {
    summary.errors.push(err);
    console.error(`  ❌  Keeper round failed: ${err.message}`);
  }
  return { ...summary, emptyRounds: newEmptyRounds };
}

async function main() {
  await validateAndLoadConfig();
  const network = NETWORK_CONFIG[CONFIG.network];
  const server = new SorobanRpc.Server(network.rpcUrl, { allowHttp: false });
  const keypair = Keypair.fromSecret(CONFIG.secretKey);
  const signatureProofKeypair = CONFIG.signatureProofSecretKey
    ? Keypair.fromSecret(CONFIG.signatureProofSecretKey)
    : null;
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
    const { summary } = await keeperLoop(
      server,
      keypair,
      networkPassphrase,
      CONFIG.registryContractId,
      0,
      signatureProofKeypair
    );
    const ok = summary.errors.length === 0;
    console.log(ok ? "✅  Round complete." : "⚠️  Round completed with errors.");
    process.exit(ok ? 0 : 1);
  }

  // Graceful shutdown for daemon mode
  let shuttingDown = false;
  let roundInFlight = false;
  let emptyRounds = 0;
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
      const { summary, emptyRounds: newEmptyRounds } = await keeperLoop(
        server,
        keypair,
        networkPassphrase,
        CONFIG.registryContractId,
        emptyRounds,
        signatureProofKeypair
      );
      emptyRounds = newEmptyRounds;
      if (summary.errors.length > 0) {
        console.error(
          `❌  Keeper round finished with ${summary.errors.length} error(s)`
        );
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

// ─────────────────────────────────────────────────────────────────────────────
// Module exports for testing
// ─────────────────────────────────────────────────────────────────────────────

module.exports = {
  isPermanentError,
  withRetry,
  fetchPendingTasks,
  validateAndLoadConfig,
  keeperLoop,
  sleep,
  buildSignatureVerifierMessage,
  signProofForTask,
  generateProof,
};
  let emptyRounds = 0;
  do {
    const result = await keeperLoop(server, keypair, network.networkPassphrase, CONFIG.registryContractId, emptyRounds);
    emptyRounds = result.emptyRounds;
    if (CONFIG.once) process.exitCode = result.errors.length ? 1 : 0;
    else await sleep(CONFIG.pollIntervalMs);
  } while (!CONFIG.once);
}

if (require.main === module) main().catch((err) => { console.error(err); process.exitCode = 1; });

module.exports = { fetchPendingTasks, keeperLoop, profitability, readContract, validateAndLoadConfig };
