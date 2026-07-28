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

const NETWORK_CONFIG = {
  testnet: { rpcUrl: "https://soroban-testnet.stellar.org", networkPassphrase: Networks.TESTNET },
  futurenet: { rpcUrl: "https://rpc-futurenet.stellar.org", networkPassphrase: Networks.FUTURENET },
  mainnet: { rpcUrl: "https://mainnet.sorobanrpc.com", networkPassphrase: Networks.PUBLIC },
};

let CONFIG;

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

async function fetchPendingTasks(server, contractId, startLedger) {
  const tasks = [];
  try {
    const response = await server.getEvents({
      startLedger,
      filters: [{ type: "contract", contractIds: [contractId], topics: [REGISTRY_EVENTS.taskRegistered] }],
      limit: 100,
    });
    for (const event of response.events || []) {
      try {
        const [taskIdVal, , rewardVal, deadlineVal] = event.value.value();
        tasks.push({ taskId: scValToNative(taskIdVal), reward: scValToNative(rewardVal), deadline: scValToNative(deadlineVal) });
      } catch (_) {
        // Ignore malformed events and continue with the remaining events.
      }
    }
  } catch (e) {
    console.warn("⚠️  Failed to fetch events:", e.message);
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

async function keeperLoop(server, keypair, networkPassphrase, contractId, emptyRounds = 0) {
  const summary = { processed: 0, errors: [] };
  let newEmptyRounds = emptyRounds;
  try {
    const nowSeconds = BigInt(Math.floor(Date.now() / 1000));
    console.log(`\n🔄  Keeper round at ${new Date().toISOString()}`);
    const latestLedger = await server.getLatestLedger();
    const pendingTasks = await fetchPendingTasks(server, contractId, Math.max(1, latestLedger.sequence - 1000));
    console.log(`  📋  Found ${pendingTasks.length} TaskRegistered events to evaluate`);

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
