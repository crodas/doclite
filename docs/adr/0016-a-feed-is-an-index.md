# ADR 0016: A Feed Is an Index

## Status
Accepted

## Context
Nothing could ask the store what changed. Every index holds what is current and
an update moves its rows, so a caller that mirrors part of a store, pushes to a
client, or wakes a worker had to poll each index it cared about and diff the
answer against what it saw last time.

The revisions themselves were already there. An update supersedes rather than
overwrites (ADR 0001), so every version of every record is still an object and
`prev` chains them. What was missing was a way to find the ones that happened
after some point without reading a record you already have.

Two demands pull in different directions. A subscriber that is connected wants
to be told, in the moment, without polling. A subscriber that was away wants
what it missed, which no amount of being told can provide: a process that was
not running cannot have been notified. The second is the harder one and it is
the one a design can get wrong permanently, because a notification that was
never durable cannot be recovered afterwards.

## Decision
A record declares who its changes belong to, and that declaration is an index.

`#[owner]` on a field keys an index on `(owner, revision, document)`, and its
rows accumulate: `Retire` leaves them where they are, so one row is filed per
revision rather than one per record. Reading one owner's changes from a point
in time is then a range scan of one ordered index, and the live half is an
optimisation over doing exactly that.

**The durable half is the primitive.** Catching up is `Feed::get_many`, which
is the same `Connection::select` every other query runs. Being told is
`Feed::watch`, which is that scan with a tail. Had it been the other way round,
a subscriber that missed a notice would have had nothing to fall back to, and
every part of the delivery path would have had to be reliable.

**An owner is a column.** It is any `ToOrdBytes` value, usually a `Ref` to the
record it belongs to. Nothing below the document layer learns the word: in
`doclite-core` it is bytes in a key, which is the same split ADR 0004 and ADR
0008 draw everywhere else. A feed is declared, filled, scanned and rebuilt by
machinery that was already there.

**One owner or several, and the index is the same.** `#[owner]` on a field
holding a `Vec` keys the feed on one element of it, so a revision files one row
per owner and lands in each of their feeds. The key is not widened, because a
feed is read for one owner: widening it would make the common read a scan of a
group it does not want. The detection is syntactic, like the byte-field refusal
`#[storage(json)]` already makes, so a type alias for a list is read as one
owner and fails to compile as a column.

Two things follow from the list rather than from the index. An empty one files
nothing, which is the honest meaning of a record that currently has no owners.
A repeated one files one row, because a row is its key: the memory store inserts
into a set and `doclite-sqlite` writes `INSERT OR IGNORE` against a primary key
over the key columns and the object.

**Whether a record has a feed is settled by the type.** `Record::FEEDS` names
the index or is `None`, and it is a constant rather than a method so the check
in front of a publish is a constant too: for a record with no owner the walk is
compiled away rather than skipped, and a subscriber watching one record does
not make every write of every other type encode keys it discards. ADR 0012's
argument in a third place, and the reason it carries no default is the same one
that makes `#[immutable]` loud: `None` would read as "no feed" for a
hand-written record that has one, which is a store that quietly tells nobody
anything.

**A cursor is a revision, not a time.** A `RevisionId` is a snowflake, so it is
a timestamp with the ties broken (ADR 0007 mints it here, which is why this is
possible at all). Two revisions can share a millisecond, and a cursor that
cannot tell them apart either skips one or hands back the pair for ever. A
`Timestamp` still converts into a `Checkpoint`, landing at the first id that
millisecond could hold, so a caller that kept only the time replays a few rows
rather than missing any. `id::first_minted_at` is that conversion and
`id::minted_at` is its inverse.

**A notice is published after the commit and never during the walk.** An
operation is a value and a losing one is walked again (ADR 0003), so a notice
published from inside the walk would announce a change that did not happen.
`Connection::insert`, `Connection::update` and `Transaction::commit` are the
three places that see `Outcome::Committed`.

**A notice comes from the walk that files the row.** `Notify` is a `Rows`
implementation beside `Retire`, `File` and `Claim`, so what a subscriber is told
and what the feed holds cannot disagree about a key. It yields feed rows only:
every other row of a record is a fact about what is current rather than a change
that happened.

