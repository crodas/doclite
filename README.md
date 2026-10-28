# DocLite

An embeddable document store whose transactions are values.

A struct is the schema. `#[derive(Document)]` turns it into its own indexes, its
own query builder, and its own revision history, and that is the whole of what
most callers touch.

```rust
use doclite::{Column, Connection, Document, Record, records};
use doclite_memory::MemoryEngine;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Column)]
struct Email(String);

#[derive(Debug, Clone, Serialize, Deserialize, Document)]
#[index(country, city, state)]
struct User {
    #[unique] email: Email,
    country: Country,
    city: String,
    state: Option<String>,
    #[index] age: u8,
    accepted_tos: bool,
}

let conn = Connection::open(MemoryEngine::new(), records![User]).await?;

let user = conn
    .insert(User { email, country, city, state, age: 24, accepted_tos: true })
    .await?;
```

The indexing attributes are all above. `#[unique]` on a field is an index with
a rule: filing a second record under a taken key is `DocumentError::Duplicate`,
naming the index that refused it. `#[index]` on a field is an index without one.
`#[index(a, b, c)]` on the struct is a compound one. A field with no attribute
is stored and not indexed, which is the common case. The others go on the struct
and say something else entirely: `#[immutable]` below, and `#[storage(json)]`
further down.

## Querying

An index answers prefixes of itself, so the derive writes one method per prefix.
`#[index(country, city, state)]` becomes `by_country`, `by_country_city` and
`by_country_city_state`. The earlier components are equalities and only the last
one takes a range.

```rust
let adults = User::query(&conn).by_age(18..).limit(10).get_many().await?;

let here = User::query(&conn)
    .by_country_city(Paraguay, "Asuncion".into())
    .get_many()
    .await?;

let one = User::query(&conn).by_email(email).get_one().await?;
```

`get_one` refuses rather than choosing when more than one row matches.
`by_age(18)` is an equality and `by_age(18..)` is a comparison, because a bare
value and a Rust range both become bounds. `>` has no range literal and is
`greater_than`, which carries the column type so a value of the wrong type
cannot encode to bytes that quietly match nothing.

## Editing

```rust
let mut user = User::query(&conn).by_email(email).get_one().await?;
user.age = 25;
let user = conn.update(user).await?;
```

An update supersedes rather than overwrites: the new revision names the one it
replaced, and every index moves with it. What does not move is the document id,
so `stored.id()` is the same before and after and another record holding it as a
foreign key keeps pointing at the record rather than at one revision of it.
`stored.revision()` is the one that changes, and `conn.history(&user)` walks
back through the ones before it.

The derive files every record under a generated `<record>_latest` index, keyed
on the document and the revision, holding one row per live record. That is what
makes an edit of a superseded revision `DocumentError::Stale` rather than a lost
write, whatever indexes the struct itself declared.

## Referring to another record

A `Ref<T>` is a document id carrying the type it points at, so a field says what
it references and following one needs no turbofish. On disk it is the id and
nothing else. It is an index column like any other, so the join back is a scan
of one index.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
struct Deal {
    #[unique] reference: Reference,
    #[index] account: Ref<Account>,
    #[index] amount_cents: i64,
}

let open = Deal::query(&conn).by_account(roma.reference()).get_many().await?;

let accounts = conn.load_many(open.iter().map(|deal| deal.account)).await?;
```

Following answers with whichever revision is live at that moment, which is the
whole reason the id does not move when a record is edited. `load_many` follows a
whole set in one scan rather than one read each, because they are all rows in
one ordered index.

## Who a change belongs to

`#[owner]` on a field says who a record's changes are about. Every revision is
then filed in a feed of its own, keyed on the owner, the revision and the
document:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
struct Deal {
    #[unique] reference: Reference,
    #[owner] account: Ref<Account>,
    #[index] amount_cents: i64,
}
```

The owner is a column like any other, so nothing below the document layer
learns the word: in `doclite-core` it is bytes in a key. What makes a feed
different from the indexes above it is that its rows accumulate. An edit moves
every other row and adds one here, so one account's row sits next to the next
one in the order the changes happened, which is what an id minted in time order
buys.

A field can be both, and the pair is the usual one: `#[index]` answers who the
record is about now, `#[owner]` what has happened to it.

A record can belong to several owners at once by holding a `Vec` of them:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
struct Task {
    #[unique] title: String,
    #[owner] watchers: Vec<Ref<User>>,
}
```

That does not widen the key. One revision files one row per owner, so the
change lands in each of their feeds, and a feed is still read for one owner. An
empty list files nothing, which is what a record with no owners means.

Reading one owner's changes is a scan of that index, from wherever the reader
left off:

```rust
let changes = Deal::feed(&conn, account).since(checkpoint).get_many().await?;

let next = changes
    .last()
    .map(|change| Checkpoint::after(change.revision()));
