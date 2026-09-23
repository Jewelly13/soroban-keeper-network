/**
 * Configuration validation and loading for keeper-bot-v2
 *
 * All configuration comes from environment variables.
 * On startup, this module validates the configuration and fails fast if anything is wrong.
 */

import { StrKey } from "@stellar/stellar-sdk";
import { NETWORK_PRESETS, NETWORK_NAMES, isNetworkName } from "@soroban-keeper-network/sdk";

/**
 * Validated configuration for the keeper bot.
 */
export interface KeeperConfig {
  // Network settings
  network: string;
  rpcUrl: string;
  networkPassphrase: string;

  // Keeper identity
  secretKey: string;
  registryContractId: string;

  // Task processing
  pollIntervalMs: number;
  maxTasksPerRound: number;
  maxConcurrentTasks: number;

  // Budget and profitability
  minProfitMarginStroops: bigint;
  withdrawThreshold: bigint;

  // Retry behavior
  maxRetries: number;
  retryBaseMs: number;

  // Features
  expireStaleTasks: boolean;
  simulateExecution: boolean;

  // Persistence
  dbPath?: string;

  // Mode
  runOnce: boolean;
}

/**
 * Reads and validates environment variables.
 * Fails fast with detailed error messages if configuration is invalid.
 */
function requireEnv(
  name: string,
  options: {
    parse?: (raw: string) => unknown;
    validate?: { fn: (parsed: unknown) => boolean; reason: string };
    secret?: boolean;
    fallback?: unknown;
  } = {}
): unknown {
  const raw = process.env[name];
  if (raw === undefined || raw === "") {
    if (options.fallback !== undefined) {
      return options.fallback;
    }
    const message = `Invalid ${name}: must be set`;
    console.error(message);
    process.exit(1);
  }

  try {
    const parsed = options.parse ? options.parse(raw) : raw;
    if (options.validate && !options.validate.fn(parsed)) {
      const value = options.secret ? null : raw;
      const message = `Invalid ${name}${value ? `: ${value}` : ""} — ${options.validate.reason}`;
      console.error(message);
      process.exit(1);
    }
    return parsed;
  } catch (e) {
    const value = options.secret ? null : raw;
    const message = `Invalid ${name}${value ? `: ${value}` : ""} — ${(e as Error).message}`;
    console.error(message);
    process.exit(1);
  }
}

/**
 * Load and validate the keeper configuration from environment variables.
 */
export async function loadConfig(): Promise<KeeperConfig> {
  const network = requireEnv("NETWORK", {
    validate: {
      fn: isNetworkName,
      reason: `must be one of: ${NETWORK_NAMES.join(", ")}`,
    },
    fallback: "testnet",
  }) as string;

  const registryContractId = requireEnv("REGISTRY_CONTRACT_ID", {
    validate: {
      fn: StrKey.isValidContract,
      reason: "must be a valid contract ID (starts with C...)",
    },
  }) as string;

  const secretKey = requireEnv("KEEPER_SECRET_KEY", {
    secret: true,
    validate: {
      fn: StrKey.isValidEd25519SecretSeed,
      reason: "must be a valid secret key (starts with S...)",
    },
  }) as string;

  const maxTasksPerRound = requireEnv("MAX_TASKS_PER_ROUND", {
    parse: (v) => parseInt(v, 10),
    validate: { fn: (v) => v >= 1, reason: "must be >= 1" },
    fallback: 5,
  }) as number;

  const maxConcurrentTasks = requireEnv("MAX_CONCURRENT_TASKS", {
    parse: (v) => parseInt(v, 10),
    validate: { fn: (v) => v >= 1, reason: "must be >= 1" },
    fallback: 2, // Conservative default: 2 concurrent tasks
  }) as number;

  // Validate that maxConcurrentTasks <= maxTasksPerRound
  if (maxConcurrentTasks > maxTasksPerRound) {
    console.error(
      `Invalid MAX_CONCURRENT_TASKS: ${maxConcurrentTasks} exceeds MAX_TASKS_PER_ROUND: ${maxTasksPerRound}`
    );
    process.exit(1);
  }

  const { rpcUrl, networkPassphrase } = NETWORK_PRESETS[network];

  return {
    network,
    rpcUrl,
    networkPassphrase,
    secretKey,
    registryContractId,
    pollIntervalMs: requireEnv("POLL_INTERVAL_MS", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => v >= 1000, reason: "must be >= 1000" },
      fallback: 10000,
    }) as number,
    maxTasksPerRound,
    maxConcurrentTasks,
    minProfitMarginStroops: BigInt(
      (requireEnv("MIN_PROFIT_MARGIN_STROOPS", {
        parse: (v) => v,
        fallback: "0",
      }) as string) || "0"
    ),
    withdrawThreshold: BigInt(
      (requireEnv("WITHDRAW_THRESHOLD", {
        parse: (v) => v,
        fallback: "10000000",
      }) as string) || "10000000"
    ),
    maxRetries: requireEnv("MAX_RETRIES", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => v >= 0, reason: "must be >= 0" },
      fallback: 3,
    }) as number,
    retryBaseMs: requireEnv("RETRY_BASE_MS", {
      parse: (v) => parseInt(v, 10),
      validate: { fn: (v) => v > 0, reason: "must be > 0" },
      fallback: 500,
    }) as number,
    expireStaleTasks: requireEnv("EXPIRE_STALE_TASKS", {
      parse: (v) => v.toLowerCase() === "true",
      fallback: true,
    }) as boolean,
    simulateExecution: requireEnv("SIMULATE_EXECUTION", {
      parse: (v) => v.toLowerCase() === "true",
      fallback: false,
    }) as boolean,
    dbPath: (requireEnv("DB_PATH", { fallback: undefined }) as string) || undefined,
    runOnce: process.argv.includes("--once") || process.env.RUN_ONCE === "true",
  };
}
