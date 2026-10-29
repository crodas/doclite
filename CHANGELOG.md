# Changelog

Notable changes to every crate in the workspace. They share one version and are
released together, so one file covers all seven.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
the versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0 a minor bump is where a breaking change goes.

## Unreleased

### Added

- **A record can say who its changes belong to.** `#[owner]` on a field
  declares a feed: an index keyed `(owner, revision, document)` whose rows
  accumulate, so one owner's changes read in the order they happened.
  `Deal::feed(&conn, account).since(checkpoint).get_many()` is the catch-up,
  and a `Checkpoint` is a revision rather than a time because two revisions can
  share a millisecond. A `Timestamp` converts into one at the start of its
  millisecond, so a reader that kept only the time replays rather than misses.
  ADR 0016.
- **`Feed::watch`, which is that read with a tail.** The rows the feed already
  held, in order, then the ones that have not happened yet. A notice is
  published once the write committed, carries the columns the record's own walk
  produced, and reaches a subscriber whose key is a prefix of them. The channel
  is bounded and allowed to drop: a reader that falls behind is told once and
  handed the rest out of the feed, so a slow subscriber cannot hold up a
  writer. A store nobody watches walks nothing.
- **A record can belong to several owners at once.** `#[owner]` on a field
  holding a `Vec` keys the feed on one element of it, so a revision files one
  row per owner and lands in each of their feeds. The key is not widened,
  because a feed is read for one owner either way. An empty list files nothing
  and a repeated owner files one row.
- **`id::first_minted_at`**, the inverse of `id::minted_at`, rounding down to
  the first id a millisecond could hold.
- **`HeldClock`, a clock a caller moves by hand.** `FixedClock` pins an instant,
  which is what makes a run reproducible and a deadline untestable. A clone
  shares the instant rather than copying it, so the clock a connection holds and
  the one the caller keeps are the same clock.

### Changed

- **`Rows::row` takes a `Filing` rather than a `unique: bool`.** Breaking for
  anyone implementing `Record` by hand, which the derive otherwise writes. What
  an index is for decides two things now rather than one: whether a unique key
  is claimed, and whether the row is retired when the record is superseded.
- **A `#[unique]` field holding an `Option` claims nothing when it is unset.**
  Behaviour change, and a fix: an unset field encoded one key for every record,
  so the second record to leave it empty was refused as a duplicate of the
  first. Any number may now leave it empty and a set one is claimed as before,
  which is what a null being distinct means in SQL. Read syntactically, so an
  `Option` behind a type alias still claims the unset key.
- **`usize` and `isize` are no longer index columns.** Breaking, and nothing in
  the workspace used them: they encoded at native width while declaring
  themselves bytes, so one key was a different number of bytes on a different
  target and a file written on one target would misorder on the other. `u64`
  and `i64` are the replacements, and naming one of the removed types is now a
  compile error rather than a store that works until it moves.
- **A `DOCLITE_NODE_ID` that is set but unreadable is refused.** Breaking:
  `NodeId::derived` answers a `Result`, and `IdError` has a variant for it,
  which is the first thing ADR 0018 buys. A typo in the variable used to fall
  back to the hashed node id, so a deployment that pinned a node to stop
  colliding kept colliding at the same odds with nothing said.
- **A generated query builder no longer requires `Copy` of a clock.** The
  builders take the connection by reference and never needed it. A caller whose
  clock is shared had to leak one to satisfy the bound.
- **The error enums are `#[non_exhaustive]`.** Breaking for a caller that
  matches one exhaustively, which now needs an arm for the variants a later
  release will add. `DocumentError`, `StorageError`, `BuildError`, `CodecError`
  and `IdError` report what went wrong, and that set is open; the structural
  enums stay closed so that an engine still has to handle every one of them.
  ADR 0018.
- **`Record` has a `FEEDS` constant.** Breaking for the same callers and
  without a default on purpose: `None` would read as "no feed" for a
  hand-written record that has one. It is what makes a record with no owner
  unable to emit rather than merely never emitting, and it keeps a subscriber
  watching one record from making every write of every other type walk its
  indexes.

## 0.2.0 - 2026-10-22

### Added

- **An index whose rows are missing is filled before it is read.** A new or
  reshaped index used to get an empty table, so a query through it answered
  correctly-shaped nothing. `Engine::define` now answers a `Defined` naming the
  indexes it has no complete set of rows for, `Engine::rebuilt` records that one
  is complete, and `doclite-sqlite` keeps the shape each index table was built
  for in `doclite_index`.
- **A connection is opened over record types.** `records![User, Note]` replaces
  concatenating `User::schemas()` and `Note::schemas()` by hand, and is what
  gives a rebuild a type to walk. `Connection::over` still takes schemas and
  refuses to open when something needs filling, because it has nothing that
  could fill it.
- **`Connection::spawning` and the `Spawner` trait.** A rebuild off the critical
  path, on a runtime the caller supplies. `Connection::open` waits for it
  instead, which needs no spawner and no gate.
