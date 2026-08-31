# A SQLite Engine

What a durable engine backed by SQLite looks like, and why each choice is the
one that keeps the contract in `crates/doclite-core/src/engine/mod.rs`.

`crates/doclite-sqlite` is this, built. Where the two disagree the code is
wrong, because this is what it was written against.

## It implements `Engine`, not `Store`

ADR 0011 already routes SQLite to the wide seam, on the grounds that a backend
with its own query language should decide for itself how a batch lands. There is
a second reason, and it is not a preference.

`Region`'s writes are infallible and the trait has no commit method.
`StoreEngine::apply` acquires a region, judges the batch, writes it, returns
`Ok(Applied::Committed)`, and lets the region drop. A SQLite `COMMIT` can fail:
the disk fills, the WAL cannot be checkpointed, the file is gone. Its only home
in that shape is `Drop`, where the error cannot be returned and success has
already been claimed.

The narrow seam cannot host a durable engine without lying about durability. A
store whose writes really cannot fail is what `Region` is for, and SQLite is not
one.

The cost is that the batch protocol has to be written again in SQL.
`crates/doclite-store/src/admit.rs` is that protocol in sixty readable lines,
and it is a private module taking `impl Region`, so it cannot be called from
another crate. Read it, reproduce its order, do not try to reuse it.

## The connection thread

`rusqlite::Connection` is `Send` and not `Sync`. `Engine` takes `&self` and
returns a `Send` future. A connection therefore cannot simply live in the engine
behind a shared reference.

ADR 0006 settles the shape of the answer before the question is asked:
`doclite-core` depends on no runtime, "so which one a caller uses is not this
crate's business". That rules out `spawn_blocking`, which chooses tokio, and it
rules out an async driver whose runtime is a feature flag.

The connection lives on one thread the engine owns. Each method sends a job down
a channel and awaits a reply on a oneshot that belongs to no runtime. The thread
is the only thing that ever touches the connection, so `Send + Sync` is
satisfied by the channel rather than by a lock, and no guard is ever held across
an await.

State the cost rather than hiding it: reads serialize behind writes. SQLite in
WAL mode allows concurrent readers on separate connections, and a pool of read
connections alongside the single writer is the obvious later change. It is not
the first version, because one thread is the version whose correctness is
obvious.

A lock poisoned by a panic is `StorageError::Poisoned`, not
`StorageError::Engine`. If the connection thread dies, every subsequent call
fails with `Engine` carrying what is known.

## The schema

### Objects

```sql
CREATE TABLE IF NOT EXISTS doclite_document (
  id    INTEGER PRIMARY KEY,
  body  BLOB NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS doclite_json_document (
  id    INTEGER PRIMARY KEY,
  body  TEXT NOT NULL
) WITHOUT ROWID;
```

Insert only. No `UPDATE`, no `DELETE`, and above all no `INSERT OR REPLACE`. ADR
0001 makes objects append-only, and a primary-key conflict is the mechanism by
which that rule is enforced within one table: catch it and return
`StorageError::DuplicateObject(id)`. An upsert here would turn a caught bug into
silent data loss.

Nothing ever frees an object. ADR 0001 says so and means it, so there is no
vacuum path and no tombstone.

**Two tables, split by what the bytes are.** Bytes that are valid UTF-8 go to
`doclite_json_document` as a text value and everything else to
`doclite_document` as a blob. One column holding two storage classes is what the
column-type section below forbids, and objects are not made an exception to it;
the split is what keeps each `body` one kind.

The engine is never told which codec a record chose. "Is this UTF-8" is the only
question it can ask, and it is enough: `#[storage(json)]` is the only thing that
produces a text body today, and a byte-level caller storing text gets the same
treatment for the same reason.

That is what makes a JSON record legible. A blob prints as hex and
`json_extract` refuses one, where a text value is the document itself:

