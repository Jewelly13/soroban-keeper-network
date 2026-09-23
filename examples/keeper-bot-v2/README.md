# Soroban Keeper Network — Keeper Bot v2

**v2 is a production-grade keeper bot for operators running competitively.** For beginners, see [`../keeper-bot`](../keeper-bot) instead.

This keeper bot:
- Polls the Soroban RPC for `TaskRegistered` events emitted by the KeeperRegistry contract
- Evaluates task profitability before committing resources
- Claims and executes eligible tasks **concurrently within a configurable limit** to maximize throughput
- Persists task state across restarts to avoid double-claiming
- Safely coordinates multiple workers in the same process to prevent task races
- Periodically withdraws accumulated XLM rewards

## Key Differences from v1

| Feature | v1 | v2 |
|---------|----|----|
| Language | JavaScript (CommonJS) | TypeScript |
| Concurrency | Serial (one task at a time) | Bounded concurrent worker pool |
| Persistence | In-memory only; lost on restart | SQLite database; survives restart |
| Task State Tracking | Per-round outcome cache | Durable claimed-in-progress state |
| Profitability Check | Basic (optional) | Mandatory pre-claim evaluation |
| Process Coordination | None | Intra-process race prevention |

## Quick Start

```bash
# Install dependencies
npm install

# Copy and configure environment
cp .env.example .env
# Edit .env with your secret key, registry contract, network, etc.

# Build TypeScript
npm run build

# Run the keeper (daemon mode)
npm start

# Run once and exit (useful for cron/serverless)
RUN_ONCE=true npm start

# Run tests
npm run test

# Run linter and type checker
npm run lint
npm run typecheck
```

## Configuration

All configuration is via environment variables (see `.env.example`):

- **`KEEPER_SECRET_KEY`** — Your keeper's Stellar secret key (required, starts with `S...`)
- **`REGISTRY_CONTRACT_ID`** — KeeperRegistry contract ID (required, starts with `C...`)
- **`NETWORK`** — `testnet`, `public`, or `futurenet` (default: `testnet`)
- **`MAX_TASKS_PER_ROUND`** — Ceiling on total tasks processed per round (default: `5`)
- **`MAX_CONCURRENT_TASKS`** — How many tasks run in parallel (default: `2`; must be ≤ `MAX_TASKS_PER_ROUND`)
- **`MIN_PROFIT_MARGIN_STROOPS`** — Minimum net profit required (default: `0`)
- **`POLL_INTERVAL_MS`** — Time between rounds in daemon mode (default: `10000`)
- **`WITHDRAW_THRESHOLD`** — Auto-withdraw when balance reaches this (default: `10000000` stroops)

## Architecture

### Concurrency Model

The bot uses a **bounded worker pool** to process tasks concurrently:

1. **Task Fetching** — Polls RPC for pending tasks (serial; bounded by `MAX_TASKS_PER_ROUND`)
2. **Claimed-In-Progress Check** — Before claiming each task, marks it claimed-in-progress in persistent state
3. **Semaphore-Guarded Execution** — At most `MAX_CONCURRENT_TASKS` workers claim/execute simultaneously
4. **Per-Task Isolation** — If one task fails, others continue unaffected
5. **Budget Enforcement** — Total fees across all in-flight tasks cannot exceed remaining budget

### Persistent State Schema

Tasks are tracked in SQLite with the following outcomes:

- `Claimed` — Task was claimed by this keeper; awaiting execution result
- `Executed` — Task execution completed successfully; reward claimed
- `Expired` — Task deadline passed; escrow refunded to owner
- `Failed` — Execution failed; task left for another keeper
- `Skipped` — Task was skipped (unsupported executor, unprofitable, etc.)

On startup, the bot loads this state before its first round so a restart does not re-claim or re-execute a task it already processed.

### Intra-Process Race Prevention

When two workers in the same process both try to claim the same task:

1. Worker A checks the claimed-in-progress state and sees the task is free
2. Worker A **atomically** marks the task claimed-in-progress before making the RPC call
3. Worker B checks the state and sees the task is already claimed-in-progress
4. Worker B skips the task and moves to the next one

This prevents the expensive failure mode where two RPC calls race and one fails late with a "already claimed" error after fees are paid.

## Executor Interface

Executors are responsible for performing the off-chain work a task describes. Register executors by task type:

```typescript
import { EXECUTORS, Executor } from "./src/executors/index.js";

const myExecutor: Executor = async (task, ctx) => {
  // task: { taskId, taskType, calldata, reward, deadline, verifier }
  // ctx: { server, keypair, networkPassphrase, log }
  
  // Return proof bytes on success, null on failure
  return Buffer.from("proof-data");
};

EXECUTORS.set("MyTaskType", myExecutor);
```

If a task type has no registered executor and `SIMULATE_EXECUTION` is false, the task is skipped.

## Monitoring & Metrics

The bot logs:
- Round start/end times and task counts
- Per-task claim/execution success/failure with reasons
- Accumulated reward balance and withdrawal events
- Errors with categorization (transient vs. permanent)

For production deployments, integrate the metrics API (under development in a separate issue).

## Development & Testing

```bash
# Run tests with coverage
npm run test

# Watch mode (re-run on file changes)
npm run test

# TypeScript type checking
npm run typecheck

# Lint code
npm run lint

# Full build validation
npm run build && npm run lint && npm run typecheck && npm run test:run
```

## Known Limitations

- Verifier support is pluggable but not yet widely implemented; unregistered verifiers cause task skip
- No MEV-aware batching yet; transactions are submitted individually
- Database-based state persistence assumes SQLite; other backends would require schema abstraction layer

## Production Considerations

- Run multiple instances on different machines (or in separate containers), each with a unique keypair
- Monitor logs and metrics for missed executions or persistent errors
- Set up alerting for keeper balance falling below operating threshold
- Periodically review executor performance and profitability thresholds
- Keep the SDK dependency up-to-date as contract versions evolve

## See Also

- [Keeper Bot v1](../keeper-bot) — Beginner-friendly example
- [SDK Documentation](../../packages/sdk-ts/README.md)
- [Contract README](../../README.md#known-design-decisions)
- [Keeper Network Overview](../../docs/KEEPER.md)