- **Reads of an index that is still being filled wait for it.** Only reads, and
  only of that index: filing a row into one is safe, and a precondition is a
  read. `Operation::reads`, `ReadPlan::reads` and `Query::reads` are where that
  line is drawn.

### Changed

- **`Engine::define` and `Store::define` answer a `Defined` rather than
  nothing.** Breaking for anyone implementing either seam. A backend that keeps
  nothing across a restart answers `Defined::ready`.
- **`Connection::open` takes `Records` rather than `&[IndexSchema]`.** Breaking.
  `records![T]` is the replacement at a call site, and `Connection::over` is the
  schemas-only door.
- **`Store::rebuilt` is a required method.** Breaking for anyone implementing
  that seam. A store is the only place a previous run's shape was written down,
  so it is the only place that can record a fill. Defaulted, a store could
  report an index building on every open with nowhere to say one finished,
  which is a rebuild that never stops and never complains. `MemoryStore` keeps
  nothing across a process, so its answer is that there is nothing to write
  down.
- **An index whose shape moved is rebuilt rather than refused.** `docs/sqlite.md`
  said an arity change was a migration and not a definition. ADR 0015 reverses
  that, and ADR 0001 is why it is safe. An existing database is not rebuilt on
  first open: an index table with no recorded shape is taken as complete.

## 0.1.0 - 2026-09-06

First release, so there is no upgrade path to describe.

### Added

- **An object store that only appends.** Objects are bytes under an `ObjectId`
  and are never overwritten in place. Indexes are rows pointing at them and are
  disposable, because anything an index holds can be rebuilt from the objects.
- **Order-preserving byte keys.** A key encodes so that comparing the bytes is
  comparing the values, which is what lets a range scan be a range scan in any
  store that keeps ordered rows.
- **Operations as values.** A unit of work is preconditions and mutations in one
  ordered list, built up front and handed over whole. There is no `begin`, no
  `commit` and no `rollback`, so an unbalanced transaction cannot be written.
  Because an operation has no side effects, losing a race costs a second walk
  rather than an error.
- **`#[derive(Document)]` and `#[derive(Column)]`.** A struct becomes its own
  indexes, its own query builder and its own revision history. `#[unique]`,
  `#[index]` and `#[index(a, b, c)]` are the whole of what it declares about
  indexing; `#[immutable]` and `#[storage(json)]` are what it declares about
  everything else.
- **Revisions and document identity.** An edit supersedes rather than
  overwrites. The document id does not move, so a record held as a foreign key
  keeps pointing at the record rather than at one version of it, and
  `Connection::history` walks back through what came before.
- **`Ref<T>`.** A document id carrying the type it points at, so a field says
  what it references and following one needs no turbofish. `load_many` follows a
  whole set in one scan.
- **`#[immutable]`.** A record that may not be superseded, settled by the type:
  the derive leaves out the impl `update` asks for, so editing one does not
  compile and nothing about it reaches an engine.
- **Two object codecs, chosen per record.** Bytes are CBOR by default;
  `#[storage(json)]` makes them JSON, revision envelope included, so the whole
  object is one JSON value with the record nested under `body`. A record
  carrying bytes cannot ask for JSON and is refused at compile time naming the
  field, because JSON would write them as an array of numbers.
- **A SQLite store you can read.** The engine keeps `doclite_document` for
  objects whose bytes are not text and `doclite_json_document` for those that
  are, so neither `body` holds two storage classes. A JSON record is a text
  value there: `sqlite3` prints the document, and `json_extract` reads a field
  or walks the revision chain without decoding anything first.
- **Transactions.** Several records staged and filed as one operation. A refusal
  names the record that caused it, because each staged write is numbered after
  the last one's abort codes.
- **Two ways to be an engine.** `Engine` is the wide seam, for a backend with a
  query language of its own. `Store` is the narrow one, for a substrate that
  keeps ordered rows, and `doclite-store` supplies what a batch means.
- **`doclite-memory`**, the in-process engine, and **`doclite-sqlite`**, the
  durable one. The SQLite engine owns a connection thread rather than choosing a
  runtime, declares typed columns rather than blobs, answers a `Plan` in SQL
  where it can, and takes a `Durability` naming what survives rather than a
  pragma.
- **`doclite-testkit`.** What makes an engine correct, as a suite any engine
  runs through one macro. It includes a `Probe` that strips every hint before
  forwarding, which is what says a hint only ever saved work.

### Documentation

- Every public item carries a doc comment, fields and enum variants included.
  The workspace denies `missing_docs`, so a new one without a line about it does
  not build. The `Document` derive documents what it generates, so a crate using
  it can deny the same lint.
- `docs/spec.md` is the design, `docs/adr/` is why it is shaped that way, and
  `CONTEXT.md` is the vocabulary. `docs/sqlite.md` and `docs/plan-pushdown.md`
  cover the durable engine. `docs/releasing.md` is the publish checklist.

### Notes

- Requires Rust 1.82 or newer.
- Licensed under Apache-2.0.