```
sqlite> SELECT body FROM doclite_json_document LIMIT 1;
{"v":2,"at":1788563381467,"doc":89497142161416192,"prev":null,
 "body":{"email":"a@b.c","city":"Asuncion","age":24}}

sqlite> SELECT json_extract(body, '$.body.email') FROM doclite_json_document;
a@b.c
```

The outer object is the revision envelope, which `doclite` writes as JSON when
the record is JSON: a binary header in front would leave the object neither one
thing nor the other. The record nests under `body` rather than flattening beside
the envelope's own fields, so nothing a record declares can collide with them.
The chain is queryable in place, which is the point:

```sql
SELECT json_extract(body, '$.doc') AS document, COUNT(*) AS revisions
  FROM doclite_json_document
 GROUP BY 1;
```

**Every read spans both tables.** An id names one object wherever its bytes went,
so `object`, `objects` and `has_object` are each a `UNION ALL` over the two. A
per-table primary key cannot see across them, which is why `apply` asks
`has_object` before it files anything rather than leaning on the conflict alone.

Reading a body is `ValueRef::as_bytes`, which takes text and blob alike, and not
a `Vec<u8>` column read: rusqlite's `FromSql for Vec<u8>` goes through `as_blob`
and would refuse everything in the JSON table.

### Indexes

One table per index, named from the incoming `IndexId` and created by `define`:

```sql
CREATE TABLE IF NOT EXISTS doclite_idx_users_by_email (
  c0      BLOB NOT NULL,
  object  INTEGER NOT NULL,
  PRIMARY KEY (c0, object)
) WITHOUT ROWID;
```

Three decisions are in that statement.

**One table per index**, because arity varies between indexes and a shared table
would need either a widest-case row or a concatenated key, and the next decision
rules the second out. Per-index tables also make ADR 0001's claim literal: an
index can be rebuilt from the objects it names, so losing one is recoverable,
and `DROP TABLE` is what that looks like.

**One SQL column per key column.** This is forced, not chosen. A key is
`Vec<Vec<u8>>` and its order is elementwise, and elementwise comparison is not
comparison of the concatenation:

| keys | elementwise | concatenated |
|---|---|---|
| `["a","z"]` vs `["ab","a"]` | `"a" < "ab"`, so first is smaller | `"az" > "aba"`, so first is larger |
| `["a","b"]` vs `["ab",""]` | different keys | both are `"ab"` |

The first inverts index order, which is the one read guarantee that is not a
hint. The second makes two distinct keys indistinguishable, so `Exact` matches
the wrong row and `RowKey::component` cannot be recovered. ADR 0002 says a
compound key is "one element per column, never a concatenation" and this is the
failure it is naming.

**No `UNIQUE` constraint, ever.** ADR 0001: an index carries no constraint, and
whether at most one object may hold a key is a rule asserted by an operation.
Two rows may legitimately share a key, and the conformance case
`a_scan_answers_in_index_order` files two rows under one key on purpose. Row
identity is `(columns…, object)`, which is why that is the primary key.

`WITHOUT ROWID` because the primary key is the whole row and a rowid would be a
second identity nothing uses.

## The object id

Store it as an `INTEGER`, and rely on ADR 0014 to make that safe.

SQLite's `INTEGER` is signed and 64 bits wide, and an id at or above 2^63 stored
as one bit-casts to a negative number and sorts *first*, which would break the
"then object id" half of index order. ADR 0014 removes the hazard rather than
working around it: an `ObjectId` is sixty-three bits, `Check` refuses anything
larger before it reaches an engine, and every id this engine can be handed is
therefore a non-negative integer.

This is why `ObjectId` declares `ColumnType::I64` rather than `U64`, which is
also what makes every key column holding an id an `INTEGER` without this engine
doing anything: a `Ref<T>`, and the document and revision columns of every
generated `<record>_latest` index. Storing them as blobs was correct and made
the database unreadable in `sqlite3`, which is most of the reason to have picked
SQLite.

