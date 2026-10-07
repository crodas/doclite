# Changelog

Notable changes to every crate in the workspace. They share one version and are
released together, so one file covers all seven.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
the versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0 a minor bump is where a breaking change goes.

## Unreleased

### Added

- **An engine says which of its indexes have rows missing.** A new or reshaped
  index used to get an empty table, so a query through it answered
  correctly-shaped nothing. `Engine::define` now answers a `Defined` naming the
  indexes it has no complete set of rows for, `Engine::rebuilt` records that one
  is complete, and `doclite-sqlite` keeps the shape each index table was built
  for in `doclite_index`.

### Changed

- **`Engine::define` and `Store::define` answer a `Defined` rather than
  nothing.** Breaking for anyone implementing either seam. A backend that keeps
  nothing across a restart answers `Defined::ready`.
- **An index whose shape moved is dropped rather than refused.** `docs/sqlite.md`
  said an arity change was a migration and not a definition. ADR 0015 reverses
  that, and ADR 0001 is why it is safe. An existing database is not rebuilt on
  first open: an index table with no recorded shape is taken as complete.

## 0.1.0 - 2026-09-05

First release, so there is no upgrade path to describe. The date is the day the
tree was made ready; move it if the upload slips, because a changelog dated
before what it describes is worse than one with no date at all.

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
