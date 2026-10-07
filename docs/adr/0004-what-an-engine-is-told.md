# ADR 0004: What an Engine Is Told, and the Three Things That Are Not Hints

## Status
Accepted

## Context
A backend boundary can be drawn in two places. Either every argument is an
obligation, and each new engine has more to get exactly right, or arguments are
advice, and the layer above re-checks everything. The first makes engines fast
and fragile. The second makes them easy to write and easy to test, at the cost
of some work done twice.

## Decision
Everything an `IndexRead` carries beyond the index itself is a hint. The test is
what happens when an engine ignores one: it returns a superset, which the
executor then filters, so ignoring costs time and never correctness.

| Told | Cost of ignoring it |
|---|---|
| `range` | a superset; the executor filters |
| `limit` | every matching row; the executor trims |
| `Engine::plan` | the read is taken apart into flat scans instead |
| `Engine::takes_sweep` | the executor expands the template row by row |
| row order | the wrong answer |
| `apply` atomicity | a corrupt store |
| `apply` step order | a corrupt store |

`Engine::read` takes a `Reads` of scans and object ids and answers both from
one view. `Answers` is positional: one row set per scan and one slot per
object id, each in the order asked, so which answer belongs to which request
is not something a caller works out. An object that is not there is a `None`
slot rather than an omission.

`Engine::apply` evaluates every assertion against pre-batch state, then deletes
index rows, then inserts objects, then inserts index rows.

There were three hints once. A candidate list, naming the only objects whose
rows were wanted, was specified and never produced: every `IndexRead` the
executor built left it empty, so no engine could be shown to have got it wrong
and no caller ever benefited. A hint nothing sends is not a hint, so it is gone.

The two that survived were specified before they were built, and each took its
row in the table when its method appeared rather than when its design did, which
is the same rule applied forwards: a hint nothing sends is not one.

`Engine::plan` hands an engine the whole `Plan` rather than the flat scans it
would otherwise be taken apart into, so a count is counted where the rows are
and the halves of a `Plan::Zip` can be one statement instead of two reads that
have to be trusted to agree. `Engine::takes_sweep` is narrower on purpose: a
sweep is offered only when the `for_each` is the whole operation, because
expanding a template stages into the overlay a later precondition folds. Both
default to declining, which is what keeps them hints rather than obligations.
ADR 0013 is where the SQLite engine takes them up, and `docs/plan-pushdown.md`
is the design.

`doclite-testkit` is where this stops being a comment. `Probe` wraps any engine
and strips every hint before forwarding, and `hints_are_hints` runs the same
script bare and probed and demands identical transcripts.

## What an engine answers
The table is about what an engine is *told*. What it *answers* is a different
thing and is never a hint, because an engine cannot ignore its own answer.

`Engine::define` answers a `Defined`, naming the indexes it has no complete set
of rows for. Ignoring a hint widens what comes back and costs time; an engine
that calls an index complete while its rows are missing has saved no work and
made the next query wrong. `Engine::rebuilt` is the other half and is the same
kind of thing. `Probe` forwards both untouched where it blanks every hint, and
that is the line: a hint is advice the layer above re-checks, an answer is
something only the engine knows. ADR 0015.

## Consequences
The executor filters what comes back on every path. `KeyRange::select` is that
line, and removing it is caught by the probed run rather than by review.

A new engine's obligations are three sentences long. Everything else it may do
for speed and skip when it cannot.

Both remaining hints are safe to half-implement, because both only ever widen
what comes back. That is what makes the rule "return a superset, never a subset"
the whole of the read contract.
