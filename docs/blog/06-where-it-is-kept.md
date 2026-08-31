# Where it is kept

Every sample so far has run on `MemoryEngine`, which keeps two maps behind one
lock and forgets everything when the process exits. That is the right default
for a tour and the wrong one for a clinic's patient records.

This post is the swap, and what it costs. Samples come from
`crates/doclite/tests/under/mod.rs` and
`crates/doclite-sqlite/tests/durable.rs`, which pass; `docs/sqlite.md` is the
design in full.

## The swap

```rust
use doclite_sqlite::SqliteEngine;

let engine = SqliteEngine::open("clinic.db")?;
let conn = Connection::open(engine, &User::schemas()).await?;
```

That is the change. The file is created if it is not there, and opening it again
finds what was filed. Every record, query, edit, reference and transaction in
the previous five posts reads exactly the same.

The interesting question is whether to believe that last sentence, because it is
the kind of claim documentation makes and code quietly stops honouring. Here it
is checked. Every scenario in `crates/doclite/tests` is generic over which
engine it runs under, and `on_every_engine!` expands each one into a test per
engine, so the suite reports
`on_memory::a_stale_edit_loses_rather_than_clobbering` and
`on_sqlite::a_stale_edit_loses_rather_than_clobbering` side by side. A behaviour
that differed between the two would fail on one of them.

That is a different guarantee from the one `doclite-testkit` gives. The testkit
proves the *engine seam*: rows in index order, batches applied whole, assertions
judged against pre-batch state. It says nothing about the layer above, which is
where `<record>_latest`, unique refusals, `Ref` joins and abort-code numbering
live. Two suites, two questions.

## Three constructors

```rust
SqliteEngine::in_memory()?                                  // no file at all
SqliteEngine::open("clinic.db")?                            // durable against power loss
SqliteEngine::open_with("clinic.db", Durability::ProcessCrash)?
```

The first is for tests that want the real engine and no cleanup. The other two
differ in one pragma and in what they promise.

## Durability is named for what survives

```rust
pub enum Durability {
    PowerLoss,
    ProcessCrash,
}
```

Not `FULL` and `NORMAL`, which is what SQLite calls them, because the pragma
name answers a question about SQLite and the caller is asking a question about
their data.

`PowerLoss` waits for the disk before calling a commit done. `ProcessCrash` does
not, so a crash of the process loses nothing, and the machine losing power may
cost the most recent commits. That waiting is the entire difference, and it is
also most of the write latency, which is why the choice exists at all.

`open` means `PowerLoss`. The safe answer is the one you get by not choosing,
because an engine whose purpose is durability should not quietly trade it away
to look faster in a benchmark. `open_with` is where the other one lives, and it
takes a word from the caller that says what they are willing to lose.

An in-memory database reaches no disk, so the question does not arise there and
the pragma does nothing.

Two settings are not the caller's. `journal_mode = WAL`, so a reader and the
writer stop blocking each other, and `busy_timeout`, so a contended write waits
instead of failing. Neither is a promise about what survives; they are about how
work is scheduled, and a caller who set them wrong would be trading correctness
for nothing.

## What the engine does with a thread

`rusqlite::Connection` is `Send` and not `Sync`. `Engine` takes `&self` and
returns a `Send` future. A connection therefore cannot simply sit in the engine
behind a shared reference.

The obvious fixes both pick a runtime for the caller. `spawn_blocking` chooses
tokio. An async SQLite driver chooses whatever it was built on. ADR 0006 already
said which runtime a caller uses is not this project's business, so neither is
available.

So the connection lives on one thread the engine owns. Each method sends a job
down a channel and awaits a reply on a oneshot that belongs to no runtime. The
thread is the only thing that ever touches the connection, `Send + Sync` is
satisfied by the channel rather than by a lock, and no guard is ever held across
an await.

State the cost rather than burying it: reads serialize behind writes. SQLite in
WAL mode would allow concurrent readers on separate connections, and a read pool
beside the single writer is the obvious later change. One thread is the version
whose correctness is obvious, which is the right first version.

## It answers plans

[The first post](01-how-doclite-stores-and-finds-things.md) listed `plan` among
the things an engine may decline, and said an engine that declines everything is
complete. This is the engine that does not decline.

A count is the clearest case. Handed a `Plan::Count`, `MemoryEngine` declines,
the executor asks for the rows and counts them, and that is nearly free because
they were already in a map. SQLite answers it as `SELECT COUNT(*)`, which counts
where the rows are instead of carrying every one of them across a channel to be
counted here.

The hint stays a hint. `Probe` strips it and the `hints_are_hints` property runs
the same script against a planning engine and a declining one and demands
identical transcripts. A backend is allowed to be clever and is not allowed to
be different.

## Typed columns

An index row's key is bytes, and the ordering is settled by `ToOrdBytes` before
any engine sees it. A store that has a type system of its own can still declare
a real column, because `IndexSchema` carries a `ColumnType`, and SQLite maps
each one to a real SQLite type where it fits.

This buys nothing for correctness, which the byte encoding already settled. It
buys a database you can open in `sqlite3` and read.
`crates/doclite-sqlite/tests/columns.rs` is the table-driven test that each
mapping round-trips and keeps its order, including the ones that do not fit a
signed 64-bit integer and stay blobs.

## What does not change

Nothing above the engine. That is the point of the seam and it is the thing
worth checking rather than asserting, so it is checked. What does change is that
the process exiting stops being the end of the data, and that a commit now costs
a disk.

Next: [two ways to be an engine](07-two-ways-to-be-an-engine.md), for anyone who
wants their store to be the third one.
