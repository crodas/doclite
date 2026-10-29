# Being told what changed

Every index in this store holds what is current. An edit moves its rows: the old
ones are retired and the new ones filed, which is what lets a query answer with
the record as it is now. It is also why, until there was a feed, nothing could
ask the store what changed. A caller mirroring part of a store, pushing to a
client, or waking a worker had to poll every index it cared about and diff the
answer against what it saw last time.

Two different questions hide behind "notify me". A process that is connected
wants to be told, in the moment, without polling. A process that was away wants
what it missed, and no amount of telling answers that: something that was not
running cannot have been notified. The second question is the harder one, and
the one a design gets permanently wrong, because a notification that was never
durable cannot be recovered afterwards.

Samples here are lifted from `crates/doclite/tests/feed.rs` and
`crates/doclite/tests/watch.rs`, which pass against every engine, and from
`crates/doclite/examples/feed.rs`, which `cargo run -p doclite --example feed`
prints the output of.

## A feed is an index

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
struct Deal {
    #[unique]
    reference: Reference,

    #[owner]
    #[index]
    account: String,

    amount_cents: i64,
}
```

`#[owner]` says who a record's changes belong to. It declares one more index,
`deal_feed`, keyed on the owner, then the revision, then the document. Nothing
else about the record moves.

Two things about that key. The revision sits in the middle, and ids here are
minted in time order, so one owner's rows are already in the order their changes
happened. And the rows are never retired: `Retire`, the walk that takes the old
rows out when a record is superseded, skips this one index. Every other index
holds what is current, so an edit moves its row. A feed holds what happened, so
an edit adds one.

That is the whole feature. Everything below is what follows from one ordered
index whose rows accumulate.

A field can carry both attributes, and the pair is the usual one: `#[index]`
answers who the deal is about now, `#[owner]` what has happened to it.

## Catching up is a scan

```rust
let changes = Deal::feed(&conn, "roma".into()).get_many().await?;
```

One owner pinned, the revision ranged, oldest first. What comes back is a
`Stored<Deal>` per change, holding the record as it was at that change rather
than as it is now, because a feed row names the revision that filed it. The ids
are on each one: `revision` is the change, `id` is the record it belongs to, and
`created_at` is when it happened.

A long feed is read in pages, and the page tells you where the next one starts:

```rust
let page = Deal::feed(&conn, "roma".into()).limit(2).get_many().await?;

let last = page.last().expect("the page is not empty").revision();
let rest = Deal::feed(&conn, "roma".into())
    .since(Checkpoint::after(last))
    .get_many()
    .await?;
```

A reader that goes away keeps that last cursor and comes back with it. There is
nothing else to keep: no subscription to re-establish, no queue holding anything
for it, no server-side position. The position is a number the reader owns, and
what it points into is an ordinary index.

## A cursor is a revision, not a time

A `Checkpoint` is a `RevisionId`. That looks like an odd thing to hand a caller
until you notice a revision id here is a snowflake: milliseconds, then a node,
then a sequence. It is a timestamp with the ties broken.

The ties are why it is not a plain timestamp. Two revisions can land in one
millisecond, so a cursor that only knows the millisecond either skips the second
one or hands back both of them for ever. A `Timestamp` still converts in, and
rounds down to the first id that millisecond could hold, so a reader that kept
only the time replays a few rows rather than missing any. Replaying is the safe
direction; skipping is not.

Which makes delivery at-least-once, and `Stored::revision` is what to
deduplicate on. It is unique, so a reader that has seen one has seen it.

## Being told is the same read with a tail

```rust
let mut changes = Deal::feed(&conn, "roma".into()).watch().await?;

while let Some(change) = changes.next().await {
    match change? {
        Change::Revised(deal) => report(&deal),
        Change::Lagged { from } => println!("behind, reading again from {from:?}"),
    }
}
```

`watch` is `get_many` that does not end. The rows the feed already held come
first, in order, and then it waits for the ones that have not happened yet. A
reader that was away and a reader that is connected use the same call, which is
the point: there is no separate catch-up API to forget to run.

Three things about where a notice comes from, and each of them is a rule rather
than a detail.

It is published after the commit. An operation here is a value and a losing one
is walked again, so a notice published from inside the walk would announce a
change that never happened. `Connection::insert`, `Connection::update` and
`Transaction::commit` are the three places that see `Outcome::Committed`.

It comes from the same walk that files the row. `Notify` is a walk of
`Record::rows` beside `Retire` and the filing one, so what a subscriber is told
and what the feed holds cannot disagree about a key. They are the same walk with
a different collector.

