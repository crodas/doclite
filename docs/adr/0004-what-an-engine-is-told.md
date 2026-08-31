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

A third has been specified and is not built. An engine that can plan a read
should be handed the `ReadPlan` rather than the flat scans it is taken apart
into, so a count is counted where the rows are. ADR 0013 takes that decision and
`docs/plan-pushdown.md` is the design; the table above gains a row when the
method exists, and not before, because the rule this ADR states is that a hint
nothing sends is not one.

`doclite-testkit` is where this stops being a comment. `Probe` wraps any engine
and strips every hint before forwarding, and `hints_are_hints` runs the same
script bare and probed and demands identical transcripts.

## Consequences
The executor filters what comes back on every path. `KeyRange::select` is that
line, and removing it is caught by the probed run rather than by review.

A new engine's obligations are three sentences long. Everything else it may do
for speed and skip when it cannot.

Both remaining hints are safe to half-implement, because both only ever widen
what comes back. That is what makes the rule "return a superset, never a subset"
the whole of the read contract.
