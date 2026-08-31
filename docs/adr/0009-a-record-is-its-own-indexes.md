# ADR 0009: A Record Is Its Own Indexes

## Status
Accepted

## Context
`doclite-core` holds objects as bytes and index rows as byte tuples, and it is
right not to know more. That leaves every caller writing the same four things by
hand for every type: mint an id, assert each unique key, file the object, file
a row per index, and keep all of it in step when a field changes.

## Decision
`#[derive(Document)]` writes them. The struct's attributes specify the indexes a
caller can query: `#[unique]` and `#[index]` on a field, `#[index(a, b, c)]` on
the struct. The macro emits the index constants, an `impl Record`, and a query
builder with one method per prefix of each index. It also emits one index nobody
asked for, which ADR 0010 is about.

One struct attribute says nothing about indexes. `#[immutable]` claims the
record has exactly one revision, ever, and the macro answers by leaving out the
`Amendable` impl that `Connection::update` asks for. ADR 0012 is why that is a
bound rather than a refusal an engine could report.

**Every non-unique index carries the document id as a trailing column.** This is
forced rather than chosen. `delete_index` retires a whole range, so a one-column
index on `age` would make retiring one record's row impossible without taking
every other record of the same age with it. The document rather than the
revision, so the key of a row does not move when the record is superseded. A
unique index stays narrow because `require_absent` on the key alone is what
asserts uniqueness, and it holds at most one row anyway.

A `by_` method takes a value or a Rust range through `IntoBounds`, so `by_age(18)`
is an equality and `by_age(18..)` is a comparison. The column type is a parameter
on the trait rather than only on `Self`, which is what keeps the scalar impl from
overlapping the range impls: unifying them would need `C = Range<C>`. `>` has no
range literal and is `greater_than`, which carries the column type so a value of
the wrong type cannot encode to bytes that quietly match nothing.

**A record walks its index rows; it does not build operations.** `Record` has
three items: its `schemas`, its `latest` index, and `rows`, which visits every
index row a value belongs in exactly once. `Rows` is what a walk hands each one
to, and it is generic over the key's width rather than taking a slice, because a
`Key<N>` is an array and an operation will not file a row whose arity it cannot
see at compile time. That is the same reason the emission has to live in
generated code; it is not a reason for the record to decide what the emission
means.

What a row becomes is `crate::write`'s. Retiring rows and filing them are the
same walk with a different `Rows`, so they cannot disagree about a key, and the
generated index is a row like any other rather than a step someone has to
remember to add. The unique keys are claimed by a third walk, which numbers them
and keeps the names it numbered, so the abort code that comes back and the index
it names are one sequence rather than two lists that have to agree.

`Revision<T>` is the envelope: the value, when it was written, which record it
belongs to, and the revision it superseded. `Connection::update` supersedes,
which ADR 0001 already describes as a delete of the old index rows plus an
insert of the new ones. `require_current` asserts this revision is still the
live one, so a concurrent supersede is `DocumentError::Stale` rather than a lost
write.

**The order of those steps is forced, not preferred, and it lives in one
function.** `require_current` has to come before the rows are retired, or
read-your-own-writes shows it the delete of the very row it is counting and
every update is stale. The unique check has to come after them, or a record
editing an unrelated field finds its own key still in place and calls itself a
duplicate. That is ADR 0005 doing its work twice in opposite directions, and
between the two there is nothing left to choose.

There is no separate type for a value read for editing. `Stored<T>` is what a
query returns and what `Connection::update` takes, and reaching for `&mut`
through its `DerefMut` copies the value aside first, because the rows to retire
are keyed on what the value was and nothing else remembers once an edit has
happened. Reading a row therefore costs no copy and editing one costs exactly
one. That `DerefMut` and `update` both ask for `Amendable`, so a record the
schema froze is never handed a `&mut` at all.

Ids are snowflakes minted here, not in `doclite-core`. ADR 0007 keeps identity
out of the storage layer so an operation stays fully described before it runs;
this is the layer that knows what a revision is, so this is where an id comes
from. `NodeId::derived` needs no configuration and `DOCLITE_NODE_ID` pins one.

## Consequences
`doclite-core` gains nothing. It still holds bytes, still knows no rule, and its
suite passes without this crate existing.

An abort becomes an error. `Outcome::Aborted` is an answer at the storage layer
because the operation did its job; here the caller asked for the insert, so a
taken email is `DocumentError::Duplicate` and `conn.insert(user).await?` reads
the way it should. `Connection` is the one place that inversion happens.

A `Clock` is read once while an operation is built and never during the walk.
`Db::run` re-walks on conflict, so a clock read inside the walk would stamp a
different time on every attempt. The id and `created_at` come from one read.

Ten bits of node means two derived writers collide about once in a thousand. A
collision is a duplicate id, which ADR 0001 turns into
`StorageError::DuplicateObject` rather than an overwrite, so it fails loudly
and the environment variable is the fix.

A record needs at least one index of its own or the macro refuses it. The
generated one would find it by identity, but a caller who has to already know
the id has not found anything.