## Column types

`ColumnType` exists so "a store with a type system of its own can declare a real
column instead of a blob", and a SQLite engine is the first backend that can. It
is also the first that can get it wrong, so each mapping owes two proofs:

1. SQLite's ordering for that storage class equals the `memcmp` order of the
bytes `ToOrdBytes` produced. 2. The original bytes come back byte-identical,
because `Observed::Rows` compares them for equality and `KeyRange::select`
re-filters them.

| `ColumnType` | SQL | Why |
|---|---|---|
| `Bool`, `Char` | `INTEGER` | fixed width, value fits, order agrees |
| `U8`, `U16`, `U32` | `INTEGER` | fits in `i64` unsigned, order agrees |
| `I8`, `I16`, `I32`, `I64` | `INTEGER` | fits, and SQLite's signed order is the order the sign-flipped bytes encode |
| `U64`, `U128`, `I128` | `BLOB` | does not fit a signed 64-bit integer |
| `Str` | `TEXT` | see below |
| `Bytes` | `BLOB` | uninterpretable by definition |
| `Nullable(_)` | as the inner type, still `NOT NULL` | see below |

Three of these need the reasoning written out, because they are where a careful
implementer goes wrong.

**`Nullable` never becomes SQL `NULL`.** `Option` already encodes as a tagged
blob: `[0]` for `None`, `[1]` followed by the body for `Some`. Mapping it to
`NULL` would lose the tag and flip the order, because SQLite sorts `NULL` before
every other storage class. For the same reason a zero-length component is an
empty blob and not `NULL`. Every key column is `NOT NULL`.

**`Str` is the judgement call.** `TEXT` under the default `BINARY` collation
compares with `memcmp` over the encoded bytes, which is exactly the order
`String::to_ord_bytes` produces, and it round-trips because the bytes came from
a Rust `String` and are valid UTF-8. It is also one `COLLATE NOCASE` away from
silent misordering, and it cannot hold the non-UTF-8 bytes a `Bytes` column can.
The declaration is safe as long as nothing ever adds a collation to it, and it
is worth having because `Str` is the column a person reading the database by
hand most wants to see. Declare it `TEXT` and write a comment in the DDL saying
why it must stay `BINARY`.

