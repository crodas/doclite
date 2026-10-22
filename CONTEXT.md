# Domain language

Words this project uses in a specific sense. `docs/spec.md` is the design and
`docs/adr/` is why it is shaped that way; this file is only the vocabulary, and
a term is added here when a conversation sharpens it rather than all at once.

## What is stored

**Object.** Bytes under an `ObjectId`, and the whole of what `doclite-core`
knows. An index row points at one. Nothing down there knows why two objects
might be related.

**Revision.** One object holding one version of a record. A `RevisionId` is
minted per edit, and it is what `prev` names and what an index row points at.
`Revision<T>` is the type: the envelope those bytes decode into. Its own
`RevisionId` is the key it sits under rather than a field of it.

**Document.** The record itself, across every revision of it. A `DocumentId` is
the `ObjectId` of the first revision and never moves, so it is what another
record holds as a foreign key. `Stored::id` is this one; `Stored::revision` is
the other. ADR 0010 is why identity is written into the envelope rather than
worked out. There is no `Document` type: a record is only ever held as a
`Stored<T>`, one revision of it at a time, and named by a `DocumentId`.
`#[derive(Document)]` is the macro, and nothing else claims that name.

**Amendable.** Whether a record may be superseded. It is a fact about the
schema rather than about anything stored, so it is settled by the type: a struct
marked `#[immutable]` does not get the `Amendable` impl `Connection::update`
asks for, and an edit of one is a compile error. The default is amendable,
because a record that is never edited is the rare one and marking the exception
is cheaper than marking every other struct. Nothing below this layer is told,
which is ADR 0012.

**Encoding.** What a record's bytes are, chosen by `#[storage(..)]` and CBOR
unless a record says `json`. Like `Amendable` it is settled by the type rather
than by anything stored, and nothing below the document layer is told: an engine
sees bytes and asks only whether they are text. What JSON buys is a store
something else can read, which under `doclite-sqlite` means the revision lands
in `doclite_json_document` as a text value rather than a blob. The envelope is
JSON too, because half a JSON object is not one, and the record nests under
`body` so no field it declares can collide with the envelope's own.

**Reference.** What one record holds to name another: a `DocumentId` with the
type it points at carried alongside it, so a field says what it references and
following one needs no turbofish. On disk it is the id and nothing else, because
what it points at is a fact about the schema rather than about the row.

**Following.** Reading the record a reference names. It is a read rather than a
lookup in something already held, and it answers with whichever revision is live
at that moment, which is the whole reason the id does not move when a record is
edited. `Connection::load_many` follows a set of them in one scan of the
generated index, because they are all rows in one ordered index.

## Doing a unit of work

**Operation.** Preconditions and mutations in one ordered list, built up front
and handed over whole. It is a value, so an engine can look at it before running
it and run it again after losing a race (ADR 0003), and a caller can assert what
one contains without an engine to run it against.

**Write.** What filing a record or superseding one is, as an `Operation`, before
anything runs it. The order of an update's steps is forced rather than
preferred, so it lives in `crate::write` and nowhere else: assert the revision
is still the head, retire the old rows, claim the unique keys, file the
revision, file the new rows.

**Transaction.** Several records staged and filed as one `Operation`. It is the
same `Write` per record that `insert` and `update` build, joined with
`Operation::then`, so it adds a unit of work rather than a kind of write. What
the joining costs is the numbering: a write's abort codes say why it refused,
and two writes numbered from the same place would both answer to one code, so
each staged write is given the codes after the last one's and the run it owns
is what names the record that refused. There is no `begin` and no `rollback`
because there is nothing open: a dropped transaction wrote nothing.

**Sweep.** A scan and a template instantiated once per row, handed to an engine
unexpanded so it can write the whole thing in its own language. Offered only
when the `for_each` is the entire operation: expanding a template stages into
the overlay a later precondition reads, so deferring one with a step after it
would change what that step sees. With nothing after it there is nothing to
change, and the scan and its writes are one transaction, so the assertion the
executor would have recorded has nothing left to protect.

**Walk.** How a record tells the layer above what index rows it belongs in. It
yields each row once, and what to do with one is the caller's; retiring and
filing are the same walk with a different `Rows`.

## Filling an index

**Building.** An index that has been declared but does not yet hold every row
it should. It is a fact an engine reports rather than one a caller declares: a
store is the only place a previous run's shape was written down, so it is the
only thing that can notice a declaration it has no rows for. `Defined` is what
`Engine::define` answers and `Engine::rebuilt` is how one stops being building.

**Rebuild.** Filling one, by paging `<record>_latest` and walking
`Record::rows` for each live revision it names. It lives above `doclite-core`
because an object is bytes there, which is the whole reason this is not an
engine's job. `Records` is what a connection is opened over so there is a type
to walk at all, and `records![User, Note]` builds one. ADR 0015, and ADR 0001
for why an index is rebuildable in the first place.

