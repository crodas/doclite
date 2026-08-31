# ADR 0003: An Operation Is a Value, Not a Closure

## Status
Accepted

## Context
The obvious transaction API is `db.transaction(|tx| ..)`. It is also the one
that gives up the most. A closure body cannot be inspected, so the engine cannot
tell whether it writes and cannot choose a lock mode. It cannot be safely
re-run, because it may have sent an email, so conflict handling has to leave the
storage layer. And it cannot be bounded, because there is no work to look at
until it is already running.

## Decision
An `Operation` is an ordered list of `Step`s built before anything runs, and
`Db::run` is the transaction boundary. `Operation::then` concatenates two step
lists, so the boundary is a value that can be passed around and combined rather
than a pair of instructions a caller can get wrong.

Preconditions and mutations are one list, not two. A `Step::Require` placed
after a mutation observes that mutation. There is no typestate separating the
halves, because separating them would cost read-your-own-writes.

There is deliberately no `db.transaction(|tx| ..)`, and adding one later would
undo this ADR rather than extend it.

## Consequences
`Db::run` retries on conflict by walking the operation again, which is safe
precisely because the operation has no side effects to repeat.

An unbalanced transaction is not constructible. Three checks a verifier would
otherwise owe (exactly one begin, commit requires an open transaction, return
requires a closed one) have nothing left to check.

`Step`, `ReadPlan` and `Column` are plain data with no builder types and no
engine types in them, kept in their own modules. That separation is cheap now
and is the whole cost of keeping a verifiable, encodable representation
available if an untrusted caller ever has to submit one. See spec section 7 for
the single condition that would trigger it.

`for_each` is the one combinator with real data dependence, and the only one
that can do unbounded work, so its scan must carry a limit and `check` refuses
it otherwise. Its body is called once, at build time, with a `RowRef` that hands
out `Cell`s carrying no `PartialEq`, so a template can construct and cannot
branch.

`Bind` is the run-time half of the same rule: it fills the slots a template
left, and `symbol_allowed` refuses one written where no row will ever exist.
Both live in one module because changing either without the other is a bug.
