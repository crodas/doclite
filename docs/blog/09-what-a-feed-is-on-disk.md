# What a feed is on disk

The tour ended with the last post. This one is the appendix: [being told what
changed](07-being-told-what-changed.md) claimed that a feed is an index and
moved on, and this is what that sentence actually costs, from the key bytes up
to the two engines.

Sources are `crates/doclite-derive/src/spec.rs`, `crates/doclite/src/write.rs`,
`crates/doclite-sqlite/src/catalog.rs` and `crates/doclite-memory/src/lib.rs`,
with ADR 0001 and ADR 0002 underneath all of it.

## The key, and why that order

A feed declared by `#[owner] account: String` is three columns:

```text
(account, revision, document)
```

Each position is a decision.

**The owner leads** because every read of a feed pins it. A scan is one owner's
rows and nothing else, so the owner has to be the first component or the group a
reader wants would not be contiguous.

**The revision is second** because that is what the range is over. Ids here are
minted in time order, so ordering by the second column is ordering by when the
change happened, and "everything since this cursor" is a range over bytes rather
than a filter over rows.

**The document is last, and it is not there for uniqueness.** A revision belongs
to exactly one document, so `(owner, revision)` already names one row. It is
there so a scan can answer which records changed from key bytes alone, without
reading a single object, and because every other non-unique index in this store
carries the document as a trailing column. Keeping the shape means one fewer
special case in the derive, which decides all of this in one place:
`IndexSpec::arity`, `IndexSpec::column_types` and `IndexSpec::key_of`.

## Several owners are an OR

A field holding a `Vec` changes none of the above. The key still holds one
owner, and the derive emits a loop instead of a call:

```rust
for owner in &self.watchers {
    into.row(
        &Task::FEED,
        key![owner.clone(), revision, document],
        "task_feed",
        Filing::Feed,
    );
}
```

Each row carries one owner and a whole key, so a change is reachable from any
one of its owners. The test for it reads as the truth table: ana sees it, bruno
sees it, carla does not. What a reader applies is membership, not a comparison
against the list.

The contrast with a compound index is where the index machinery shows through. A
compound index is one row whose key is a tuple: a read supplies components from
the left, and every one it supplies has to match, which is a conjunction even
where a prefix is all it gives. A list of owners is several rows, each a
complete key, so a read supplies one of them and matches: a disjunction.
Identical machinery, opposite logic, and the only difference is whether the walk
emits one row or several.

Which means the disjunction is paid for at write time. Several owners cost
several rows when the record is filed, and still one range scan when it is read.
The alternative shape, one row holding the list, would have cost one row and
then a scan per owner, or a decode of every row in the index to test membership.

A reader of one owner never meets the disjunction at all. A reader merging two
owners' feeds does: a record owned by both filed a row under each, so the same
revision comes back from both scans, and the merge deduplicates on
`Stored::revision`. That is the deduplication at-least-once delivery already
asks for, so it costs nothing new.

What this shape cannot say is the AND. There is no row that means "this change
belongs to one clinic and one dentist", because every row holds one owner. That
is a compound owner, `#[owner(a, b)]`, which is refused today and named in the
roadmap: a key of two components, matched in full, which is the conjunction case
and a different index.

## Bytes that sort the way values do

A column is bytes by the time it reaches an engine, and the encoding is chosen
so that comparing the bytes is comparing the values (ADR 0002). That is the
whole reason a range scan can be a range scan in any store that keeps ordered
rows: nothing has to decode a key to compare two of them.

For the owner, that is `ToOrdBytes` for whatever the field holds. For the two
ids it is more deliberate than it looks. An `ObjectId` is a `u64` whose top bit
is reserved, so it always fits a signed 64-bit integer, and it encodes as one:
the value goes through `i64`, the sign bit is flipped, and the result is written
big-endian. Flipping the sign bit of a non-negative value preserves exactly the
order the unsigned encoding gave, so nothing moves, and the declared type stays
honest. The column says `ColumnType::I64` and the bytes are an `i64`, which
matters because an engine decodes a column according to what the schema said it
was. Declaring one thing and encoding another would read every id back off by a
sign bit.

So a feed row's key is the owner's bytes, then eight bytes of revision, then
eight of document, and a byte comparison of two of them answers which change
came first.

## One walk, four collectors

A record never builds an operation. It walks its rows once, handing each to
whoever asked, and what to do with one is decided above it. `Record::rows` is
that walk, and each row arrives with a `Filing` saying what the index is for.

There are four collectors and the feed is the reason the fourth exists:

| Collector | What it does with a feed row |
|---|---|
| `File` | Files it, like any other row |
| `Retire` | Skips it, which is the whole feature |
| `Claim` | Skips it, because an index carries no constraint |
| `Notify` | Collects it as a `Notice`, and ignores every other row |

`Retire` skipping one index is the only behavioural difference between a feed
and an ordinary index. And because `Notify` is the same walk with a different
collector, a notice and the row it stands for cannot disagree about a key: they
were produced by the same code reading the same value.

A rebuild is this walk too, narrowed by `Only::these` to the indexes that are
missing rows, which is why filling a feed cannot invent a key the writer would
not have used.