Nothing in between interprets a key. A notice carries the columns the record's
own walk produced, and a subscriber matches a prefix of them. The owner is
encoded once, when the watch starts.

A watch also waits for its feed to hold every row it should. A feed declared
over a store that already has records is filled before it can be read, the way
any index here is, and a notice from a half-filled one would let a reader move
its checkpoint past rows it never saw.

## Falling behind is allowed

The channel behind a watcher is bounded. A reader that stops draining fills it,
and then this happens:

```rust
let mut changes = Deal::feed(&conn, "roma".into()).backlog(1).watch().await?;

// six changes are written while nothing is draining

let Some(Ok(Change::Lagged { from })) = changes.next().await else {
    panic!("a watcher given six changes and a backlog of one is behind");
};
```

It is told once, and then handed the changes it missed out of the feed, from its
own cursor. Not an error, not a closed subscription, and not a writer waiting
for it.

This is the consequence to sit with. The live channel is a cache of the index,
so dropping from it costs a scan rather than an event. A design where being told
was the primitive could not do this: every notice would have to be delivered,
which means an unbounded queue the writer pays for, or a slow reader applying
backpressure to a commit. Here the rows are on disk either way, so the cheap
thing is allowed to fail.

## One owner or several

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Task {
    #[unique]
    title: String,

    #[owner]
    watchers: Vec<String>,
}
```

A `Vec` is several owners, and the index does not change shape. One revision
files one row per owner, so the change is in any of their feeds and each of them
still reads it one owner at a time. A list is an OR: the reader that matches is
the one named in it, not the one that supplies the whole list.

```rust
conn.insert(task("t-1", &["ana", "bruno"])).await?;

assert_eq!(titles(&Task::feed(&conn, "ana".into()).get_many().await?), ["t-1"]);
assert_eq!(titles(&Task::feed(&conn, "bruno".into()).get_many().await?), ["t-1"]);
```

Widening the key to hold the list instead would have made the common read a scan
of a group nobody asked for. Two edges fall out of the list rather than the
index, and both are tested: an empty one files nothing, which is the honest
meaning of a record with no owners yet, and a repeated one files a single row,
because a row is its key.

One owner at a time is worth taking literally. A reader that wants two owners
reads two feeds and merges them, and a record owned by both appears in each, so
the merge deduplicates on `Stored::revision`. The same deduplication the
delivery guarantee already asks of it.

## A record with no owner cannot emit

Most records have no owner, and they should cost nothing. `Record::FEEDS` names
a record's feed or is `None`, and it is a constant rather than a method, so the
check in front of a publish is a constant too: for a record with no owner the
walk is compiled away rather than skipped at run time. A subscriber watching
deals does not make every note written anywhere in the store encode keys that
get thrown away.

Whether a record has a feed is a fact about the schema, which is the same
argument `#[immutable]` and `#[storage(json)]` already make in [what the type
settles](05-what-the-type-settles.md).

## What it does not promise

Each of these is argued at more length in
`docs/adr/0016-a-feed-is-an-index.md`.

**Order is mint order, not commit order.** A revision id is minted while the
operation is built, so a transaction that takes a while can commit after a
change with a higher id is already visible. A reader resuming from the highest
id it saw can miss that one. The fix is the reader's: keep a checkpoint lagged
by your longest build-to-commit time and let the at-least-once delivery absorb
the overlap. The live path has no such gap, because a notice is published when
the commit is known.

**A feed added to an existing store starts from the live revision of each
document.** Filling an index pages the generated index of live revisions, so the
first catch-up reads as a snapshot rather than a history, even though the older
revisions are still there.

**A reassignment is invisible as a departure.** A row is keyed on the owner the
record held when the row was filed, so moving a record from one owner to another
ends one feed and starts another. The old owner is not told it left.

**Delivery is in-process.** A second process writing the same database is not
seen. The answer is the catch-up scan on a timer, which is the same code path,
and that is exactly why the scan being the primitive matters.

And one more that is not a limit so much as a fact: nothing frees a feed. It is
the one index whose size grows with edits rather than with records. Pruning is a
range delete, which an operation can already express.

## The shape of it

One decision carries all of this: the durable scan is the primitive and being
told is the optimisation. That is what makes catch-up work at all, what lets a
bounded channel drop a notice, what makes a cross-process story a poller rather
than a redesign, and what kept the whole feature inside machinery that already
existed. No engine learned a word of it. `#[owner]` declares an index, the index
is rebuildable like any other, the read gate already protects it, and the only
new idea is that one walk leaves its rows alone.

Next: [two ways to be an engine](08-two-ways-to-be-an-engine.md), for anyone who
wants their store to be the third one.
