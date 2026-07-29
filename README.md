# Soroban Keeper Network

> **The decentralized automation & upkeep layer for the Stellar/Soroban ecosystem.**
> Chainlink Keepers — but native to Soroban.

[![CI](https://github.com/soroban-tooling/soroban-keeper-network/actions/workflows/ci.yml/badge.svg)](https://github.com/soroban-tooling/soroban-keeper-network/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Built on Soroban](https://img.shields.io/badge/built%20on-Soroban-blueviolet)](https://soroban.stellar.org)

## Documentation

| Doc | What's inside |
|-----|---------------|
| [Live demo](docs/DEMO.md) | Deployed testnet contract and full on-chain transaction trace |
| [Architecture](docs/ARCHITECTURE.md) | Components, task lifecycle, storage, money invariants, and trust model |
| [Deploying & running](docs/DEPLOYING.md) | Testnet deployment walkthrough and keeper-bot operator guide |
| [Deployments](DEPLOYMENTS.md) | Canonical record of on-chain addresses |
| [Security policy](SECURITY.md) | How to report a vulnerability |

## What it does

Task owners register jobs with a token reward and execution conditions. Keepers discover eligible jobs, claim them, perform the off-chain work, and submit the execution transaction. Successful execution credits the keeper with the reward after the protocol fee; cancellation and expiry return escrow to the owner.

The repository contains the Soroban registry contract and an example JavaScript keeper bot.

## Security considerations

The contract's security is defined by the money invariants in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#money-invariants). That section is the canonical specification for solvency, escrow recoverability, single payout, fee bounding, escrow isolation, withdrawal liveness, and monotonic task ids. It also identifies the contract functions that enforce each property, concrete changes that would break them, and known gaps tracked in the issue backlog.

Implementation mechanisms such as authorization, checked arithmetic, status guards, and checks-effects-interactions are means of preserving those properties; they are not a substitute for reviewing the properties themselves. Any change involving token transfers, task terminal states, fee accounting, pausing, storage TTL, or task-id allocation must be checked against the architecture invariants.

Known open issues include the relationship between task TTL and deadline ([#0005](https://github.com/soroban-tooling/soroban-keeper-network/issues/5)) and the historical CEI ordering concerns in cancellation and expiry ([#0002](https://github.com/soroban-tooling/soroban-keeper-network/issues/2), [#0003](https://github.com/soroban-tooling/soroban-keeper-network/issues/3)).

Report suspected vulnerabilities according to [SECURITY.md](SECURITY.md), rather than opening a public issue with exploit details.

## Repository layout

```text
contracts/keeper-registry/  Soroban keeper registry contract
examples/keeper-bot/         Example keeper bot
a fuzz/                        Fuzzing targets and shared support code
docs/                        Architecture, deployment, and demo documentation
```

## Development

### Prerequisites

- Rust stable (1.78 or newer)
- `wasm32-unknown-unknown` Rust target
- Soroban/Stellar CLI 22.x or newer
- Node.js 18 LTS or newer for the example bot

### Common commands

```sh
make build       # Build the workspace
make test        # Run the contract test suite
make fmt-check   # Check formatting
make wasm        # Build the release WASM contract
make ci          # Run the required CI checks locally
```

The contract can also be tested directly with `cargo test --workspace --locked`.

## Contributing

Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. In particular, changes that move funds or alter task lifecycle behavior must be reviewed against the numbered invariants in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#money-invariants), and tests should name the invariant they protect.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for the full text.
