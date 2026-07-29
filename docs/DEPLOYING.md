# Deploying & Running

A step-by-step guide to deploy the `KeeperRegistry` to Stellar testnet and run a
keeper bot against it.

## Prerequisites

- Rust with the `wasm32-unknown-unknown` target:
  ```bash
  rustup target add wasm32-unknown-unknown
  ```
- The [Stellar CLI](https://developers.stellar.org/docs/tools/developer-tools/cli/stellar-cli)
  (`stellar`), formerly `soroban`.
- Node.js ≥ 18 (for the keeper bot).

## 1. Build the contract

```bash
make wasm        # or: ./scripts/optimize.sh
```

This produces `target/wasm32-unknown-unknown/release/keeper_registry.wasm`.

## 2. Create and fund a testnet identity

```bash
stellar keys generate deployer --network testnet
stellar keys fund deployer --network testnet
```

## 3. Deploy

```bash
stellar contract deploy \
  --wasm target/wasm32-unknown-unknown/release/keeper_registry.wasm \
  --source deployer \
  --network testnet
# → prints the deployed CONTRACT_ID (C...)
```

> The repo also ships `scripts/deploy.sh` which wraps these steps. See the
> [troubleshooting guide](#troubleshooting) if a deployment fails.

## 4. Initialize

Pick a reward token — on testnet you can use the native XLM SAC address from
`stellar contract id asset --asset native --network testnet`.

```bash
stellar contract invoke --id <CONTRACT_ID> --source deployer --network testnet -- \
  initialize \
  --admin <DEPLOYER_ADDRESS> \
  --reward_token <TOKEN_SAC_ADDRESS> \
  --fee_bps 300
```

## 5. Register a task (as a dApp)

```bash
stellar contract invoke --id <CONTRACT_ID> --source deployer --network testnet -- \
  register_task \
  --owner <OWNER_ADDRESS> \
  --task_type Liquidation \
  --calldata <HEX_BYTES> \
  --reward 1000000 \
  --deadline <UNIX_TS> \
  --ttl_ledgers 17280 \
  --lock_ledgers 120
```

## 6. Run the keeper bot

The bot can be run as a long-running daemon or as a one-shot process via cron.

### Daemon mode

```bash
cd examples/keeper-bot
cp .env.example .env
# edit .env: KEEPER_SECRET_KEY, REGISTRY_CONTRACT_ID, NETWORK=testnet
npm install
npm start
```

The bot polls for `TaskRegistered` events, reads the current on-chain
`get_fee_bps` value once per round, and estimates the net reward before it
submits `claim_task`. Tasks that do not meet both profitability guardrails are
skipped without submitting a transaction. It then executes profitable tasks
and periodically withdraws accrued rewards. It also expires past-deadline tasks
(`EXPIRE_STALE_TASKS=true`) to refund owners.

### Calibrating profitability settings

`MIN_NET_REWARD_STROOPS` is the minimum amount the keeper expects to retain after
the registry fee. The default is `1000000` stroops (0.1 XLM).

`ESTIMATED_TX_COST_STROOPS` is a deliberately static estimate for one submitted
transaction. Set it from the keeper's observed claim and execute costs. The bot
uses three times this value: claim, execute, and an amortised withdrawal. The
claim simulation's reported resource fee is logged alongside the static estimate
so operators can tune it over time. It is an estimate, not a fee guarantee.

`MIN_PROFIT_MULTIPLE` requires the net reward to exceed the estimated total cost
by that multiple; the default is `2.0`. Increase it when fee volatility or failed
transactions require a larger safety margin. Decrease it only after observing
stable costs. Both checks apply, so a task must clear the minimum net reward and
the profit multiple.

All reward and cost comparisons are performed as integer stroop amounts. The
fee rate is read from the initialized contract rather than relying on any view
fallback value.

### Cron mode

For serverless or cron-based deployments, use the `--once` flag or the
`RUN_ONCE=true` environment variable. The bot will run one round and exit with a
status code indicating success (0) or failure (non-zero).

**Example crontab (runs every minute):**

```crontab
# /etc/cron.d/keeper-bot
* * * * * your-user /path/to/soroban-keeper-network/examples/keeper-bot/run.sh >> /var/log/keeper-bot.log 2>&1
```

You'll need a wrapper script like `run.sh` to `cd` into the right directory and
invoke `node`.

**`run.sh`**
```bash
#!/bin/sh
cd /path/to/soroban-keeper-network/examples/keeper-bot
/usr/bin/node index.js --once
```

This setup ensures that even if one run fails, the next minute's run will try
again, providing resilience without requiring a long-lived process.

## Troubleshooting

The error text below must be verified against the CLI and network version being
used; CLI output can change between releases. Put the observed error text first
when searching logs, then check the cause and fix.

### `Account not found` when deploying

**Cause:** The Friendbot request made by `stellar keys fund` did not complete.
Friendbot can be rate-limited or temporarily unavailable. The deployer's key
can exist locally even though no account exists on the selected network.

**Fix:** Verify the account directly on the same network:

```bash
ADDRESS="$(stellar keys address deployer)"
curl --fail "https://horizon-testnet.stellar.org/accounts/${ADDRESS}"
```

A successful response contains an account record and a `balances` array. If the
account is not found, retry funding and check again:

```bash
stellar keys fund deployer --network testnet
```

Do not continue until the account is visible on the intended network.

### `signature` or account errors after switching networks

**Cause:** The command's `--network` selection does not match the network for
which the account, contract ID, or configured passphrase was created.

**Fix:** Inspect the configured network names:

```bash
stellar network ls
```

Pass the intended network explicitly to funding, deployment, invocation, and
balance checks. Never reuse a testnet contract ID on mainnet or vice versa.

### `WASM exceeds the maximum contract size`

**Cause:** The deploy command was given an unoptimized release artifact. The
network contract code-size limit is 64 KiB (65,536 bytes), and an unoptimized
WASM file can exceed it.

**Fix:** Check the artifact before deploying:

```bash
stat -c '%s bytes' target/wasm32-unknown-unknown/release/keeper_registry.wasm
```

On macOS, use:

```bash
stat -f '%z bytes' target/wasm32-unknown-unknown/release/keeper_registry.wasm
```

Optimize it, then check the resulting file again:

```bash
make optimize
# or: ./scripts/optimize.sh
```

Deploy only an artifact no larger than 65,536 bytes.

### `AlreadyInitialized` (error 1)

**Cause:** `initialize` has already succeeded for that contract ID. This often
happens when a deployment script is rerun after initialization already
completed.

**Fix:** Do not call `initialize` again on that contract. Redeploying creates a
new contract ID; it does not reset the old contract. The old contract remains
live and initialized. If a fresh instance is required, deploy a new WASM
instance and initialize its new contract ID exactly once.

### Deploy succeeds, but later calls fail; `admin()` returns `None`

**Cause:** Deployment and initialization are separate transactions. The deploy
transaction created a live contract, but initialization failed or was never
submitted. An uninitialized registry has no configured admin or reward token.

**Fix:** Detect this state with the read-only admin call:

```bash
stellar contract invoke \
  --id <CONTRACT_ID> \
  --network testnet \
  -- \
  admin
```

If the result is `None`, call `initialize` on that same contract ID using the
correct admin address and reward-token SAC address. Do not redeploy merely
because initialization failed.

### `initialize` rejects the reward token address

**Cause:** `initialize` expects the SAC contract address for the reward asset,
not an issuer address, raw asset code, or account address. SAC addresses are
network-specific.

**Fix:** Derive the native XLM SAC address for the deployment network:

```bash
stellar contract id asset --asset native --network testnet
```

For mainnet:

```bash
stellar contract id asset --asset native --network mainnet
```

Pass the resulting `C...` address as `--reward_token`.

### Generic invocation failure while calling `register_task`

**Cause:** `register_task` transfers the reward from the owner into the
registry during registration. The owner either has insufficient balance or,
for a non-native asset, has not established the required trustline and funded
that asset balance.

**Fix:** Fund the owner on the same network and verify its balance before
registering. For a non-native asset, establish the trustline and fund the
corresponding asset balance. Retry only after the owner can make the required
token transfer.

### `XDR error` or other obscure XDR errors from the Stellar CLI

**Cause:** The installed `stellar-cli` is older than the Soroban SDK version
used by this repository. Older CLI releases can fail to encode or decode the
contract's current XDR types correctly.

**Fix:** This repository requires `stellar-cli` 22.x or newer. Check the
installed version:

```bash
stellar --version
```

Update the CLI and confirm the version again:

```bash
cargo install --locked stellar-cli --features opt
stellar --version
```

## Verifying a deployment

Add the contract to a block explorer link for your application:

```
https://stellar.expert/explorer/testnet/contract/<CONTRACT_ID>
```