```

What comes back is a `Stored<Deal>` per change, holding the record as it was at
that change rather than as it is now. A `Checkpoint` is a revision rather than a
time, because two revisions can share a millisecond; a `Timestamp` converts into
one, landing at the start of its millisecond, so a reader that kept only the
time replays a few rows rather than missing any.

`watch` is the same read with a tail. The rows the feed already held come first,
in order, and then it waits:

```rust
let mut changes = Deal::feed(&conn, account).watch().await?;

while let Some(change) = changes.next().await {
    match change? {
        Change::Revised(deal) => handle(deal),
        Change::Lagged { from } => log(from),
    }
}
```

A notice is published once the write has committed and never during the walk,
since a losing operation is walked again. The channel behind a watcher is
bounded and is allowed to drop: a reader that stops draining is told it fell
behind once and handed the rest out of the feed, so a slow subscriber cannot
hold up a writer. That is the whole reason the durable scan is the primitive
and being told is the optimisation. Delivery is at least once, and
`Stored::revision` is what to deduplicate on.

## Records that are never edited

A record marked `#[immutable]` has exactly one revision, ever. The derive leaves
out the impl `conn.update` asks for, so editing one does not compile:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
#[immutable]
#[index(account, at)]
struct LedgerEntry {
    account: Ref<Account>,
    at: Timestamp,
    amount: i64,
}
```

Whether a record may be superseded is a fact about the schema rather than about
what the store holds, so it is settled by the type and never reaches an engine.
Filing, querying and following work exactly as they do for any other record.

## Records you can read from the database

A record's bytes are CBOR unless it says otherwise. `#[storage(json)]` makes
them JSON, which costs size and buys a store that can be read without this
crate:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
#[storage(json)]
struct Note {
    #[unique]
    slug: String,
    body: String,
}
```

Under `doclite-sqlite` that reaches all the way down. Such a record lands in
`doclite_json_document` as a text value rather than a blob, so the document
prints as itself and SQLite's own JSON functions read a field out of it without
decoding anything:

```
sqlite> SELECT body FROM doclite_json_document LIMIT 1;
{"v":2,"at":1788563381467,"doc":89497142161416192,"prev":null,
 "body":{"slug":"n-1","body":"the first one"}}

sqlite> SELECT json_extract(body, '$.body.slug') FROM doclite_json_document;
n-1
```

The outer object is the revision envelope, and the record nests under `body` so
that no field it declares can collide with one the envelope writes. History is
queryable there too: `$.prev` is the revision this one superseded, and `$.doc`
is the id that does not move.

The choice is per record, so two records in one database may disagree; a CBOR
one goes to `doclite_document` as a blob. A record carrying a `Vec<u8>` cannot
ask for JSON and is refused at compile time naming the field, because JSON
writes bytes as an array of numbers: it round-trips and is unreadable, which is
the one thing the encoding was chosen for.

## Several records at once

An insert or an edit is one operation, which is the right unit for one record
and the wrong one for a change that is only correct as a whole.

```rust
let mut tx = conn.transaction();
let deal = tx.insert(Deal { reference, account: roma.reference(), amount_cents })?;
let mut account = roma;
account.open_deals += 1;
tx.update(account)?;
tx.commit().await?;
```

A transaction builds exactly the operations `insert` and `update` build and
joins them into one, so the store holds all of them or none. There is still no
`begin` and no `rollback`: the value is complete or it is not handed over, and a
transaction that is dropped instead of committed wrote nothing.

The ids come back before the commit, because they are minted here rather than by
the store. A refused commit means the caller discards them, and it names the
record that refused: each staged write is numbered after the last one's codes,
so a duplicate says which record and which index, not merely that something was
taken.

## Where it is kept

Everything above was written against `MemoryEngine`, which is for a process that
wants no file. `SqliteEngine` is for one that wants its records after the
process is gone, and swapping it in is the whole of the change:

```rust
use doclite_sqlite::SqliteEngine;

let engine = SqliteEngine::open("clinic.db")?;
let conn = Connection::open(engine, records![User]).await?;
```

The file is created if it is not there, and opening it again finds what was
filed. Nothing above this line moves: the same records, the same queries, the
same derive. That is checked rather than claimed: the scenarios in
`crates/doclite/tests` are generic over the engine and every one of them runs
against both.

What a committed write is promised to survive is the caller's, and it is named
for what survives rather than for the pragma underneath:

```rust
use doclite_sqlite::Durability;

