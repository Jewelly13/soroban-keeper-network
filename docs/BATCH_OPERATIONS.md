# Batch Operations — Feasibility Study (E05)

This document is the feasibility study backlog issue 0099 asked for: whether
batching `claim_task` and/or `execute_task` is worth building, given that
Soroban transactions are atomic (a call either fully commits or fully
reverts). It does **not** implement anything — the output is a
recommendation, per the issue's acceptance criteria.

Batch *registration* (issue 0097/0098) is out of scope here; it is a
different problem because every task in that batch shares one owner and none
of them interact with another actor's state. Claiming and executing both
touch state a competing actor (another keeper, or a task's verifier) can also
touch, which is what makes them worth studying separately.

## The core risk, restated precisely

`claim_task`'s permissionless first-come-first-served design (see
`contracts/keeper-registry/src/lib.rs`, `claim_task`) checks one task's
current status and, if it is `Pending` (or `Claimed` with an expired lock),
flips it to `Claimed` by the caller. That check-and-set is atomic *per task*
today because each `claim_task` call is its own transaction.

A hypothetical `batch_claim(task_ids: Vec<u64>)` spanning task A and task B,
submitted as one transaction, would either commit for both or revert for
both — Soroban has no notion of "commit the sub-operations that succeeded and
roll back only the ones that failed" within a single contract invocation. If
another keeper's `claim_task(B)` lands in an earlier transaction of the same
ledger close (or even the same one, depending on execution order), the
entire batch reverts, including the claim on task A that would have
succeeded had it been submitted alone.

## Question 1 — Does batch claiming help in practice, or only where the benefit is smallest?

**It only helps where the benefit is smallest, and actively hurts everywhere
else.** Reasoning:

- Model each candidate task in the batch as having an independent
  probability `p` that some other keeper claims it before this transaction
  lands, and treat `p` as one proxy for "how contested is this market" (`p`
  near 0 in a quiet market, `p` growing as more keepers compete for the same
  pool of tasks). For a naive all-or-nothing `batch_claim` of size `N`, the
  probability that the whole batch reverts is `1 - (1 - p)^N` — it grows
  with both contention and batch size. At `p = 0.1` and `N = 5`, a keeper
  that would individually win ~90% of its claims now fails the *entire*
  batch roughly 41% of the time, losing tasks it would have won standing
  alone.
- In a **busy market** (`p` non-trivial), batching converts "I win most of
  my races" into "I lose the whole batch if I lose any single race in it."
  That is strictly worse than submitting `N` independent `claim_task` calls,
  where a loss on task B has zero effect on the outcome for task A. Batching
  does not help a keeper compete harder here — it makes competing *harder*.
- In a **quiet market** (`p` near 0 — supply of claimable tasks exceeds
  keeper demand), the atomicity risk is negligible because collisions are
  rare regardless of batching. But that is exactly the regime where a
  keeper didn't need batching to win its claims in the first place: each
  individual `claim_task` would have succeeded on its own with near
  certainty. The only thing batching buys there is amortizing per-transaction
  overhead (base fee, one signature/submission instead of `N`) — a real but
  modest saving, not a competitiveness advantage, and one that matters least
  to a keeper precisely because contention (and therefore urgency) is low.

So batch claiming's only upside shows up exactly where the issue's summary
predicted: low-contention scenarios where the benefit is smallest. In any
market worth competing in, it is strictly worse than the status quo.

## Question 2 — Is there a version that sidesteps this?

**Yes: `claim_first_available(candidates: Vec<u64>) -> Result<u64, KeeperError>`.**
Rather than trying to claim *all* of several candidates (all-or-nothing),
this claims *whichever one* of the candidates is still available: it walks
the candidate list in order, applying `claim_task`'s existing check-and-set
per entry, and returns as soon as one succeeds (or a typed error once the
list is exhausted). Success is defined per-call as "claimed at least one,"
not "claimed all," so a collision on `candidates[0]` falls through to
`candidates[1]` instead of reverting the whole transaction.

