# ADR 0018: An Error Is Open and a Shape Is Closed

## Status
Accepted

## Context
No public enum carried `#[non_exhaustive]`. Before 1.0 that costs nothing,
because a minor bump is where a breaking change goes and the changelog says so.
After 1.0, adding a variant to any of them becomes a breaking change, so the
question had to be answered before the surface froze rather than discovered
afterwards.

The roadmap carried it as an open decision with both positions written down,
because both are real. Leaving the attribute off means a new variant is a major
version. Putting it on means every downstream `match` needs a catch-all arm,
which is exactly the pattern this project's own hygiene rules ban: an arm that
collapses an error away is how a failure goes unhandled.

What settled it was noticing that the question has two different answers
depending on what the enum is for, and that applying one answer to every enum is
what made it look undecidable.

## Decision
An enum that reports what went wrong carries `#[non_exhaustive]`. An enum that
describes a shape the caller has to handle completely does not.

`DocumentError`, `StorageError`, `BuildError`, `CodecError` and `IdError` carry
it. The set of things that can go wrong is open by nature: a new backend, a new
column type or a new precondition each brings a cause nobody has named yet, and
none of them changes what a caller is supposed to do. A caller handles the
variants it can act on and passes the rest on, which is what `?` is for, so the
catch-all arm the hygiene rules object to is not what this forces. What it
forbids is a caller claiming to have enumerated every way a store can fail,
which was never true.

Everything else stays closed: `Step`, `ReadPlan`, `Plan`, `ReadValue`,
`KeyRange`, `RangeSpec`, `Bound`, `Expect`, `Batch`, `Applied`, `Assertion`,
`Observed`, `ObjectMode`, `ObjectRef`, `Column`, `ColumnType`, `Outcome`,
`Bounds`, `Filing`, `Change` and `Durability`.

The clearest case is `ColumnType`. An engine maps every variant to a stored
representation, and one it does not recognise has no safe default: guessing
`Bytes` for a float would store it in an order that is not the float's. If a
numeric type is added later, every engine *should* stop compiling until it says
what the new type is held as. That is a major version doing its job, not a cost
to be engineered around.

`Step` and `ReadPlan` are the same argument from the other end: an engine that
silently ignores a step it does not know commits a transaction missing part of
itself, and ADR 0004 already makes `apply` the one place where nothing is a
hint. `Change` is the argument at the typed layer, where a subscriber that
treats an unrecognised change as nothing misses a change.

## Consequences
A caller matching an error exhaustively today needs one more arm. That is the
one source change this asks for, and it is the change that makes the match
honest.

The hygiene rule against collapsing an error still holds, and this does not
weaken it. `Err(_) => ()` is still banned; the arm this adds names the error and
propagates or logs it, which is what the rule asks for in the first place.
`#[non_exhaustive]` does not create the obligation to handle an unknown error
well, it just stops the compiler from pretending there will never be one.

A new error variant is now a minor change rather than a major one, which means
the next one can be added when it is understood rather than saved up for a major
release. Variants saved up for a major release are how an error enum ends up
describing the version it was designed in.

Nothing below the error types moves. The structural enums are where this design
puts its invariants, and they keep the property that matching one exhaustively
is both possible and required, which is what makes a backend's obligations
checkable by the compiler rather than by review.
