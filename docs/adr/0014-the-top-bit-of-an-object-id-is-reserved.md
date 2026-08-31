# ADR 0014: The Top Bit of an Object Id Is Reserved

## Status
Accepted

## Context
`ObjectId` is a `u64` and ADR 0007 puts minting in the caller's hands, so any of
the sixty-four bits could in principle be set. SQLite's integers are signed
sixty-four, and most of the backends worth writing next are the same: Postgres
`bigint`, and the key encodings of the embedded stores.

That mismatch cost the SQLite engine more than it first appeared. An id at or
above 2^63 stored as `INTEGER` bit-casts negative and sorts before every smaller
one, which breaks the "then object id" half of index order, and ADR 0004 makes
row order the one thing about a read that is not a hint. The engine took the
safe option and held every id as eight big-endian bytes.

The cost was not correctness but legibility, and it was larger than the object
table. `ObjectId::COLUMN_TYPE` was `U64`, and `docs/sqlite.md` maps `U64` to
`BLOB`, so every column holding an id became a blob: every `Ref<T>`, and the
document and revision columns of every generated `<record>_latest` index. A
database whose foreign keys are all blobs cannot be read in `sqlite3`, which is
most of the reason to have picked SQLite.

Nothing was using the range. The only minter in the workspace is `Snowflake`,
whose layout is 41 bits of milliseconds, 10 of node and 12 of sequence. That is
sixty-three, and `the_sign_bit_stays_clear` has always asserted it. The bit was
already free; `doclite-core` had simply never promised it, so no engine could
lean on it.

## Decision
An `ObjectId` is sixty-three bits. `ObjectId::LIMIT` is the first value that is
not one, and `Check` refuses any operation naming an id at or above it before
that operation reaches an engine, as `BuildError::ObjectIdTooLarge`.

`ObjectId` declares `ColumnType::I64` and encodes through `i64`. The declared
type and the bytes have to agree, because an engine decodes a column according
to what the schema said it was. Flipping the sign bit of a non-negative value
preserves exactly the ordering the unsigned encoding gave, so ids still sort by
creation and the revision chain still reads in the order things happened.

The SQLite engine stores an id as `INTEGER`: the object table's primary key, the
object column of every index table, and every key column that holds one.

## Consequences
A database is legible. `SELECT * FROM doclite_idx_deal_by_account` shows numbers
where it used to show blobs, and a foreign key can be followed by eye.

The refusal is at the boundary rather than in an engine, because the promise is
the type's and every backend is allowed to rely on it. A backend whose integers
are unsigned loses nothing: sixty-three bits fit anywhere sixty-four did.

A caller who wanted the full range cannot have it. This is the trade, and it is
worth naming: an id is now a number the store constrains rather than an opaque
handle. Sixty-three bits is 9.2 quintillion, and the one minter here reaches
2090 before it runs out of milliseconds, so the constraint costs nothing that
was going to be spent.

One place had to change rather than fall out. A sweep writing the scanned row's
object into a key column copies the object column straight across, which lands
the right value only where the target column is the integer that column now is.
Into anything else the engine declines the sweep, and the executor expands the
template row by row, which is what declining a hint has always cost.
