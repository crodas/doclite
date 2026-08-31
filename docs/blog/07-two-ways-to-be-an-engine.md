# Two ways to be an engine

The last post swapped one backend for another and nothing above it moved. This
one is about writing the third: what a backend owes, which of the two seams to
implement, and how you find out you got it wrong before your users do.

Sources are `crates/doclite-store/`, `crates/doclite-testkit/`, and ADRs 0011
and 0013.

## Why there are two

The obvious design has one trait. Everything a backend must do goes on it, and a
new backend fills in the blanks.

That works until you notice the backends are not the same shape. An in-process
map and an embedded key-value store keep ordered rows and nothing more: they
have no query language, no planner, and no opinion about what a batch means.
SQLite and Postgres have all three, and telling them how to apply a batch is
throwing away the thing you picked them for.

So there are two seams and a backend picks the one that matches what it has.

**`Engine`** is the wide one. You are handed a whole `Batch` and admit it
however you like. Everything about what a batch means is yours, which is what
lets a store with a query language answer in a statement. SQLite and Postgres
go here.

**`Store`** is the narrow one. Define an index, answer reads, and hand out a
`Region`. `doclite-store` supplies the rest, and `StoreEngine` is what turns one
into an `Engine`.

## The narrow seam

```rust
pub trait Store: Send + Sync {
    type Region<'a>: Region where Self: 'a;

    fn define(&self, indexes: &[IndexSchema]) -> Result<(), StorageError>;
    fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError>;
    fn exclusive(&self) -> Result<Self::Region<'_>, StorageError>;
}
```

Three methods, all synchronous. That is deliberate: the substrates this is for,
an in-process map, RocksDB, sled, redb, are synchronous, and ADR 0006 put the
async at the engine seam where it belongs. A store that waits on a network
should implement `Engine` directly.

`MemoryEngine` is a type alias for `StoreEngine<MemoryStore>`, and `MemoryStore`
is two maps behind an `RwLock`. What a batch means is not in that file at all.

## Region is where atomicity lives

```rust
pub trait Region {
    fn arity(&self, index: IndexId) -> Result<usize, StorageError>;
    fn rows(&self, index: IndexId, range: &KeyRange) -> Result<Vec<Row>, StorageError>;
    fn has_object(&self, id: ObjectId) -> Result<bool, StorageError>;

    fn delete(&mut self, index: IndexId, range: &KeyRange);
    fn put_object(&mut self, id: ObjectId, bytes: Vec<u8>);
    fn put_row(&mut self, index: IndexId, row: Row);
}
```

Look at the return types. The three reads are fallible and the three writes are
not, and that split is the whole of atomicity here.

Everything that could refuse the batch is asked while nothing has moved. Every
assertion is judged, every object that is already there is caught, every index
nobody declared is named. Only once all of that has passed does anything get
written, and by then nothing can fail. There is never a half-written region to
unwind, so there is no unwinding to implement and no rollback to get wrong.

`Admission` is that protocol, and it is one type rather than two loose functions
precisely so the order cannot be got wrong by a caller:

```rust
let mut region = self.0.exclusive()?;
let mut admission = Admission::over(&mut region, batch)?;
if !admission.admits()? {
    return Ok(Applied::Conflict);
}
admission.commit();
Ok(Applied::Committed)
```

`admits` returning `Ok(false)` is a conflict: an assertion stopped holding and
the executor will walk the operation again. `Err` is the store refusing on its
own terms, which no amount of re-walking fixes. Those are different answers and
conflating them turns a retryable race into a hard failure.

`commit` writes deletes, then objects, then index rows, which is the order a
batch promises. It returns nothing because nothing it does can fail.

## Why SQLite cannot use the narrow seam

This is the part worth reading even if you never write a backend, because it is
a case of a good abstraction having a real edge.

`Region` has no commit point and its writes cannot fail. A SQLite `COMMIT` can
fail: the disk fills, the WAL cannot be checkpointed, the file is gone. In the
narrow seam the only place that commit could happen is as the region drops,
after `Ok(Applied::Committed)` has already been returned, where the error cannot
be reported and success has already been claimed.

The narrow seam cannot host a durable engine without lying about durability. So
SQLite implements `Engine` and writes the batch protocol again in SQL. The cost
is real and stated: `crates/doclite-store/src/admit.rs` is that protocol in
sixty readable lines, and it is private, taking `impl Region`. Read it,
reproduce its order, do not try to reuse it.

There is a related failure the code now refuses outright. A store never takes a
sweep, so being handed one means an engine claimed a capability it does not
have. `Admission::over` matches on the batch and errors. Before that it was a
field neither `admits` nor `commit` read, so a store that claimed the capability
wrote nothing and reported success, which is the one failure mode in that crate
that looked like success.

## Finding out you are wrong

An engine trait is a list of promises, and a list of promises in a doc comment
is a list of things a backend can quietly not do. `doclite-testkit` is the
alternative: the definition of a correct engine, as a suite you run.

```rust
struct Mine;

#[async_trait]
impl Fresh for Mine {
    type Engine = MyEngine;
    async fn fresh() -> Result<MyEngine, StorageError> { /* .. */ }
}

doclite_testkit::conformance!(
    #[tokio::test]
    Mine
);
```

You say how to build one and get a test per rule. The macro expands to one test
each rather than one big test, so a backend under construction reports every
rule it breaks in a single run, and a single rule can be re-run by name during
bring-up. The test attribute is yours to pass, because which runtime your engine
uses is not this project's business.

The suite has two halves and they ask different questions. A **case** checks
what an engine owes, against the `Engine` trait directly: rows come back in
index order, a batch applies whole or not at all, an assertion sees pre-batch
state. A **property** checks the executor rather than the engine, by running the
same work two ways and comparing.

`hints_are_hints` is the property that makes the whole hint story trustworthy.
It wraps your engine in a `Probe`, which blanks every hint before forwarding:
the range becomes everything, the limit goes, `plan` declines, `takes_sweep`
says no. Then it runs the same script bare and probed and demands identical
transcripts. An engine that quietly depended on being told the range fails there
rather than in production, and an engine that answers plans is held to giving
the same answers as one that does not.

The transcripts are labelled per step, so a divergence names the step it
happened at instead of printing both runs whole and leaving you to diff them.

## Where the line is

An engine holds bytes. It knows nothing about revisions, documents, references
or the derive, and it should stay that way: everything in posts two through five
is built on top of a seam that has never heard of any of it. What an engine owes
is the three required methods, the ordering guarantees, and atomicity. What it
may do is everything in the hints table, for speed, and skip when it cannot.

If you write one, the suite is the specification and this post is the
orientation. Where the two disagree, the suite is right.

---

That is the tour. [The storage layer](01-how-doclite-stores-and-finds-things.md)
holds bytes and operations that are values; [the
derive](02-the-struct-is-the-index.md) turns a struct into its own indexes;
[nothing is locked](03-nothing-is-locked.md) is what happens when two writers
meet; [transactions](04-several-records-at-once.md) make several records one
unit of work; [the type](05-what-the-type-settles.md) settles what the store
should not have to; and [where it is kept](06-where-it-is-kept.md) is the choice
of backend this post explains how to extend.
