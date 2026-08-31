# ADR 0010: Identity Is Stored, Not Inferred

## Status
Accepted

## Context
A record is a chain of revisions and every revision is a separate object under
its own id. That leaves two questions with no answer written down anywhere:
which record a revision belongs to, and which revision of a record is the
current one.

Both used to be worked out from the index rows. The id a caller held was the id
of the revision it read, so a foreign key written against it named one revision
and went stale the moment that record was edited. Staleness was asserted by
rebuilding a key from the pre-edit value and counting rows in whichever index
happened to be declared first, which made concurrency depend on field order, and
which was simply wrong for a record whose indexes are all `#[unique]`: those
keys carry no id, so a concurrent supersede that left the key alone kept the
count at one and the losing write committed silently.

## Decision
Three words, and the split between them is the decision.

An **object** is bytes under an `ObjectId`. That is all `doclite-core` knows,
and the rename says so: `put_object`, `Batch::objects`, `Row::object`. An index
row points at an object.

A **revision** is one object holding one version of a record. `RevisionId` is
what `Snowflake` mints, what `prev` names, and what an index row points at.

A **document** is the record itself, across every revision it ever has.
`DocumentId` is the `ObjectId` of the first revision and never changes.
`Stored::id` returns it, so a foreign key names the record; `Stored::revision`
returns the other one.

The envelope carries the `DocumentId`. `FromStorage::from_storage` sees bytes
and nothing else, so a revision that did not carry it could not be decoded into
one that knows what it belongs to. That is envelope version 2.

`#[derive(Document)]` emits `<record>_latest`, keyed `(document, revision)`,
holding exactly one row per live record. An insert files a row, an update
retires the old one and files a new one, and `require_current` is a count of one
over the exact key. It answers the question uniformly for every record shape,
whatever indexes the struct declared, and it is an index of the live records in
its own right.

`(document, revision)` rather than `(document)` alone is forced. `Expect` is
`Absent` or `Count(n)` over a key range, and a range only ever inspects leading
key bytes, so asserting *which* revision is live means putting the revision in
the key rather than in the row's payload.

## Consequences
A foreign key survives an edit of what it points at. `Deal::account` is a
`DocumentId`, and updating the account files a new revision without moving the
document id, so the deals still find it.

Stale detection stops depending on how a struct is written. It no longer picks
an index by declaration order, no longer needs the pre-edit value, and no longer
has a shape it silently fails for.

The separate read-for-editing type and the query method that returned it are
gone. They existed only to carry the pre-edit value from the read to the write,
and `Stored<T>` now does that itself by copying on the first `&mut`. One type
comes back from a query, and it is the one `Connection::update` takes.

Every write costs two more steps: a row into `<record>_latest` on insert, and a
delete plus an insert on update. That is the price of the question having an
answer rather than a reconstruction.

Older bytes do not decode. The envelope version is checked and refused rather
than misread, which ADR 0008 already required; there is no migration because
there is nothing deployed to migrate.

`history` still walks `prev` one revision at a time. The generated index holds
only the live revision, so it cannot answer what came before, and giving it
every revision instead would make the live one no longer a single row.
