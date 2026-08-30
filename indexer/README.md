# Keeper Registry Indexer

Indexes the keeper registry's on-chain events into a queryable store, and
serves them over a REST API and a live WebSocket feed.

## Design

The `events` table is append-only and authoritative. Every current-state
answer — a task's status, a keeper's balance, the live fee — is folded from
that event history on read, never kept as separate mutable rows. A derived
view therefore cannot drift from the events that produced it, and replaying
the same events always yields the same state.

Ingestion polls the RPC's `getEvents`, the mechanism the keeper-bot already
uses. Backfill and steady-state polling share one parsing path
(`ingest::Ingestor::ingest_batch`); the only difference between them is the
ledger range being walked.

Ingestion is idempotent. A `(tx_hash, event_index)` pair identifies an
emission uniquely, so re-reading a ledger — an overlapping backfill page, a
retried poll, a restart mid-page — stores nothing twice.

## Configuration

Every variable is validated at startup. A misconfigured service reports all
problems at once and exits, rather than failing later inside the ingest loop.

| Variable | Required | Default | Meaning |
| --- | --- | --- | --- |
| `INDEXER_RPC_URL` | yes | — | Soroban RPC endpoint to poll |
| `INDEXER_CONTRACT_ID` | yes | — | Registry contract id to filter on |
| `INDEXER_DATABASE_URL` | yes | — | sqlx connection string, e.g. `sqlite://indexer.db` |
| `INDEXER_START_LEDGER` | yes | — | Ledger to backfill from on a fresh database |
| `INDEXER_BIND_ADDRESS` | no | `127.0.0.1:8080` | API bind address |
| `INDEXER_POLL_INTERVAL_SECS` | no | `5` | Seconds between polls once caught up |
| `INDEXER_BACKFILL_PAGE_SIZE` | no | `200` | Ledgers per page during backfill |
| `INDEXER_LOG` | no | `info` | `tracing` filter directive |

`INDEXER_START_LEDGER` should be the contract's deployment ledger. On a
network where that is not known exactly, any ledger at or before the
`initialize` call works: ingestion is idempotent, so starting early costs
extra scanning rather than correctness.

## Running

```bash
export INDEXER_RPC_URL=https://soroban-testnet.stellar.org
export INDEXER_CONTRACT_ID=C...
export INDEXER_DATABASE_URL=sqlite://indexer.db
export INDEXER_START_LEDGER=1000

cargo run -p keeper-indexer
```

## Tests

```bash
cargo test -p keeper-indexer
```

Tests run against an in-memory SQLite database and a fixture event source, so
no RPC endpoint or external database is needed.

## Event coverage

All fifteen events the contract emits are ingested with their exact payload
fields, in the order `contracts/keeper-registry/src/events.rs` publishes them:

`TaskRegistered`, `TaskClaimed`, `TaskExecuted`, `TaskExpired`,
`TaskCancelled`, `RewardIncreased`, `DeadlineExtended`, `RewardsWithdrawn`,
`Paused`, `FeeUpdated`, `AdminTransferred`, `MinRewardUpdated`, `FeesSwept`,
`Initialized`, `Upgraded`.

Fields the events do not carry are not reconstructed. `TaskClaimed` has no
reward, so a consumer needing the claim and the reward together joins against
the task's `TaskRegistered` event rather than reading an invented value.

## Caching

The aggregate queries the API exposes — currently the keeper leaderboard — are
served from a short-TTL cache (`src/cache.rs`).

| Variable | Default | Meaning |
|---|---|---|
| `INDEXER_CACHE_TTL_SECS` | `10` | Staleness bound for cached aggregates. `0` disables caching entirely. |

The TTL **is** the guarantee: no cached response is ever older than it. At the
default of 10 seconds, against a ~5s ledger close, a viewer can be at most two
ledgers behind — less than the time it takes to read the page.

A TTL is used rather than explicit invalidation because the event that would
invalidate the leaderboard (`task_executed`) is the one that arrives
continuously while the indexer is caught up: an invalidate-on-write cache would
spend most of its life empty, and would be emptiest exactly when traffic is
highest. Point lookups are not cached — their cost does not grow with traffic
the same way, and they are the reads most likely to be checked right after a
write.
