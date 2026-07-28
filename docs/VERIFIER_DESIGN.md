# Verifier Design

## Status

Decision record for verifier failure handling. This document unblocks implementation of `execute_task` verifier calls.

## Interface

A verifier is an optional contract attached to a task. The verifier exposes the following Soroban contract entry point:

```rust
pub fn verify(env: Env, task: Task, proof: Bytes) -> bool
```

The verifier returns `true` when the proof is acceptable and `false` otherwise. The task and proof are passed by value at the contract ABI boundary; generated Soroban clients may use references in their Rust-facing method signatures.

A task without an attached verifier retains the existing behavior: `execute_task` does not perform an external call and proceeds with the normal reward accounting after its existing checks.

## Investigation: panics in cross-contract calls

Soroban does not use Rust unwinding to isolate a callee. A panic in a contract is a VM/host trap, not a Rust panic that a contract can catch with `std::panic::catch_unwind`. Contract code is compiled with `no_std`, and Soroban does not provide a general-purpose panic-catching mechanism.

Soroban exposes a fallible invocation API:

- [`Env::try_invoke_contract` in soroban-sdk 22.0.1](https://docs.rs/soroban-sdk/22.0.1/soroban_sdk/struct.Env.html#method.try_invoke_contract)
- [`Env::invoke_contract` in soroban-sdk 22.0.1](https://docs.rs/soroban-sdk/22.0.1/soroban_sdk/struct.Env.html#method.invoke_contract)

`try_invoke_contract` is intended for handling an invocation that returns an error value. It does not turn an arbitrary callee VM panic/trap into a recoverable verifier result. A verifier panic therefore propagates as a failed host invocation and aborts the calling transaction; registry state changes from that transaction are rolled back.

This distinction is important:

- A verifier returning `false` is an ordinary successful invocation and must be handled as a typed verification rejection.
- A verifier returning a contract error may be handled through Soroban's fallible invocation API where supported by the chosen interface.
- A verifier panicking or otherwise trapping is not safely catchable by `execute_task`; the transaction fails and no payout state is committed.
- Budget exhaustion and other transaction-level host failures also abort the transaction and must not be treated as successful verification.

## Decision

`execute_task` must treat verifier results as follows:

1. `None` verifier: preserve the current execution path exactly.
2. Verifier returns `true`: continue with the existing reward split, keeper credit, task transition, and event emission.
3. Verifier returns `false`: return the typed verification-failed error specified by issue 0080. The task remains `Claimed`; no reward is credited or transferred.
4. Verifier panics or traps: allow the invocation failure to abort the transaction. No reward is credited or transferred, and the task remains unchanged because the transaction is rolled back.
5. A transaction-level failure such as exhausted budget may also abort the transaction. It must not be represented as a successful verification or cause partial reward accounting.

The implementation must not use `catch_unwind` or depend on Rust panic behavior. It may use `Env::try_invoke_contract` only for invocation errors that Soroban exposes as recoverable results; it must not assume that this API catches a callee VM panic.

## Recovery and denial of service

A panicking verifier can prevent successful execution attempts, but it cannot permanently consume the escrow merely by causing verification attempts to fail:

- A failed verifier invocation does not make the task `Executed`.
- The failed transaction does not credit or transfer the reward.
- The task remains `Claimed` after the failed transaction.
- Once the deadline passes, the existing permissionless `expire_task` path can refund the owner.

Thus a panicking verifier causes a liveness delay until the deadline, but it does not permanently strand the escrow under the current lifecycle rules. No separate maximum-failed-attempts or force-cancel mechanism is introduced by this decision record. Any such mechanism would be a separate protocol change and follow-up issue.

This conclusion depends on the task record remaining available until expiry. The existing storage-TTL/deadline invariant is tracked by issue 0005 (`ttl_ledgers` must cover the task deadline and expiry window). That issue must be resolved for the general escrow-recoverability invariant to hold for arbitrarily long-lived tasks.

## Consequences for issue 0074

The implementation in issue 0074 should call the attached verifier using the interface defined above. A returned `false` must map to the typed verification-failed error and must leave the task `Claimed` without crediting or transferring the reward. A callee panic is expected to abort that transaction; it must not be falsely treated as approval or as a successful payout.

The no-verifier path must not invoke an external contract and must remain backward compatible with tasks created before verifier support.
