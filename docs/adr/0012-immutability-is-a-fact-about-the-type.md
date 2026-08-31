# ADR 0012: Immutability Is a Fact About the Type

## Status
Accepted

## Context
Some records should never be edited: a ledger entry, an audit row, an event. For
those an edit is a bug in the caller rather than a race, and there is no revision
to supersede because there was only ever going to be one.

Every existing refusal in this layer is a refusal about stored state.
`DocumentError::Stale` means some other writer got there first;
`DocumentError::Duplicate` means somebody already holds the key. Both are things
only the store knows, so both are asserted by the operation and judged by the
engine. Whether a record may be superseded at all is not like that. It is
written in the struct definition, it is the same for every value of the type,
and no engine could ever answer it differently.

## Decision
A struct-level `#[immutable]` says a record has exactly one revision, ever, and
the refusal is a trait bound rather than a value.

`Amendable` is an empty trait in `doclite::record`. `#[derive(Document)]` emits
`impl Amendable` for every record except one marked `#[immutable]`.
`Connection::update` requires it, so editing a record that has no impl does not
compile. There is no new `DocumentError` variant and no new `AbortCode`.

`Stored`'s `DerefMut` carries the same bound. That is what puts the error on the
line that wrote the mutation instead of on the `update` that could never have
filed it, and it means a value that cannot be superseded is never handed a `&mut`
in the first place.

The default is amendable. A record that is never edited is the rare one, so the
exception is what carries the mark, and a struct that says nothing keeps working
exactly as it did.

## Consequences
Nothing below this layer is told. `doclite-core` declares no rules on an index
(ADR 0001) and this does not become the first one: an immutable record builds
the same insert, walks the same rows, and claims its unique keys the same way.
`doclite-store`, `doclite-memory` and `doclite-testkit` are untouched.

The generated `<record>_latest` index stays as it is, keyed on the document and
the revision, even though the two are equal for every immutable record.
`Record::latest` answers with an `Index<2>` for every record, and `load` scans it
by document prefix, so narrowing it would buy nothing and cost the uniformity.

`Amendable` is deliberately not a subtrait of `Record`. What it gates is
`Stored`'s `DerefMut`, and a `Stored` holds any value the store can hold rather
than only a record.

The refusal cannot be asserted by an ordinary test, because there is nothing to
run. It is checked by a `compile_fail` doctest on `Amendable`, paired with an
ordinary one over the same struct without the attribute. The pair is the point:
a `compile_fail` block passes for any compile error at all, so the half that has
to succeed is what keeps the half that has to fail honest.

A caller writing code generic over `Record` that supersedes now needs
`T: Record + Amendable`. That is the bound being true rather than a new
restriction, and it is the only source change this asks of anyone.