**Gate.** What holds a read back while the index it names is still building.
Work is parked if and only if it *reads* such an index: filing a row into one is
safe, because the row belongs there either way, and so is retiring a range. A
precondition is a read, and that is the half that matters, because
`require_absent` against an index with rows missing would let two documents
claim one unique key. `Operation::reads` draws the line.

**Spawner.** Where a rebuild runs when the caller would rather not wait for it.
Supplied by the caller, because ADR 0006 keeps the runtime theirs. Not
supplying one is the ordinary case: `Connection::open` waits, which costs one
scan and needs no gate. A spawner that drops the work makes the reads parked on
that index answer `DocumentError::Rebuilding` rather than wait for something
that is not coming.

## Being an engine

There are two ways, and a backend picks the one that matches what it has. ADR
0011 is why there are two rather than one.

**Engine.** The full seam: define, read, apply a whole batch. Everything about
what a batch means is the implementer's, which is what lets a store with a query
language of its own answer in a statement. SQLite and Postgres go here. It also
answers what it had to build, which is not a hint in ADR 0004's sense: an
engine cannot ignore its own answer.

**Store.** The narrow seam, for a substrate that keeps ordered rows and nothing
more. Define an index, record one as filled, answer reads, and hand out a
**Region**. `StoreEngine`
supplies the rest and is what turns one into an `Engine`. Synchronous, because
an in-process map and the embedded key-value stores are; the async stays at the
engine seam where ADR 0006 put it.

**Region.** The store held still, long enough to judge one batch and write it.
Its reads are fallible and its writes are not, and that split is the whole of
atomicity here: everything that can refuse the batch is asked before anything
moves, so there is never a half-written region to unwind.

**Plan.** What a read asked for, before the executor takes it apart into flat
scans. An engine may be offered one and may decline it. `MemoryEngine` declines
every one, because counting rows it already holds in a map is what the executor
would have done anyway; the SQLite engine answers, which is what lets a backend
with a planner count where the rows are rather than carrying them here first. It
is a hint in ADR 0004's sense, so `Probe` strips it and `hints_are_hints` proves
a planning engine and a declining one agree. ADR 0013 and
`docs/plan-pushdown.md`.

**Column type.** What a column holds, carried on an `IndexSchema` so a store
with a type system of its own can declare a real column rather than a blob. It
settles nothing about order, which `ToOrdBytes` already did (ADR 0002); a type
nobody has described is `ColumnType::Bytes` rather than a refusal. A `Store`
ignores these entirely.

## Verifying an engine

`doclite-testkit` is the definition of a correct engine. It has two halves, and
they are not the same question.

**Rule.** One thing the suite checks, whichever half it belongs to. Every rule
has the same shape, a `Fresh` in and a `Failure` out, so which module it lives
in says what it checks rather than how it is run. `rules.rs` gathers them and
`conformance!` names them once, in one list that `registry_drift` holds to being
all of them.

**Case.** One rule an engine owes, checked against the `Engine` trait directly:
rows come back in index order, a batch applies whole or not at all, an assertion
sees pre-batch state, an assertion made under a limit is judged on what its
reader looked at. A case builds a fresh engine, so nothing carries over from the
case before. Lives in `crates/doclite-testkit/src/cases.rs`, one function each,
and becomes one test.

**Property.** A rule about the executor rather than the engine, checked by
running the same work two ways and comparing. `hints_are_hints` is the only one
so far. It builds more than one engine, which is why every rule takes the
constructor. Lives in `crates/doclite-testkit/src/properties.rs`.

**Probe.** An `Engine` adapter that strips every hint before forwarding: the
range becomes everything and the limit goes. What an engine wrapped in one still
answers correctly is what proves the hint was only ever saving work. See ADR
0004.

**Transcript.** The ordered record of what a script produced, one labelled line
per step, compared between two runs. Labelled so a divergence names the step it
happened at instead of printing both runs whole.

**Conformance.** The whole suite, as a macro an engine crate invokes:

```rust
doclite_testkit::conformance!(
    #[tokio::test]
    fresh
);
```

It expands to one test per case and property, so the test harness is the walk: a
failing engine reports every rule it breaks in one run, and a single rule can be
re-run by name during bring-up. The engine comes from a constructor the caller
names, and the test attribute is the caller's to pass, because which runtime an
engine uses is not this project's business (ADR 0006).

**Under.** Which engine a connection is under, and the same idea one layer up.
`Conformance` proves the engine seam on every engine and says nothing about what
sits on top of it, so a scenario that only ever ran in memory said nothing about
the durable engine: the generated `<record>_latest` index, a refused unique key,
a `Ref` followed through `load_many` and the codes a transaction numbers are all
the document layer's. A scenario is generic over this and `on_every_engine!`
expands it into one test per engine, which is why the tests read
`on_sqlite::a_stale_edit_loses_rather_than_clobbering` rather than naming an
engine in the body. It lives in `crates/doclite/tests/under/mod.rs` and takes
the test attribute for the same reason `conformance!` does: the contention
scenarios need a runtime with more than one thread and the rest do not.