**Nothing in the bus interprets a key.** A notice carries the `Column`s the
record's own walk produced, and a subscriber matches a prefix of them. The
typed layer encodes the owner once, when it subscribes.

**The live channel is allowed to drop.** A subscriber's channel is bounded. A
full one is a reader that fell behind: it is told once and handed the rest out
of the feed, rather than being disconnected or having the writer wait for it.
This is the whole payoff of the durable half being primitive, and it is why a
slow subscriber cannot apply backpressure to a write.

**A watch waits for the feed to be filled.** A notice from a feed whose older
rows are still being rebuilt would let a reader advance its checkpoint past rows
it never saw, so `Feed::watch` clears the gate (ADR 0015) before it subscribes.

**Subscribing happens before the catch-up scan.** A write committing while the
scan runs would otherwise reach neither half. With the subscription first it
arrives as a notice whatever its revision, and the revisions the scan returned
are remembered so a change that is both scanned and announced is delivered once.
The deduplication is by exact revision id rather than by comparing cursors,
because a revision is minted when the operation is built and not when it
commits, so a legitimately new change can carry an id below the cursor.

**Delivery is at least once.** `Checkpoint::after` makes the ordinary resume
exact, and a timestamp cursor or a restart mid-page can repeat. A reader
deduplicates on `Stored::revision`, which is unique.

## Consequences
`Rows::row` takes a `Filing` rather than a `unique: bool`, which is breaking for
anyone implementing `Record` by hand. The derive writes it, so the ordinary
caller is unaffected, and one honest parameter is better than a second boolean
every walk would have to read correctly.

**A feed is the one index whose size grows with edits rather than with
records.** Nothing frees it, because nothing in this design deletes (ADR 0001).
Pruning is a range delete, which an operation can already express, but the key
order decides what is cheap: `(owner, revision, ...)` makes "prune this owner"
one range and "prune everything before this time" one range per owner.

**A reassignment is invisible as a departure.** A feed row is keyed on the owner
the record held when the row was filed, so moving a record from one owner to
another ends one feed and starts another. The old owner is not told it left, and
a subscriber mirroring that owner keeps a stale copy. Filing a row under the old
owner as well was considered and declined: it would mean a feed row whose owner
column disagrees with the revision it names, and a subscriber would have to
treat every row as possibly a departure.

**A feed added to an existing store is filled from the live revision of each
document.** A rebuild pages `<record>_latest`, which names live revisions only,
so the first catch-up reads as a snapshot rather than as a history even though
the older revisions are still in the store. Walking `prev` to fill the whole
history is possible and is not done.

**Order is mint order, not commit order.** An id is minted while the operation
is built, so a transaction that takes a while can commit after a change with a
higher id is already visible. A reader resuming from the highest revision it saw
can therefore miss one, and the fix is the reader's: keep a checkpoint lagged by
the longest build-to-commit time and let the at-least-once delivery absorb the
overlap. The live path has no such gap, because a notice is published when the
commit is known. Closing it properly would need a sequence the store assigns,
which ADR 0007 rules out.

**Delivery is in-process.** A second process writing the same database is not
seen, because the bus is a `Bus` on one `Connection`'s `Shared`. The answer is
the catch-up scan on a timer, which is the same code path; a notifier over
SQLite's own update hook would be an optimisation of that, not a different
design.

**A watcher borrows its connection.** `Changes` holds `&Connection`, so a watch
lives in the scope that opened the store rather than being handed to a task that
outlives it.

### Rejected: a notice carrying the record's bytes
The write has the encoded revision in hand, so a notice could carry it and a
subscriber would need no read at all. It would mean holding those bytes in every
subscriber's queue and encoding them where nothing is watching, and the read it
saves is one point lookup of an object that was just written. A notice carries
ids instead.

### Rejected: one feed index shared by every record type
A subscriber usually wants everything about an owner rather than everything
about one type of record. One index for all of them would answer that in one
scan, and it cannot say what type a row's object is: reading it back means
knowing which `Record` to decode it as. Per-type feeds keep that in the type
system, and a subscriber that wants several merges them, which is cheap because
each is already in revision order.

### Rejected: an unbounded channel per subscriber
It removes the lag path entirely and makes a reader that stops draining into
memory the writer pays for. The bounded channel plus the feed underneath has the
same effect without that failure mode: the rows are on disk either way.
