# Several records at once

An insert is one operation and an edit is one operation. That is the right unit
for one record and the wrong one for a change that is only correct as a whole. A
deal and the tally on the account it belongs to are the standard example: a
tally that counts a deal nobody filed is wrong, and so is a deal nobody counted.

Samples here are lifted from `crates/doclite/tests/transaction.rs`, which passes
against every engine.

## There is still no begin

```rust
let mut tx = conn.transaction();
let filed = tx.insert(note("first", Shelf::Front))?;
tx.insert(Mark { tag: Slug("read".into()), note: filed.reference() })?;
tx.commit().await?;
```

Four lines and none of them opens anything. `conn.transaction()` hands back a
value that accumulates steps, and `commit` is the first thing that talks to the
store. Between those two lines nothing is held: no lock, no connection, no
snapshot, no row marked as being looked at.

That is not a convenience. [The first
post](01-how-doclite-stores-and-finds-things.md) built an operation as a value
so the engine could inspect it before running it and run it again after losing a
race. A transaction is the same value with more in it. `Transaction::insert` and
`Transaction::update` build exactly the operations `Connection::insert` and
`Connection::update` build, and join them with `Operation::then`. There is no
second code path for "the transactional version" of a write, because there is no
non-transactional version to distinguish it from.

The consequence a reader should sit with: a transaction that is dropped instead
of committed wrote nothing, and needed no rollback to achieve that. There was
never anything but a value on the stack.

```rust
{
    let mut tx = conn.transaction();
    tx.insert(note("abandoned", Shelf::Front))?;
}

assert!(notes(&conn, Shelf::Front).await.is_empty());
```

## The ids come back first

`tx.insert` returns a `Stored<T>` before anything is committed, which looks
backwards until you remember where ids come from. They are minted by the caller,
not by the store (ADR 0007), so the id exists as soon as the record is staged.

This is what makes the two-record case writable at all. The `Mark` above holds
`filed.reference()`, a `Ref<Note>` pointing at a note that is not in the store
yet and will land in the same batch. A store that minted ids would have to be
asked, which means a round trip, which means the first record is written before
the second one is described, which is the thing being avoided.

The price is stated rather than hidden: a refused commit means the ids you were
handed belong to nothing, and the caller discards them. Nothing was written
under them.

## A refusal names the record

Each record's write carries preconditions, and a failed precondition aborts with
a code that says which rule refused. Two records staged into one operation
therefore have a problem: if both writes number their codes from the same place,
both answer to the same number, and an abort can say a unique key was taken but
not whose.

So each staged write is given the codes after the last one's. `Codes::at` walks
a cursor forward as records are staged, and the run a write owns is what
resolves an abort back to the record that raised it.

```rust
conn.insert(Mark { tag: Slug("read".into()), note: anchor.reference() }).await?;

let mut tx = conn.transaction();
tx.insert(note("fresh", Shelf::Front))?;                       // fine
tx.insert(Mark { tag: Slug("read".into()), note: /* .. */ })?;  // tag is taken

assert_eq!(
    tx.commit().await,
    Err(DocumentError::Duplicate { index: "mark_by_tag" }),
);
```

`mark_by_tag`, not `note_by_slug`. The second record's index, named
specifically, even though the first record also has a unique index and also
staged a claim on it. Without the numbering both would be code 1000 and the
error could only say that something was taken.

## Whole or not at all

The property the type exists for:

```rust
conn.insert(note("taken", Shelf::Back)).await?;   // claims the slug

let mut tx = conn.transaction();
tx.insert(note("fresh", Shelf::Front))?;
tx.insert(note("taken", Shelf::Front))?;          // will refuse

assert_eq!(
    tx.commit().await,
    Err(DocumentError::Duplicate { index: "note_by_slug" }),
);
assert!(notes(&conn, Shelf::Front).await.is_empty());
```

The second assertion is the one that matters. `fresh` was staged first and had
nothing wrong with it, and it is not in the store. One operation, one batch, one
verdict.

Nothing about this is special-cased. The executor walks the joined step list
against an overlay, and the first precondition that fails ends the walk before
anything is handed to the engine. A refusal is not an unwinding; it is a batch
that was never sent.

## Staleness travels too

An edit staged in a transaction carries the same assertion a lone edit carries:
that the revision being superseded is still the head. If somebody else moved it
in between, the whole commit refuses.

```rust
let mut ahead = filed.clone();
ahead.shelf = Shelf::Front;
conn.update(ahead).await?;          // somebody else got there first

let mut stale = filed;              // the handle we were holding
stale.body = "late".into();

let mut tx = conn.transaction();
tx.insert(note("alongside", Shelf::Front))?;
tx.update(stale)?;

assert_eq!(tx.commit().await, Err(DocumentError::Stale));
```

`alongside` is not written. [Nothing is locked](03-nothing-is-locked.md) covers
what `Stale` means and what to do about it; the only thing the transaction adds
is that the answer is now all-or-nothing across every record in it.

## An empty commit is not an operation

```rust
let tx = conn.transaction();
assert!(tx.is_empty());
assert_eq!(tx.commit().await, Ok(()));
```

Committing nothing does nothing and succeeds. There is no empty batch handed to
the engine to be politely ignored, because an operation with no steps is not
worth a round trip.

## The shape of it

A transaction here adds a unit of work, not a kind of write. The same builder
runs, the same preconditions are asserted, the same executor walks the same list
against the same overlay, and the same engine sees one batch. What the layer
contributes is the numbering that lets a refusal name a record, and the
guarantee that a value you never handed over changed nothing.

Next: [what the type settles](05-what-the-type-settles.md), where some of these
rules stop being checked at runtime at all.