let engine = SqliteEngine::open_with("clinic.db", Durability::ProcessCrash)?;
```

`open` means `Durability::PowerLoss`: the engine waits for the disk before
calling a commit done, so the safe answer is the one you get by not choosing.
`ProcessCrash` does not wait, which is faster and means the most recent commits
may not survive the machine losing power. A write-ahead log and a busy timeout
are set either way, because neither is a promise about what survives.

`SqliteEngine::in_memory()` is the third constructor, for a test that wants the
real engine and no file to clean up afterwards.

## Crates

| Crate | What it is |
|---|---|
| `doclite` | The typed layer: records, revisions, and a query builder |
| `doclite-derive` | `#[derive(Document)]` and `#[derive(Column)]` |
| `doclite-core` | Operations, queries, the executor, and the engine trait. It holds objects: bytes under an `ObjectId`, and index rows pointing at them |
| `doclite-store` | The batch protocol, for engines that keep ordered rows |
| `doclite-memory` | The in-process engine |
| `doclite-sqlite` | The durable engine, over SQLite |
| `doclite-testkit` | What makes an engine correct, as a suite any engine runs |

`doclite-core` depends on nothing else in the workspace and knows nothing about
the layer above it. Which engine is under a `Connection` is the caller's choice
and nothing above it changes: `doclite-testkit` runs the same rules against
both.

## The low-level API

Under the derive there is no hidden machinery, only `doclite-core`, and it can
be used on its own. A unit of work is an `Operation`: preconditions and
mutations in one ordered list, built up front and handed to the engine whole.
Because it is a value rather than a closure, the engine can look at it before
running it, and can run it again after losing a race. There is no `begin`, no
`commit`, and no way to construct an unbalanced transaction.

```rust
use doclite_core::{exact, key, AbortCode, Db, ObjectId, Index, Operation, Outcome};
use doclite_memory::MemoryEngine;

static USERS_BY_EMAIL: Index<1> = Index::new("users_by_email");
const DUPLICATE_EMAIL: AbortCode = AbortCode::new(100);

let db = Db::open(MemoryEngine::new(), &[USERS_BY_EMAIL.schema()]).await?;

let id = ObjectId(7);
let op = Operation::new()
    .require_absent(&USERS_BY_EMAIL, exact(key![email]), DUPLICATE_EMAIL)
    .put_object(id, blob)
    .put_index(&USERS_BY_EMAIL, key![email], id)
    .returning(id);

match db.run(&op).await? {
    Outcome::Committed(id) => ...,
    Outcome::Aborted(DUPLICATE_EMAIL) => ...,
    Outcome::Aborted(_) => ...,
}
```

A failed precondition is `Ok(Aborted(code))`, because the operation did its job.
A failed write is `Err`, because it did not. The typed layer inverts the first
into an error, because there the caller asked for the insert.

This is the layer an engine implements against, and it holds bytes rather than
records: it knows nothing about revisions, documents, or the derive.

## How it holds together

An operation reads, then commits, and the two are separate calls on the engine.
What carries consistency across the gap is not a held lock but the assertions
the executor recorded while reading: `apply` re-evaluates every one against
pre-batch state and reports a conflict if any has moved, and the executor
re-runs the operation from the start.

That is also what makes read-your-own-writes work. The executor keeps an
overlay of what the operation has staged, so a precondition placed after a
mutation sees it, while the engine sees nothing until the batch lands.

Everything an engine is told about a scan is a hint. Ignoring one costs time and
never correctness, and `doclite-testkit` proves it by stripping every hint and
demanding the same answers.

There are two ways to be an engine. A store with a query language of its own
implements `Engine` and admits a batch however it likes. A store that keeps
ordered rows implements `Store`, and `doclite-store` supplies what a batch
means: which assertions still hold, what is refused, and the order a write
lands in. `MemoryEngine` is the second kind.

## Examples

```sh
cargo run -p doclite --example quickstart          # one record, filed and changed
cargo run -p doclite --example crm                 # two records, a pipeline, history
cargo run -p doclite --example feed                # one owner's changes, caught up and watched
cargo run -p doclite-memory --example operations   # the storage layer, no derive
```

## Reading order

- `docs/blog/` is the guided tour, numbered in reading order.
- `docs/spec.md` is the design this implements.
- `docs/adr/` is why it is shaped this way.
- `crates/doclite-testkit/src/programs.rs` is the spec's worked programs,
  running against every engine.
- [docs/roadmap.md](docs/roadmap.md) is what is left before the first release,
  what the one after it is for, and what is absent on purpose.

## Development

```sh
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
```

The workspace denies `missing_docs`, set once in the root manifest, so a public
item with no doc comment fails the build rather than the review. The `Document`
derive documents the constants and `by_` methods it generates, so a crate using
it can deny the lint too.

`doclite-core` has three features and none is on by default. `serde` derives
`Serialize` on the operation representation; `cbor` and `json` are the two
document codecs, behind `serde_storage!` and `json_storage!`. Running the suite
with features off is what proves the byte API still stands on its own.

The workspace builds on Rust 1.82 and up, which is what every manifest declares
as its `rust-version`. CI runs all of the above plus `cargo fmt --all --check`
and the 1.82 check, on Linux and macOS. `CHANGELOG.md` is what changed and
`docs/releasing.md` is the publish checklist.

## License

Apache-2.0. The text is in [LICENSE](LICENSE).
