# What the type settles

Most rules in a database are checked when a write runs. A unique key is claimed
or refused; a revision is current or stale. Some of DocLite's rules are not like
that. They are decided by the type, which means they are decided before the
program runs, and none of them reaches an engine or has an abort code.

Samples are lifted from `crates/doclite/tests/immutable.rs` and the doctests on
`crates/doclite/src/record.rs` and `crates/doclite/src/lib.rs`, which pass.

## A record that is never edited

A ledger entry, an audit line, a posted invoice: records that exist precisely
because nobody may go back and adjust them. The usual way to express that is a
trigger, a permission, or a convention nobody enforces.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
#[immutable]
struct Entry {
    #[unique]
    slug: Slug,

    #[index]
    account: u8,

    amount: i64,
}
```

`#[immutable]` on the struct is the whole declaration. What it changes is not
what gets written but what gets generated: `#[derive(Document)]` writes an `impl
Amendable` for a record unless the struct says this, and `Amendable` is what
`Connection::update` asks for.

So the refusal is a compile error:

```rust
let mut entry = conn.insert(entry("e-1", 4, 100)).await?;
entry.amount = 250;
conn.update(entry).await?;   // does not compile
```

There is nothing to catch, nothing to match on, and no `DocumentError` variant
for it. The trait bound is not satisfied and the program does not build. This is
checked rather than described: the doctest on `Amendable` is marked
`compile_fail`, so if the derive ever started writing that impl unconditionally,
the test suite fails.

`Amendable` is deliberately not a subtrait of `Record`. What it gates is
`DerefMut` on `Stored`, and a `Stored` holds any value the store can hold, so
tying the two together would be tying the wrong knot.

## Everything else about it is ordinary

Worth saying plainly, because "immutable" invites the assumption that such a
record is stored differently. It is not.

```rust
let filed = conn.insert(entry("e-1", 4, 100)).await?;

let found = Entry::query(&conn).by_slug(Slug("e-1".into())).get_one().await?;
assert_eq!(found.id(), filed.id());

let by_account = Entry::query(&conn).by_account(4..=4).get_many().await?;
assert_eq!(by_account.len(), 1);
```

It claims its unique keys the same way, and a second record taking `e-1` is
`DocumentError::Duplicate { index: "entry_by_slug" }` exactly as it would be for
any other struct. Immutability says a record has one revision; it says nothing
about who may hold a key.

It is filed under the same generated `<record>_latest` index as everything else,
with the same walk:

```rust
assert_eq!(
    walk.0,
    [
        ("entry_latest", false),
        ("entry_by_slug", true),
        ("entry_by_account", false),
    ],
);
```

And its history is a chain of one:

```rust
assert_eq!(filed.prev(), None);
assert_eq!(filed.id().0, filed.revision().0);   // the first revision is the record
assert!(conn.history(&filed).await?.is_empty());
```

Two structs differing only by the attribute declare the same indexes and walk
the same rows. The attribute changes which impls the derive writes, and nothing
a record declares.

## An encoding a record asks for

The bytes a record is stored as are settled the same way. They are CBOR unless
the struct says otherwise:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
#[storage(json)]
struct Note {
    #[unique]
    slug: String,
    body: String,
}
```

What that buys is a store something other than this crate can read. Under the
SQLite engine the object is a text value in a table of its own, so `sqlite3`
prints the document and `json_extract` reads a field out of it or walks the
revision chain in plain SQL. It costs size, which is the whole of the trade.

This one carries a refusal as well as a choice. JSON writes a `Vec<u8>` as an
array of numbers, which round-trips correctly and reads as nothing, so a record
carrying a byte field cannot ask for JSON and the derive says so by name:

```
error: `image` cannot be stored as JSON; bytes would encode as an array of
       numbers. Drop storage(json) to store this record as CBOR
```

Like the attribute above it, none of this reaches an engine. An engine sees
bytes and asks only whether they are text, so two records in one database may
disagree about their encoding and nothing below the document layer is told.

## A reference that knows what it points at

The last thing the type settles is what a foreign key means.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
struct Deal {
    #[index]
    account: Ref<Account>,
    #[index]
    amount_cents: i64,
}
```

`Ref<Account>` is a `DocumentId` and a `PhantomData<fn() -> Account>`. On disk
it is the id and nothing else, because what it points at is a fact about the
schema rather than about the row. In the program it is the difference between

```rust
let account = conn.load(deal.account).await?;
```

and having to name the type at the call site every time you follow one. The
field already says what it references, so the read does not have to be told
again.

The `PhantomData` is `fn() -> T` rather than `T` on purpose: it makes `Ref<T>`
covariant in `T` and, more usefully, keeps `Ref` from inheriting bounds it has
no business having. The manual `Clone` and `Copy` impls exist for the same
reason. A derived `Clone` would demand `T: Clone`, which is wrong: copying an id
has nothing to do with whether the record it names can be cloned.

Following one answers with whichever revision is live at that moment, which is
the whole reason a document id does not move when a record is edited. [The
struct is the index](02-the-struct-is-the-index.md) covers the join, and why
`load_many` is one scan rather than one read per reference.

## Why this is worth doing at all

Every one of these could have been a runtime check. Immutability could be a flag
on the schema that `update` consults, the encoding could be a column beside the
bytes, and a reference could be a bare `DocumentId` with the target type passed
at each call.

The argument against is the same every time and it is not about performance.
A rule the store enforces is a rule you find out about from a running program,
usually the one in production, and it needs an error variant, a test for that
variant, and a reader who remembers the variant exists. A rule the type enforces
is a build that fails on the machine of whoever wrote the mistake, needs no
error variant, and cannot be forgotten because the compiler does the
remembering.

The cost is that none of it is negotiable at runtime. You cannot decide at
startup that this deployment allows editing ledger entries. That is the correct
trade for a fact about the schema, and the wrong trade for anything that
genuinely varies, which is why uniqueness and staleness are still preconditions
the store judges.

Next: [where it is kept](06-where-it-is-kept.md), which is the first question in
this tour whose answer is a choice.