This directly removes the atomicity problem from Question 1: the keeper only
loses entirely if *every* candidate it listed has already been claimed by
someone else — strictly better than or equal to a single-candidate
`claim_task`, and strictly better than the naive batch. It is also better
than the keeper doing the equivalent off-chain — retrying candidates one at
a time in separate transactions after each `InvalidTaskStatus`/`LockPeriodActive`
failure — because that approach leaves a window between attempts (another
ledger close, another keeper's transaction) where a competitor can claim the
next candidate before the retry lands. `claim_first_available` closes that
window by trying the fallback candidates synchronously, inside the same
transaction, at the same ledger.

Cost-wise, this is cheap: at most `N` status reads plus one write (the
winning claim), versus `N` independent transactions' worth of base fee and
submission overhead for the sequential-retry alternative.

Issue 0099's own dependency graph already anticipated this outcome: backlog
issue **0101** (`claim_first_available`, in
`.github/backlog/issues/0101-batch-claim-first-available.md`) is written
conditionally — "implement it if 0099 recommends it, otherwise close as not
applicable." This study's conclusion is that condition is met, so 0101
should be picked up as-is rather than a new issue being drafted for it.

## Question 3 — Does the same atomicity risk apply to batch execute?

**Not the same risk, but a real one, of a different shape.**

`execute_task` already checks `task.claimer.as_ref() != Some(&keeper)`
before crediting — a competing keeper cannot claim a task this keeper
already holds, so re-claim collision is not a failure mode for a batch of
`execute_task` calls the way it is for `claim_task`. If a keeper holds
exclusive claims on tasks A and B, nothing another keeper does can make a
`batch_execute([A, B])` fail because of A or B being contested.

Two different risks do apply, though:

1. **Verifier failure (epic E04, not yet built in this repo).** Once a task
   can carry an optional per-task verifier (`IKeeperVerifier::verify`, per
   `docs/VERIFIER_DESIGN.md`), an all-or-nothing `batch_execute` would
   revert the entire batch if *any* task's verifier returns `false` or
   panics — even though the keeper did legitimate work and holds a valid
   claim on every other task in the batch. That blocks crediting for work
   that was entirely valid, solely because of one unrelated task's verifier
   outcome. This is a real instance of the same "all-or-nothing punishes
   the unrelated majority" problem Question 1 identified for claiming.
2. **Aggregate resource cost.** Batching `N` `execute_task` calls into one
   transaction sums their CPU/memory/storage-write cost against a single
   transaction's resource ceiling. A verifier's cost is not fixed or
   controlled by the registry (see issue 0076/0100/0113 — verifier resource
   cost is measured, not capped, as of this writing); a large enough batch,
   or one that happens to include an expensive verifier, could fail purely
   on resource limits, for reasons unrelated to any task's validity.

Unlike claiming, there is no "try the next one instead" sidestep here,
because execute isn't a race among interchangeable options — each task's
proof is specific to that task and not substitutable. What *would* sidestep
the all-or-nothing revert is a **best-effort** batch execute: attempt every
task in the batch, credit the ones whose verifier passes, and skip (not
revert) the ones that fail, returning a per-task result so the caller can
see individual outcomes. This is the execute-side analogue of
`claim_first_available` — partial success expressed as the actual outcome,
rather than forced into all-or-nothing.

Whether that is buildable depends on two things this repo does not have yet:

- **Epic E04 itself** (issues 0071–0096: the verifier interface, the three
  reference verifiers, and — critically — issue 0075's verifier
  failure-handling policy, which decides what "a verifier fails" even means
  today for a single `execute_task` call). Best-effort batching cannot be
  designed before its single-call failure semantics are pinned down.
- **Confirmation that Soroban's cross-contract call semantics let a caller
  catch a callee's panic without the panic unwinding the caller's own
  invocation.** If a verifier panicking during `execute_task` currently
  aborts the whole host invocation with no way for the registry to catch it
  and continue to the next task, "best-effort" is not implementable as
  described and the only remaining options are naive all-or-nothing (bad,
  per the reasoning above) or not building batch execute at all. This needs
  to be verified against the actual SDK/host behavior as part of
  implementing 0075, not assumed here.

Because those prerequisites don't exist in this repo yet, batch execute's
real risk (verifier failure) doesn't even apply today — there is no verifier
to fail. Recommending implementation now would mean designing against a
failure mode that doesn't yet exist and guessing at semantics 0075 hasn't
settled. That is exactly the kind of premature scope this study should not
create.

## Recommendation

| Idea | Build it? | Why |
|---|---|---|
| Naive `batch_claim` (all-or-nothing) | **No.** | Strictly worse than independent claims in any contested market; only "helps" where help isn't needed. |
| `claim_first_available(candidates)` | **Yes.** | Removes the atomicity downside entirely; cheaper than sequential retries; backlog issue 0101 already exists, gated on this exact conclusion — proceed with it as written. |
| Naive `batch_execute` (all-or-nothing) | **No, not now.** | Its real risk (a bundled verifier failure reverting unrelated valid work) doesn't exist yet, since epic E04 hasn't shipped. Building against a failure mode that isn't real yet risks guessing wrong. |
| Best-effort `batch_execute` (skip failures, credit the rest) | **Investigate after E04 lands**, specifically after issue 0075 (verifier failure-handling policy) settles what a single verifier failure means, and after confirming Soroban lets a caller catch a callee panic without aborting its own invocation. | See backlog issue 0201, filed alongside this study. |

New backlog issue **0201** (`batch-execute-best-effort-feasibility`) has been
added, scoped narrowly to "investigate + design a best-effort `batch_execute`,
conditional on epic E04 and issue 0075 landing first" — this keeps that
follow-on work out of this study's scope rather than silently expanding
0099 from study to implementation.

## Summary

- Batch claim: don't build the obvious version; build `claim_first_available`
  instead (issue 0101, already scoped and unblocked by this conclusion).
- Batch execute: don't build yet; the interesting version (best-effort,
  skip-on-failure) can't be designed responsibly until epic E04's verifier
  failure semantics exist. Filed as issue 0201, gated on that dependency.
