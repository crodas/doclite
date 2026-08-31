# ADR 0005: Read-Your-Own-Writes by Overlay, Commit by Pre-State Assertion

## Status
Accepted

## Context
An operation's steps are one ordered list, so a precondition can sit after a
mutation and must see it. A batch, meanwhile, is judged against the state before
it is applied, because that is what lets one condition mean both "nothing may
hold this key" and "this key, which I am about to release". Those two demands
look contradictory, and the usual way out is a transaction handle the executor
drives step by step.

A handle is the wrong price. It has to name a backend type this crate cannot,
it breaks object safety, and a single-connection engine deadlocks on the first
read taken inside one.

## Decision
The executor keeps a `Delta` of everything the operation has staged. A
precondition reads the engine, folds the delta over the answer, and judges the
result. The engine sees nothing until `apply`.

What crosses the gap between reading and committing is not a lock but an
`Assertion` per observation. `apply` re-evaluates every one against pre-batch
state and answers `Applied::Conflict` if any has moved; `Db::run` then discards
the delta and walks the operation again.

An assertion records what the precondition actually looked at. An untouched
index yields `Observed::Absent` or `Observed::Count`, because that is all the
step observed. Once anything is staged against that index the count no longer
says what was seen, so the whole committed row set is asserted instead.

The delta is replayed in written order rather than collapsed, because a delete
and an insert over the same key mean opposite things depending on which came
first. `Delta::into_batch` then drops any insert a later delete covers, which is
what lets an engine apply every delete before every insert and still get the
written order's meaning.

## Decision, consequence
The lock mode is settled by which method is called rather than by a flag on the
trait. An operation with nothing staged never reaches `apply`, so an engine
takes its write lock in exactly one place. Spec section 6 describes this as
inspecting the operation for writes; the method split is the same choice made
one layer earlier.

## Consequences
Concurrency is optimistic, and a hot key costs re-walks. `Db::with_attempts`
bounds them, and exhausting them is `StorageError::Conflict`, not a silent
failure.

The engine trait stays three methods and stays object-safe-shaped, and an engine
never has to hold state between calls on a caller's behalf.

Asserting no more than what was observed is what keeps a `require_count` on a
busy range from conflicting with every unrelated writer.