**Never mix storage classes in one column.** SQLite orders `NULL` before
`INTEGER` and `REAL`, those before `TEXT`, and `TEXT` before `BLOB`
(<https://sqlite.org/datatype3.html#sort_order>). A column that holds an integer
for one row and a blob for another is not ordered by the rule this design needs.
A declared column type is an affinity and not a constraint in SQLite, so the
engine must guarantee this itself: decode the incoming bytes according to the
declared `ColumnType`, and if they are the wrong width for it, that is
`StorageError::Engine` rather than a silently different storage class.

That last obligation is the real price of typed columns, and it is the reason a
first implementation may reasonably declare every column `BLOB` and come back to
this table later. Both are conformant. Only one is legible from `sqlite3`, and
ids are the columns where that reads worst, which is what ADR 0014 is about.

## `define`

The two object tables and a `CREATE TABLE IF NOT EXISTS` per index, all in one
transaction.

It is called on every `Db::open` with the whole index list, so it must tolerate
repetition. It must not create a `UNIQUE` index, and it must not drop or alter a
table it did not expect: an index whose arity has changed is a migration, not a
definition, and silently rewriting it would lose rows.

An engine keeps its own map from `IndexId` to table name. It cannot rebuild an
`IndexId` from the database, because `IndexId` is a `&'static str` and no string
read at runtime can become one. Names come from `define`; the database is only
asked whether a table exists.

## `read`

One deferred transaction spans the whole call, so every scan and every object in
one `Reads` answers from one view. That is what `Query::zip` rests on.

`Answers` is positional and exact: one row set per scan and one slot per object
id, in the order asked. A missing object is `None` in its slot, never an
omission. A miscount is reported back as `StorageError::MissingAnswers`, so the
engine gets no benefit from guessing.

Rows come back in index order: columns, then object id. This is the one thing
about a read that is not a hint.

```sql
SELECT c0, c1, object FROM doclite_idx_x WHERE ... ORDER BY c0, c1, object LIMIT ?;
```

A scan of an index never declared is `StorageError::UnknownIndex(id)`, named
rather than guessed at.

### `KeyRange` as a `WHERE` clause

| variant | meaning | SQL |
|---|---|---|
| `All` | everything | omit the predicate |
| `Exact(k)` | full-key equality, `k.len() == arity` | `c0 = ? AND … AND cn = ?` |
| `Prefix(k)`, `k.len() == K` | first `K` columns equal | `c0 = ? AND … AND c(K-1) = ?` |
| `Between { lo: Some((l, b)), .. }` | comparison over the `l.len()` columns it names | `(c0,…) >= (?,…)`, or `>` for `Excl` |
| `Between { hi: Some((h, b)), .. }` | the same, other side | `<=`, or `<` for `Excl` |
| `Between { lo: None, hi: None }` | everything | omit |

Row-value comparison needs SQLite 3.15 (<https://sqlite.org/rowvalue.html>),
which is the minimum version this engine declares.

Three things bite:

- `lo` and `hi` may name different numbers of columns. There is no single width
to compute once.
- A partial bound selects whole groups, because `KeyRange::compare` truncates
both sides to the shorter. A row-value comparison over the columns the bound
names reproduces that exactly, which is why it is the right construct rather
than a convenience.
- `Prefix` is a column prefix, never a byte prefix. `Prefix(["a"])` does not
match a row whose first column is `"ab"`.

### The range is a hint here and nowhere else

On `read`, answering with more rows than the range selects is legal: the
executor filters with `KeyRange::select`, and ADR 0004 makes that the whole read
contract.

Inside `apply` it is not legal. `Observed::holds` compares counts and row sets,
so a superset there turns a satisfied assertion into a failed one, the batch is
refused as a conflict, and the operation retries until it exhausts and raises
`StorageError::Conflict`. Nothing points at the engine.

This asymmetry between two methods of one trait is the most expensive thing in
this document to get wrong. Filter exactly in `apply`.

## `apply`

`BEGIN IMMEDIATE`, then the protocol, then `COMMIT`. The immediate mode is right
because the method split has already decided it: an operation with nothing
staged never reaches `apply`, so this is the only method that writes.

The order is the contract, and it is not the order of `Batch`'s fields:

1. **Every assertion, against pre-batch state.** The first that does not hold
ends the batch as `Ok(Applied::Conflict)`. Nothing has been written, so there is
nothing to undo. 2. **Each object id in `batch.objects`** that is already
present is `Err(StorageError::DuplicateObject(id))`. 3. **Each `RangeDelete`
index** that was never defined is `Err(StorageError::UnknownIndex)`. 4. **Each
`IndexEntry`**: an undefined index is `UnknownIndex`, and `columns.len() !=
arity` is `Err(StorageError::Arity { index, expected, found })`. 5. Then write:
**deletes, then objects, then index rows.**

A `RangeDelete` removes every row of the index whose key falls in the range,
whatever object it names. Unlike a read range this one is exact, and answering
with more is data loss.

Filing a row that is already there is not an error. The reference keeps rows in
a set, so `INSERT OR IGNORE` on the primary key matches it. Deciding otherwise
means a duplicate row appears twice in `Answers.scans` and in an
`Observed::Rows` comparison.

### Assertions

Call `Observed::holds(limit, rows)`. Do not reimplement the comparison in SQL.

It applies the limit *inside* itself, taking the first `limit` rows of what it
is given, so it must be handed the whole range's rows rather than a
pre-truncated set. An engine that instead re-runs the range with its own `LIMIT`
refuses batches that should land, which is what
`a_limited_assertion_judges_what_was_read` exists to catch.

`Judging` is why that is hard to get wrong here rather than only documented: it
has `every_row` and no way to ask for fewer, so the read that judges an
assertion cannot be handed a limit at all.

`Absent` and `Count` could be answered by `SELECT EXISTS` and `SELECT COUNT(*)`
without materializing rows, and that is a legitimate optimisation once the plain
version passes. `Rows` needs the row set in Rust either way, because the
comparison is byte-exact.

An assertion naming an index that was never defined is `UnknownIndex`, not a
conflict. The reference checks arity before it checks the observation, and the
order matters.

### Refusal is not failure

A failed assertion is `Ok(Applied::Conflict)`. The executor discards its delta
and walks the operation again, up to sixteen times. Returning `Err` there turns
a retryable race into a hard failure.

`DuplicateObject` is an `Err`, because walking again cannot fix it.

`SQLITE_BUSY` is neither. Retry it inside the engine against the `busy_timeout`,
and surface what is left as `StorageError::Engine`. Reporting it as
`Applied::Conflict` would make the executor re-walk an operation that never had
a conflict, sixteen times, and then report one.

## Planning

Everything above describes an engine that answers the flat `Reads` it is given.
`docs/plan-pushdown.md` specifies an optional seam that hands the engine the
`ReadPlan` instead, so a count becomes `SELECT COUNT(*)` rather than a million
rows counted in Rust, a zip becomes one statement, and a `for_each` becomes
`INSERT INTO ... SELECT` inside this transaction.

That is where the value of using SQL at all mostly lives, and it is specified
separately because it is a change to `doclite-core` rather than to this engine.
An engine may ignore it entirely and still be correct.

## Opening

Three constructors: one in memory, one over a path, and one over a path that
says what it is willing to lose.

`journal_mode = WAL` for a file, because readers and the writer stop blocking
each other (<https://sqlite.org/wal.html>).

Durability is the caller's, and is named for what survives rather than for the
pragma, because that is the question they are answering. `Durability::PowerLoss`
waits for the disk before calling a commit done; `Durability::ProcessCrash` does
not, so the most recent commits may not survive the machine dying. `open` means
`PowerLoss`, so the safe answer is the one you get by not choosing, and
`open_with` is where the other one lives. An in-memory database reaches no disk,
so the question does not arise there and the pragma does nothing.

`busy_timeout` stays fixed and non-zero, so a contended write waits instead of
failing. It is not a promise about what survives, only about how long a writer
is willing to queue, so it is not the caller's to choose. `foreign_keys` is
irrelevant here because nothing declares one.

## Testing

`doclite-testkit` is the definition of a correct engine, and the whole suite is
one file:

```rust
async fn fresh() -> Result<SqliteEngine, StorageError> {
    SqliteEngine::open_in_memory()
}

doclite_testkit::conformance!(
    #[tokio::test]
    fresh
);
```

`fresh` is called twelve times and `hints_are_hints` holds two engines at once,
so each call needs its own database. In memory gives that for nothing.

Two gaps the suite does not cover, which this engine must close itself.

**The file path.** Conformance in memory exercises no WAL, no `busy_timeout`,
and no fsync. Add a small suite over a temporary file: open, write, close,
reopen, and read back.

**Typed columns.** The testkit's two indexes, `testkit_by_email` and
`testkit_by_pair`, are declared entirely with `ColumnType::Bytes`. Every mapping
in the table above is therefore invisible to the suite. Add a table-driven test
that, for each `ColumnType`, files rows spanning the interesting boundaries
(negative and positive, empty and non-empty, `None` and `Some`), scans them
back, and asserts both the order and the byte-exact round trip. Without it the
typed mapping is unverified by construction, and its failure mode is silent
misordering.
