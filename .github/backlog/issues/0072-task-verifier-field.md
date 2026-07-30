---
title: "feat(registry): add an optional verifier field to Task"
labels: [contract, enhancement, intermediate]
epic: E04
wave: 2
depends_on: [0071]
---

## Summary

The first concrete step of the design from 0071: add `verifier: Option<Address>` to the `Task` struct, unused by any logic yet. This issue is deliberately scoped to just the schema change plus migration considerations, kept separate from 0073/0074 (which make the field actually do something) so the storage-layout change gets reviewed on its own.

## Expected behaviour

- `Task` gains `pub verifier: Option<Address>`.
- Every existing constructor of `Task` in `register_task` sets it to `None` until 0073 adds a way to set it otherwise.
- `#[contracttype]` field addition is backward-compatible for new writes.
- Existing persisted entries must not be assumed to deserialize automatically after this change.

## Schema-evolution finding

The repository's contract source and manifest must be checked against the exact `soroban-sdk` version used by the contract before asserting a migration behavior. In particular, the implementation must verify whether the SDK's `#[contracttype]` decoder accepts an older positional struct encoding when a field is appended.

If the SDK does not supply a default for an omitted trailing field, an already-persisted `Task` written with the old schema cannot be decoded as the new `Task` with `verifier == None`. In that case, deploying the new implementation and then reading old entries is not a viable migration strategy: decoding can fail before the value can be rewritten. An explicit migration must be designed and reviewed before upgrading an existing deployment, or the change must be limited to deployments with no existing `Task` records.

The migration conclusion must be backed by an SDK-version-specific test or authoritative SDK documentation. Once confirmed, the result should be documented here and coordinated with the migration work tracked by issue 0138.

## Suggested approach

Check the exact `soroban-sdk` version declared by the contract and its documented behavior for schema evolution of `#[contracttype]` structs. Specifically verify whether an old, already-persisted `Task` can still be deserialized after this change is deployed via `upgrade`. If it cannot, provide a migration path before deploying the new schema.

## Acceptance criteria

- [ ] `Task.verifier: Option<Address>` added.
- [ ] All existing constructors in `register_task` set `verifier` to `None`.
- [ ] All existing tests pass apart from any required construction updates.
- [ ] A test specifically constructs a `Task` without a verifier and confirms `get_task` round-trips the newly written value with `verifier == None`.
- [ ] The schema-evolution question is answered for the exact `soroban-sdk` version in use.
- [ ] If old entries cannot be decoded, a storage migration path is designed and reviewed before upgrading an existing deployment.
- [ ] Follow-up migration work is tracked by issue 0138 when required.

## Files

- `contracts/keeper-registry/src/lib.rs`
- `contracts/keeper-registry/src/test.rs`
