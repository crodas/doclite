# ADR 0013: A SQLite Engine, and Letting It Plan

## Status
Accepted

## Context
There is one engine and it keeps everything in memory. A durable backend is the
point of the engine seam existing, and SQLite is the one worth building first:
it is embedded, it is everywhere, and it has a query planner and a transaction
manager of its own.

ADR 0011 already says a backend like that implements `Engine` rather than
`Store`. It gives the reason as freedom, and there is a harder one underneath.
`Region`'s writes are infallible and it has no commit method, so a backend whose
commit can fail has nowhere to report it: the write would happen as the region
drops, after `Applied::Committed` has already been returned. The narrow seam
cannot carry a durable engine without lying about durability.

## Decision
The engine implements `Engine` directly, and reproduces the batch protocol in
SQL rather than borrowing it. The protocol lives in one private module of sixty
lines, so this is a transcription rather than a design.

One table per index, one SQL column per key column, and the object id stored as
an `INTEGER`. Per-column storage is forced by ADR 0002: a key is `Vec<Vec<u8>>`
compared elementwise, and elementwise order is not the order of the
concatenation. The integer is bought by ADR 0014: an id reserves its top bit, so
none of them bit-casts negative and sorts ahead of a smaller one. `ColumnType`
becomes a real column where the type both fits and orders the same way, and
bytes everywhere else.

The connection lives on a thread the engine owns and is reached over a channel.
ADR 0006 keeps the choice of runtime with the caller, so the async cannot come
from a runtime-specific blocking pool or from a driver that picks one.

**An engine may also be handed the read plan, and may decline it.** Today the
executor takes a `ReadPlan` apart before any engine sees it, so a `Count` is
issued as a scan and counted in Rust, and the two halves of a `Zip` arrive as
unrelated scans. A `for_each` is expanded row by row before the batch is built.
A new method offers the plan and defaults to declining. It is a hint in the
sense ADR 0004 already defines: `Probe` strips it along with `range` and
`limit`, and `hints_are_hints` proves a planning engine and a declining one
answer identically.

A second offers an unexpanded `for_each`, and not on the same terms. A plan may
be offered whenever one is read; a sweep only when the `for_each` is the whole
operation, which `Batch` says by making it a variant rather than a field. Expanding a template stages into the overlay that a later
precondition folds, so deferring one in an operation that has a later step would
change what that step observes rather than only how the work was done. With a
single step there is nothing earlier for the scan to have missed and nothing
later to observe what it wrote, and the scan and its writes land in one
transaction, so the assertion the executor would have recorded has nothing left
to protect.

`docs/sqlite.md` is the engine. `docs/plan-pushdown.md` is the seam.

## Consequences
The wide seam finally has a second inhabitant, which is what tells us whether it
was the right shape. Until now every `Engine` in the workspace was either the
adapter over a store or a decorator over another engine.

A batch is writes or a sweep and cannot be both, so where a sweep goes in the
step order is not a question any engine answers. It was a field first, and the
one failure in `doclite-store` that returned success was a store ignoring it.

A deferred `for_each` runs inside the transaction that commits it, so its scan
and the writes derived from it are one atomic act and the assertion the executor
would have recorded has nothing left to protect. That is ADR 0005's argument
applied one level down: an assertion carries consistency across a gap, and here
there is no gap.

An engine can now be wrong in a way review will not catch, by planning a query
and computing a different answer than the flat path. The existing hint property
is what catches it, which is why the seam is defined as a hint rather than as a
capability.

A typed column is a promise about ordering and a promise about round-tripping
bytes, and the conformance suite declares every column as bytes, so it checks
neither. The engine owes itself a test per column type or the mapping is
unverified by construction.

Reads serialize behind writes while there is one connection. WAL allows
concurrent readers on separate connections, so a reader pool is the obvious
later change and is deliberately not the first version.