## What one edit is, as steps

An operation is a value, so an edit can be printed rather than described. For a
`Deal` with a unique reference, an owner and an indexed account, superseding one
revision with the next comes out as:

```text
require        the revision being replaced is still the live one
delete index   deal_latest
delete index   deal_by_reference
delete index   deal_by_account
require        the new reference is not taken
put object     the new revision
put index      deal_latest
put index      deal_by_reference
put index      deal_feed
put index      deal_by_account
```

Three rows retired and four filed. `deal_feed` appears in the second list and
not the first, and that asymmetry is the entire implementation of "a feed
accumulates".

The order of those steps is not a preference. The assertion has to come before
the deletes, or read-your-own-writes shows it the delete of the row it is
counting. The unique claim has to come after them, or the record finds its own
key still in place and reports itself as a duplicate. `crates/doclite/src/write.rs`
is where that order lives and the only place it is written down.

## What the engine is handed

A batch of those steps, and nothing else. `put_row` takes an index id and a
`Row`, which is a `RowKey` of already-encoded columns plus the object it names.
No engine learns the word owner, or that one of its tables is a feed, or that
rows in it are never deleted.

Under `doclite-sqlite` the table is built like every other index table:

```sql
CREATE TABLE IF NOT EXISTS "doclite_idx_deal_feed" (c0 TEXT NOT NULL,
  c1 INTEGER NOT NULL, c2 INTEGER NOT NULL, object INTEGER NOT NULL,
  PRIMARY KEY (c0, c1, c2, object)) WITHOUT ROWID
```

The columns are positional, because the engine never learned what any of them
means. The table is prefixed so a database may hold tables of its own without
them being mistaken for indexes.

Two more things to notice. There is no `UNIQUE` anywhere, because an index
carries no constraint: whether at most one object may hold a key is asserted by
an operation, which is what lets one index answer for two rules. And the primary
key is the whole row, columns and object together, since two rows may legally
share a key. `WITHOUT ROWID` makes the table its own index, so the rows are
stored in key order and a scan is a seek plus a walk.

Rows go in with `INSERT OR IGNORE`. That is where "a repeated owner files one
row" comes from: naming the same owner twice in a `Vec` produces the same key
and the same object, so the second insert is the same row and is ignored.

The memory engine reaches the same answer by a different route. Its indexes are
a `BTreeMap` from index id to a `BTreeSet<Row>`, and `Row` derives `Ord`, so
index order costs nothing and inserting a row that is already there changes
nothing. Two engines, two mechanisms, one behaviour, which is what the
conformance suite in `crates/doclite-testkit/` exists to hold them to.

## What a catch-up is as a read

`Feed::get_many` is one read plan. The range is a `RangeSpec::Between` whose
ends are two columns wide against a three-column index: the owner pinned and the
revision bounded. An absent high end falls back to the owner alone rather than
to the end of the index, which is the difference between "this owner's changes"
and "every owner sorting above this one".

The objects come with it. `with_objects` sets `ObjectMode::Strict`, so a row
naming an object the store does not hold is an abort rather than a hole, which is
the right answer when objects are append-only and nothing deletes one. That is
also why a change carries the record as it was at the time: the row names the
revision that filed it, and the revision is what gets decoded.

## What a notice is

Ids and columns, not bytes of record:

```rust
pub(crate) struct Notice {
    index: IndexId,
    key: Vec<Column>,
    document: DocumentId,
    revision: RevisionId,
}
```

A subscriber holds an index id and a prefix of columns, and the match is a
component-wise prefix comparison. Nothing in the `Bus` decodes a key, which is
the same split the store makes one layer down.

The record's bytes are deliberately absent. The write had them encoded and could
have passed them along, which would have saved the subscriber a point read of an
object that was written moments ago. It would also mean holding those bytes in
every subscriber's queue, and encoding them on every write in a process where
nobody is watching. A notice carries ids and the reader fetches.

## Filling one that is behind

A feed is derivable from the objects, like every index here, which is what makes
it something you can add to a database that already has records. The engine
reports it in a `Defined`, a reader of it waits at the gate, and the rebuild
pages the generated index of live revisions and re-walks `Record::rows` for each
one.

That is also the shape of its one real limitation: the generated index names
live revisions only, so a feed filled this way gets one row per document rather
than one per revision. The older revisions are still in the store, reachable by
walking `prev`, and nothing walks them. The first catch-up on a newly declared
feed therefore reads as a snapshot.

## What is deliberately not there

No constraint on the index, so no uniqueness to violate. No tombstone, because a
tombstone is erasure wearing an append's clothes. No delete, so nothing frees a
feed and its size grows with edits rather than with records. And no sequence
assigned by the store, which is what would have bought commit-order delivery and
would have meant an operation that cannot be fully described before it runs.

Each of those is an existing decision, not a new one. The feed inherits them,
which is the strongest thing to be said for building it as an index: almost
nothing about it is its own.

For why it is shaped this way rather than how it works,
`docs/adr/0016-a-feed-is-an-index.md`.
