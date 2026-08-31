# ADR 0011: Two Ways to Be an Engine

## Status
Accepted

## Context
ADR 0004 made `Engine` a wide seam on purpose. An engine is handed a whole
`Batch` and decides for itself whether it lands: which assertions still hold,
what refuses it, and the order it is written in. That is exactly right for a
backend with a query language and a transaction manager of its own, which can
answer the whole thing in a statement and would only be slowed down by being
told how.

It is far too much to ask of a substrate that keeps ordered bytes and nothing
else. An in-process map, RocksDB, sled and redb have no way to be told about a
batch except one row at a time, so each of them would write out the same
protocol again: evaluate every assertion against pre-batch state, refuse an
object id already held, check that every row's width matches its index, then
write deletes before objects before index rows. Four re-implementations of the
same rules, none of which can see the others, each free to get a detail subtly
wrong in a way the suite would only catch if someone thought to test it.

## Decision
Two seams, and a backend picks the one that matches what it has.

`Engine` is unchanged: `define`, `read`, `apply`, async as ADR 0006 put it.
Everything about what a batch means stays the implementer's.

`Store` is the narrow one. `define` declares a home per index, `read` answers
every scan and object from one view, and `exclusive` hands out a `Region`.
`StoreEngine<S>` wears the engine seam over it, and the batch protocol lives
behind that and nowhere else, as `admits` and `commit`.

```text
Engine ─┬─ SQLite, Postgres      implement Engine, use their own SQL
        └─ StoreEngine<S> ─── S  implement Store, keep ordered rows
```

A `Region` is the store held still, long enough to judge one batch and write it.
Its reads are fallible and its writes are not, and that split is the whole of
atomicity here: everything that can refuse the batch is asked before anything
moves, so there is never a half-written region to unwind. A store contributes
where rows are kept and contributes nothing about what a batch means.

`Store` is synchronous because the substrates it is for are. A store that waits
on a network implements `Engine` directly, where ADR 0006 already put the async.

## Consequences
The protocol is testable without a store. `admits` and `commit` are pure over a
`Region`, so they are checked against a hand-written one rather than through a
store, a runtime and an executor.

The two halves of an assertion share one definition. The executor records what
it observed while reading and a region re-evaluates it at apply time, and both
go through `Observed::holds` rather than agreeing by inspection.

`MemoryEngine` is a type alias. It is `StoreEngine<MemoryStore>`, and what used
to be its batch handling is gone rather than moved.

A `Store` ignores `ColumnType` entirely. It keeps bytes, so a column's declared
type is only ever of interest to a backend that can declare a real column, and
ADR 0002 already settled that ordering does not depend on it.

Both kinds run the same suite. `conformance!` verifies an `Engine`, and a
`StoreEngine` is one, so a store gets the rules checked without the testkit
knowing which seam it was built on.

A store pays for judgment it might have been able to delegate. `StoreEngine`
decides a batch in Rust, row by row, where a backend with its own engine could
have asked for the whole thing at once. That cost is the reason the wide seam is
still there rather than being replaced by this one.
